// kitchen-companion 中継Worker
// デバイスからWAVを受け取り、STT→LLM→TTSを中継して16kHz PCMを返す。
// APIキーは secret (OPENAI_API_KEY) にのみ存在し、デバイスには一切置かない。

// 使用モデル(レイテンシ比較しやすいようここで一元管理)
// STT: whisper-1 → gpt-4o-mini-transcribe に変更(高速化。認識精度は実用で確認済み)
// TTS: gpt-4o-mini-tts を試したが日本語の音質が悪化したため tts-1 に戻した
const STT_MODEL = "gpt-4o-mini-transcribe";
const LLM_MODEL = "gpt-4o-mini";
const TTS_MODEL = "tts-1";
const TTS_VOICE = "nova"; // 既定(X-Character未指定時)
// キャラごとの声(デバイスの X-Character ヘッダで切替。変更はここを書き換えてdeployするだけ)
const VOICE_BY_CHARACTER = {
  robo: "alloy",
  girl: "nova",
};

const COMMON_RULES = `あなたは台所に置かれた小さなAI料理相棒です。回答はすべて音声で読み上げられます。
- 完全に自然な日本語の話し言葉だけで答える。箇条書き・番号(1. 2.)・記号・見出し・絵文字は絶対に使わない
- 2〜4文で簡潔に。前置き・復唱・同じ内容の言い換えをしない
- 複数の候補を挙げるときも「AかB、あとCもいいよ」のように文の中で自然に並べる
- 加熱の要否や生食の可否など食品安全に関わることは断言せず、「公的な情報の確認」を勧める
- 料理と無関係な発話には一言だけ軽く応じる`;

// モード別システムプロンプト(デバイスの X-Mode ヘッダで切り替え)
const MODE_PROMPTS = {
  consult: `${COMMON_RULES}
【いまのモード: レシピ相談】
- 料理の相談には具体的な料理名を1〜2個、一言の理由付きで提案する
- 最後に短い一言(「作り方いる?」「他に何かある?」など)で会話を続けやすくする`,
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

// 24kHz(OpenAI TTSのPCM出力) → 16kHz(デバイスのI2S設定)への線形補間リサンプル
function resample24kTo16k(pcm24) {
  const outLen = Math.floor((pcm24.length * 2) / 3);
  const out = new Int16Array(outLen);
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
  async fetch(request, env, ctx) {
    if (request.method !== "POST") {
      // 試聴用: GET /voice?name=shimmer&text=... で任意の声のWAVを返す(聞き比べ用)
      const url = new URL(request.url);
      if (url.pathname === "/voice") {
        const voice = url.searchParams.get("name") ?? TTS_VOICE;
        const text = url.searchParams.get("text") ?? "こんにちは。今夜は肉じゃがなんてどうかな。作り方も教えられるよ。";
        const tts = await openai("audio/speech", env, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ model: TTS_MODEL, voice, input: text, response_format: "pcm" }),
        });
        const pcm = new Int16Array(await tts.arrayBuffer()); // 24kHzのまま返す(試聴用WAV)
        const header = new DataView(new ArrayBuffer(44));
        const bytes = pcm.byteLength;
        const enc = new TextEncoder();
        new Uint8Array(header.buffer, 0, 4).set(enc.encode("RIFF"));
        header.setUint32(4, 36 + bytes, true);
        new Uint8Array(header.buffer, 8, 8).set(enc.encode("WAVEfmt "));
        header.setUint32(16, 16, true);
        header.setUint16(20, 1, true);
        header.setUint16(22, 1, true);
        header.setUint32(24, 24000, true);
        header.setUint32(28, 48000, true);
        header.setUint16(32, 2, true);
        header.setUint16(34, 16, true);
        new Uint8Array(header.buffer, 36, 4).set(enc.encode("data"));
        header.setUint32(40, bytes, true);
        const out = new Uint8Array(44 + bytes);
        out.set(new Uint8Array(header.buffer));
        out.set(new Uint8Array(pcm.buffer, 0, bytes), 44);
        return new Response(out, { headers: { "Content-Type": "audio/wav" } });
      }
      return new Response("kitchen-companion relay: POST audio/wav to /talk", { status: 200 });
    }

    // ※フィラー音声先行のストリーミング方式は、デバイス側を全受信再生に戻したため撤去した
    //   (経緯は docs/decisions.md「フィラー音声先行方式」の項を参照)
    const t0 = Date.now();
    try {
      const wav = await request.arrayBuffer();

      // 1. STT
      const fd = new FormData();
      fd.append("file", new Blob([wav], { type: "audio/wav" }), "speech.wav");
      fd.append("model", STT_MODEL);
      fd.append("language", "ja");
      const stt = await openai("audio/transcriptions", env, { method: "POST", body: fd });
      const transcript = (await stt.json()).text ?? "";
      const tStt = Date.now();

      // キャラごとの声(X-Characterヘッダで選択)
      const characterName = request.headers.get("X-Character") ?? "";
      const voice = VOICE_BY_CHARACTER[characterName] ?? TTS_VOICE;

      // 2. LLM(KVの会話履歴つき。「新規」指示があれば先に消す)
      const HISTORY_KEY = "session";
      const MAX_TURNS = 6;
      const mode = request.headers.get("X-Mode") ?? "consult";
      const systemPrompt = MODE_PROMPTS[mode] ?? MODE_PROMPTS.consult;
      if (request.headers.get("X-New-Session") === "1") {
        await env.HISTORY.delete(HISTORY_KEY);
      }
      const history = ((await env.HISTORY.get(HISTORY_KEY, "json")) ?? []).slice(-MAX_TURNS * 2);
      const chat = await openai("chat/completions", env, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          model: LLM_MODEL,
          messages: [
            { role: "system", content: systemPrompt },
            ...history,
            { role: "user", content: transcript },
          ],
          max_tokens: 250,
        }),
      });
      const reply = (await chat.json()).choices[0].message.content.trim();
      const tLlm = Date.now();

      const newHistory = [
        ...history,
        { role: "user", content: transcript },
        { role: "assistant", content: reply },
      ].slice(-MAX_TURNS * 2);
      await env.HISTORY.put(HISTORY_KEY, JSON.stringify(newHistory), { expirationTtl: 1800 });

      // 3. TTS(一括) → 16kHzへリサンプル+末尾300ms無音(デバイスのDMA対策)
      const tts = await openai("audio/speech", env, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ model: TTS_MODEL, voice, input: reply, response_format: "pcm" }),
      });
      const pcm16 = resample24kTo16k(new Int16Array(await tts.arrayBuffer()));
      const out = new Int16Array(pcm16.length + 4800);
      out.set(pcm16);
      const tTts = Date.now();

      return new Response(out.buffer, {
        headers: {
          "Content-Type": "application/octet-stream",
          "X-Transcript": encodeURIComponent(transcript),
          "X-Reply": encodeURIComponent(reply),
          "X-Timing": `stt=${tStt - t0}ms llm=${tLlm - tStt}ms tts=${tTts - tLlm}ms total=${tTts - t0}ms voice=${voice}`,
        },
      });
    } catch (e) {
      return new Response(`relay error: ${e.message}`, { status: 502 });
    }
  },
};
