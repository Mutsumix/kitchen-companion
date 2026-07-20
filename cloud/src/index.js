// kitchen-companion 中継Worker
// デバイスからWAVを受け取り、STT→LLM→TTSを中継して16kHz PCMを返す。
// APIキーは secret (OPENAI_API_KEY) にのみ存在し、デバイスには一切置かない。

// 使用モデル(レイテンシ比較しやすいようここで一元管理)
// STT: whisper-1 → gpt-4o-mini-transcribe に変更(高速化。認識精度は実用で確認済み)
// TTS: gpt-4o-mini-tts を試したが日本語の音質が悪化したため tts-1 に戻した
const STT_MODEL = "gpt-4o-mini-transcribe";
const LLM_MODEL = "gpt-4o-mini";
const TTS_MODEL = "tts-1";
const TTS_VOICE = "nova";

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
      return new Response("kitchen-companion relay: POST audio/wav to /talk", { status: 200 });
    }
    const wav = await request.arrayBuffer();
    const mode = request.headers.get("X-Mode") ?? "consult";
    const newSession = request.headers.get("X-New-Session") === "1";
    // 応答ストリームを即座に返し、フィラー音声→本編の順で書き込む。
    // デバイスは届いた順に再生するだけなので、待ち時間中に「考え中」の声が出せる
    const { readable, writable } = new TransformStream();
    ctx.waitUntil(pipeline(env, wav, mode, newSession, writable));
    return new Response(readable, {
      headers: { "Content-Type": "application/octet-stream" },
    });
  },
};

// 待ち時間に流すフィラー(初回にTTS生成してKVへキャッシュ)
const FILLERS = [
  "うーん、ちょっと考えるね。",
  "はいはい、ちょっと待っててね。",
];

async function fillerAudio(env, idx) {
  const key = `filler_v1_${idx}`;
  const cached = await env.HISTORY.get(key, "arrayBuffer");
  if (cached) return new Uint8Array(cached);
  const tts = await openai("audio/speech", env, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model: TTS_MODEL, voice: TTS_VOICE, input: FILLERS[idx], response_format: "pcm" }),
  });
  const pcm16 = resample24kTo16k(new Int16Array(await tts.arrayBuffer()));
  const bytes = new Uint8Array(pcm16.buffer, 0, pcm16.byteLength);
  await env.HISTORY.put(key, bytes.buffer); // フィラーは失効させない
  return bytes;
}

async function pipeline(env, wav, mode, newSession, writable) {
  const t0 = Date.now();
  const writer = writable.getWriter();
  try {
    // 0. フィラーを先に流す(この裏でSTT以降が走る)
    const filler = await fillerAudio(env, Math.floor(Math.random() * FILLERS.length));
    await writer.write(filler);
    // フィラーと本編の間に短い無音(0.4秒)を挟む
    await writer.write(new Uint8Array(6400 * 2));

    // 1. STT
    const fd = new FormData();
    fd.append("file", new Blob([wav], { type: "audio/wav" }), "speech.wav");
    fd.append("model", STT_MODEL);
    fd.append("language", "ja");
    const stt = await openai("audio/transcriptions", env, { method: "POST", body: fd });
    const transcript = (await stt.json()).text ?? "";
    const tStt = Date.now();

    // 2. LLM(KVの会話履歴つき。「新規」指示があれば先に消す)
    const HISTORY_KEY = "session";
    const MAX_TURNS = 6;
    const systemPrompt = MODE_PROMPTS[mode] ?? MODE_PROMPTS.consult;
    if (newSession) {
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

    // 3. TTS(一括。文分割ストリーミングは発音品質の問題で不採用) → 末尾無音つきで送出
    const tts = await openai("audio/speech", env, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model: TTS_MODEL, voice: TTS_VOICE, input: reply, response_format: "pcm" }),
    });
    const pcm16 = resample24kTo16k(new Int16Array(await tts.arrayBuffer()));
    await writer.write(new Uint8Array(pcm16.buffer, 0, pcm16.byteLength));
    await writer.write(new Uint8Array(4800 * 2)); // 末尾300ms無音(デバイスのDMA対策)
    console.log(`talk ok: stt=${tStt - t0}ms llm=${tLlm - tStt}ms tts=${Date.now() - tLlm}ms 「${transcript}」`);
  } catch (e) {
    console.log(`pipeline error: ${e.message}`);
  } finally {
    await writer.close().catch(() => {});
  }
}
