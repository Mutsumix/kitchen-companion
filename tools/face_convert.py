#!/usr/bin/env python3
"""顔スプライトの位置合わせ+RGB565変換。

AI生成のフレームはベゼル(顔の枠)の位置・大きさが微妙に揃っていないため、
1. 輝点プロジェクションでベゼル矩形を検出
2. 基準フレーム(idle_0)にスケール+平行移動で位置合わせ(NEAREST補間)
3. assets/face/ に補正済みPNGと、デバイス組み込み用 .raw(RGB565ビッグエンディアン)を出力

使い方: python3 tools/face_convert.py
"""
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent / "assets"
W, H = 320, 155
THRESH = 40  # これより明るい画素を「点灯」とみなす
COL_MIN = 60  # ベゼル縦エッジとみなす列の点灯数(アンテナ等の細い部品を除外)
ROW_MIN = 120  # ベゼル横エッジとみなす行の点灯数

REFERENCE = "idle_0"
NAMES = [f"{s}_{i}" for s in ("idle", "listen", "think", "speak") for i in range(3)]

# キャラごとの入力ディレクトリと変換設定。
# align=True はベゼル(枠)基準の位置合わせ(ロボット顔向け)。
# キャラ2はベゼルのような基準構造が無いため無補正で変換する
CHARACTERS = [
    {"name": "robo", "src": ROOT / "openai-output" / "face1", "align": True},
    {"name": "girl", "src": ROOT / "openai-output" / "face2", "align": False},
]


def bezel_rect(img: Image.Image):
    """ベゼル(顔の外枠)のバウンディングボックスを検出する"""
    a = np.asarray(img.convert("L"))
    lit = a > THRESH
    cols = lit.sum(axis=0)  # 列ごとの点灯数
    rows = lit.sum(axis=1)  # 行ごとの点灯数
    xs = np.where(cols >= COL_MIN)[0]
    ys = np.where(rows >= ROW_MIN)[0]
    if len(xs) == 0 or len(ys) == 0:
        raise SystemExit(f"ベゼル検出失敗(閾値要調整): cols_max={cols.max()} rows_max={rows.max()}")
    return xs[0], ys[0], xs[-1], ys[-1]  # left, top, right, bottom


def to_rgb565_be(img: Image.Image) -> bytes:
    a = np.asarray(img.convert("RGB"), dtype=np.uint16)
    r, g, b = a[..., 0] >> 3, a[..., 1] >> 2, a[..., 2] >> 3
    v = (r << 11) | (g << 5) | b
    return v.astype(">u2").tobytes()  # ビッグエンディアン(embedded-graphics ImageRawの既定)


def convert_character(char):
    dst = ROOT / "face" / char["name"]
    dst.mkdir(parents=True, exist_ok=True)
    src = char["src"]

    if char["align"]:
        ref_img = Image.open(src / f"{REFERENCE}.png").convert("RGB")
        rl, rt, rr, rb = bezel_rect(ref_img)
        rw, rh = rr - rl, rb - rt
        print(f"[{char['name']}] 基準 {REFERENCE}: bezel=({rl},{rt})-({rr},{rb}) size={rw}x{rh}")

    for name in NAMES:
        img = Image.open(src / f"{name}.png").convert("RGB")
        if char["align"]:
            l, t, r, b = bezel_rect(img)
            w, h = r - l, b - t
            sx, sy = rw / w, rh / h
            # ベゼルサイズを基準に合わせて全体を拡縮(3%未満の差はそのまま)
            if abs(sx - 1) > 0.03 or abs(sy - 1) > 0.03:
                img = img.resize((round(img.width * sx), round(img.height * sy)), Image.NEAREST)
                l, t = round(l * sx), round(t * sy)
            canvas = Image.new("RGB", (W, H), (0, 0, 0))
            canvas.paste(img, (rl - l, rt - t))
        else:
            canvas = Image.new("RGB", (W, H), (0, 0, 0))
            canvas.paste(img.crop((0, 0, W, H)), (0, 0))
        canvas.save(dst / f"{name}.png")
        (dst / f"{name}.raw").write_bytes(to_rgb565_be(canvas))
        print(f"[{char['name']}] {name} -> 変換済み")


def main():
    for char in CHARACTERS:
        convert_character(char)
    print(f"完了: {ROOT / 'face'} 配下にキャラ別のPNG(確認用)と.raw(組み込み用)を出力")


if __name__ == "__main__":
    main()
