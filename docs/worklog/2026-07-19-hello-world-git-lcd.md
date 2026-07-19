# 2026-07-19: 実機Hello world・GitHub・画面点灯

## やったこと

1. ビルド完走の確認(前回からの持ち越し)
2. gitリポジトリ初期化・GitHub(https://github.com/Mutsumix/kitchen-companion)へプッシュ
3. CoreS3実機でHello worldのシリアルログ確認
4. 画面点灯: AXP2101/AW9523初期化 + LCDにRGB3色バー描画

## 実行コマンド

```sh
# ビルド確認(完走済みだったため一瞬で終了)
cargo build

# git初期化(ブランチ名をmainに変更して初回コミット)
git branch -m main
git add <テンプレート一式 + docs/>
git commit
git remote add origin https://github.com/Mutsumix/kitchen-companion.git
git push -u origin main

# 実機接続確認(CoreS3をUSB-C接続後)
ls /dev/cu.*        # → /dev/cu.usbmodem1101 が出現

# 書き込み+シリアルモニタ
cargo run -- --port /dev/cu.usbmodem1101 --non-interactive
```

## GitHub上の操作(クラウド操作の記録)

1. github.com で新規リポジトリ `Mutsumix/kitchen-companion` を作成(空リポジトリ、README等は追加しない)
2. 上記の `git remote add` → `git push` でローカルから初回プッシュ

## 結果

Hello worldのシリアルログ(抜粋):

```
I (375) main_task: Calling app_main()
I (375) kitchen_companion: Hello, world!
I (375) main_task: Returned from app_main()
```

画面点灯は `src/main.rs` に以下を実装して達成(コミット `4175503`):

- I2C(SDA=GPIO12, SCL=GPIO11)で AXP2101(0x34)・AW9523(0x58)をレジスタ直叩き初期化
  - AXP2101のDLDO1(バックライト電源)をONにしないと画面が真っ暗のまま(CoreS3最大の罠)
  - LCDリセット線はAW9523のP1_1につながっている
- SPI(SCK=36, MOSI=37, CS=3, DC=35)+ mipidsi(ILI9342C)でLCD初期化
- RGB3色バーを描画 → 実機で上から赤・緑・青を目視確認。色順設定は `ColorOrder::Bgr` + `ColorInversion::Inverted` が正解

クレート追加: `esp-idf-hal` / `embedded-graphics` / `mipidsi`(バージョン完全固定、[decisions.md](../decisions.md) 参照)

## 計測値

- 書き込み(フラッシュ)〜再起動: 体感10秒前後
- インクリメンタルビルド: 2〜3秒

## ハマりどころ(詳細は troubleshooting.md)

- `cargo run` にespflashのオプションを渡すには `--` 区切りが必要
- esp-idf-hal 0.46 で `prelude` モジュールが廃止されている
