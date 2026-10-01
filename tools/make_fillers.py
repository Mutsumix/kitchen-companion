#!/usr/bin/env python3
"""フィラー音声(「考え中」のつなぎ)をキャラごとの声で生成してデバイス埋め込み用rawに変換。

Workerの試聴エンドポイント(GET /voice)でTTSし、24kHz WAV→16kHzモノラルraw(PCM16LE)へ。
出力: assets/fillers/<キャラ名>_<番号>.raw

使い方: python3 tools/make_fillers.py
"""
import struct
import urllib.parse
import urllib.request
from pathlib import Path

import numpy as np

WORKER = "https://kitchen-companion-relay.mkajihara-dev.workers.dev/voice"
OUT = Path(__file__).resolve().parent.parent / "assets" / "fillers"

# キャラ名 → 声(cloud/src/index.js の VOICE_BY_CHARACTER と揃える)
VOICES = {"robo": "alloy", "girl": "nova"}
PHRASES = [
    "うーん、ちょっと考えるね。",
    "はいはい、ちょっと待っててね。",
]


def fetch_wav(voice: str, text: str) -> bytes:
    url = f"{WORKER}?name={voice}&text={urllib.parse.quote(text)}"
    # User-AgentなしのリクエストはCloudflareに403で弾かれるため付与する
    req = urllib.request.Request(url, headers={"User-Agent": "curl/8.0"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


def wav_to_16k_raw(wav: bytes) -> bytes:
    # 試聴エンドポイントのWAVは24kHz/16bit/モノラル・44バイト標準ヘッダ
    rate = struct.unpack("<I", wav[24:28])[0]
    pcm = np.frombuffer(wav[44:], dtype="<i2").astype(np.float64)
    if rate != 16000:  # 線形補間で16kHzへ
        n_out = int(len(pcm) * 16000 / rate)
        x_out = np.linspace(0, len(pcm) - 1, n_out)
        pcm = np.interp(x_out, np.arange(len(pcm)), pcm)
    return pcm.astype("<i2").tobytes()


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for char, voice in VOICES.items():
        for i, phrase in enumerate(PHRASES):
            raw = wav_to_16k_raw(fetch_wav(voice, phrase))
            path = OUT / f"{char}_{i}.raw"
            path.write_bytes(raw)
            print(f"{path.name}: {len(raw)}バイト ({len(raw)/32000:.1f}秒) voice={voice}")


if __name__ == "__main__":
    main()
