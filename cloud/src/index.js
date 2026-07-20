// kitchen-companion 中継Worker
// デバイスからWAVを受け取り、STT→LLM→TTSを中継して16kHz PCMを返す。
// APIキーは secret (OPENAI_API_KEY) にのみ存在し、デバイスには一切置かない。

const SYSTEM_PROMPT = `あなたは台所に置かれた小さなAI料理相棒です。日本語で答えます。
- 回答は音声で読み上げられるため、2文以内で短く話し言葉で答える
- レシピ相談・調理の段取り・食材の扱いに親身に応じる
- 加熱の要否や生食の可否など食品安全に関わることは断言せず、「公的な情報の確認」を勧める
- 料理と無関係な発話には一言だけ軽く応じる`;

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

      // 2. LLM
      const chat = await openai("chat/completions", env, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          model: "gpt-4o-mini",
          messages: [
            { role: "system", content: SYSTEM_PROMPT },
            { role: "user", content: transcript },
          ],
          max_tokens: 200,
        }),
      });
      const reply = (await chat.json()).choices[0].message.content.trim();
      const tLlm = Date.now();

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
