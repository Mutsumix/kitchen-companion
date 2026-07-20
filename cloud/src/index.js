// kitchen-companion 中継Worker
// デバイスからWAVを受け取り、STT→LLM→TTSを中継して16kHz PCMを返す。
// APIキーは secret (OPENAI_API_KEY) にのみ存在し、デバイスには一切置かない。

const COMMON_RULES = `あなたは台所に置かれた小さなAI料理相棒です。日本語の話し言葉で答えます。
- 回答は音声で読み上げられる。3〜6文・読み上げ30秒以内を目安に、要点を絞って話す
- 加熱の要否や生食の可否など食品安全に関わることは断言せず、「公的な情報の確認」を勧める
- 料理と無関係な発話には一言だけ軽く応じる`;

// モード別システムプロンプト(デバイスの X-Mode ヘッダで切り替え)
const MODE_PROMPTS = {
  consult: `${COMMON_RULES}
【いまのモード: レシピ相談】
- 料理の相談には、具体的な料理名を2〜3個、それぞれ一言の理由や作り方のポイント付きで提案する
- 最後に短い質問や次の一手(「他に何がある?」「作り方いる?」など)を添えて会話を続けやすくする`,
  shopping: `${COMMON_RULES}
【いまのモード: 買い出しリスト作り】
- ユーザーが挙げる冷蔵庫や棚の在庫を会話の中で覚えていく
- 作りたい料理や日数を聞き、在庫と照らして「買うべきものリスト」を組み立てる
- 発話のたびに、現時点のリストを短く復唱して確認する(「いまのリスト: 玉ねぎ、豚肉。他には?」のように)`,
  cooking: `${COMMON_RULES}
【いまのモード: 調理ガイド】
- いま作っている料理の手順を、1回の発話につき1ステップだけ、短く具体的に指示する
- 「次は?」「終わった」と言われたら次のステップへ進む
- 火加減・時間・切り方など、そのステップで大事なポイントを一言添える
- 危険な工程(揚げ物・熱湯など)では安全への注意を必ず一言入れる`,
};

// 24kHz(OpenAI TTSのPCM出力) → 16kHz(デバイスのI2S設定)への線形補間リサンプル。
// 末尾に300msの無音を付ける: デバイスのI2S送信DMAはデータが尽きると最後の
// バッファを繰り返すため、無音で終わらせないと音が鳴りっぱなしになる
const TAIL_SILENCE_SAMPLES = 4800; // 300ms @16kHz

function resample24kTo16k(pcm24) {
  const outLen = Math.floor((pcm24.length * 2) / 3);
  const out = new Int16Array(outLen + TAIL_SILENCE_SAMPLES); // 末尾はゼロ初期化=無音
  for (let i = 0; i < outLen; i++) {
    const src = i * 1.5;
    const i0 = Math.floor(src);
    const frac = src - i0;
    const a = pcm24[i0] ?? 0;
    const b = pcm24[i0 + 1] ?? a;
    out[i] = a + (b - a) * frac;
  }
  return out;
}

async function openai(path, env, init) {
  const res = await fetch(`https://api.openai.com/v1/${path}`, {
    ...init,
    headers: {
      Authorization: `Bearer ${env.OPENAI_API_KEY}`,
      ...(init.headers ?? {}),
    },
  });
  if (!res.ok) {
    const body = await res.text();
    throw new Error(`${path} failed: ${res.status} ${body.slice(0, 300)}`);
  }
  return res;
}

export default {
  async fetch(request, env) {
    if (request.method !== "POST") {
      return new Response("kitchen-companion relay: POST audio/wav to /talk", { status: 200 });
    }
    const t0 = Date.now();
    try {
      const wav = await request.arrayBuffer();

      // 1. STT (Whisper)
      const fd = new FormData();
      fd.append("file", new Blob([wav], { type: "audio/wav" }), "speech.wav");
      fd.append("model", "whisper-1");
      fd.append("language", "ja");
      const stt = await openai("audio/transcriptions", env, { method: "POST", body: fd });
      const transcript = (await stt.json()).text ?? "";
      const tStt = Date.now();

      // 2. LLM(KVに保存した会話履歴を文脈として渡す。30分でセッション失効)
      const HISTORY_KEY = "session"; // 単一デバイス前提。複数台化したらデバイスIDをキーにする
      const MAX_TURNS = 6; // 直近6往復だけ渡す(トークン節約)
      const mode = request.headers.get("X-Mode") ?? "consult";
      const systemPrompt = MODE_PROMPTS[mode] ?? MODE_PROMPTS.consult;
      // 「新規」ボタンで仕切り直しが指示されたら履歴を消してから始める
      if (request.headers.get("X-New-Session") === "1") {
        await env.HISTORY.delete(HISTORY_KEY);
      }
      const history = (await env.HISTORY.get(HISTORY_KEY, "json")) ?? [];
      const chat = await openai("chat/completions", env, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          model: "gpt-4o-mini",
          messages: [
            { role: "system", content: systemPrompt },
            ...history,
            { role: "user", content: transcript },
          ],
          max_tokens: 400,
        }),
      });
      const reply = (await chat.json()).choices[0].message.content.trim();
      const tLlm = Date.now();

      // 履歴を追記保存(直近MAX_TURNS往復のみ、TTL30分のスライド式)
      const newHistory = [
        ...history,
        { role: "user", content: transcript },
        { role: "assistant", content: reply },
      ].slice(-MAX_TURNS * 2);
      await env.HISTORY.put(HISTORY_KEY, JSON.stringify(newHistory), { expirationTtl: 1800 });

      // 3. TTS (pcm = 24kHz/16bit/モノラル) → 16kHzへリサンプル
      const tts = await openai("audio/speech", env, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          model: "tts-1",
          voice: "nova",
          input: reply,
          response_format: "pcm",
        }),
      });
      const pcm24 = new Int16Array(await tts.arrayBuffer());
      const pcm16 = resample24kTo16k(pcm24);
      const tTts = Date.now();

      return new Response(pcm16.buffer, {
        headers: {
          "Content-Type": "application/octet-stream",
          "X-Transcript": encodeURIComponent(transcript),
          "X-Reply": encodeURIComponent(reply),
          "X-Timing": `stt=${tStt - t0}ms llm=${tLlm - tStt}ms tts=${tTts - tLlm}ms total=${tTts - t0}ms`,
        },
      });
    } catch (e) {
      return new Response(`relay error: ${e.message}`, { status: 502 });
    }
  },
};
