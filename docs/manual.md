# 作業手順マニュアル

CoreS3デバイス開発の全手順を、つまずきどころ・一次情報とセットで記録するマニュアル。
時系列の生ログは [worklog/](worklog/)、エラー詳細は [troubleshooting.md](troubleshooting.md) を参照。
**運用ルール: マイルストーンごとに本マニュアルへ手順・つまずき・一次情報を追記してからコミットする。**

## 0. 共通の一次情報(常に参照するもの)

| 情報源 | URL | 用途 |
|---|---|---|
| The Rust on ESP Book | https://docs.espressif.com/projects/rust/book/ | esp-rs開発全体の公式ガイド |
| esp-idf-template | https://github.com/esp-rs/esp-idf-template | プロジェクトの元テンプレート |
| esp-idf-hal(APIソース) | https://github.com/esp-rs/esp-idf-hal | ドライバAPI。**破壊的変更が多いためdocs.rsでなく手元のgitチェックアウト(`~/.cargo/git/checkouts/`)の実バージョンを直接読むのが確実** |
| M5Stack CoreS3 公式ドキュメント | https://docs.m5stack.com/en/core/CoreS3 | ピンアサイン・内蔵デバイス一覧・回路図 |
| M5Unified(公式Arduinoライブラリ) | https://github.com/m5stack/M5Unified | **内蔵IC群の初期化レジスタ値の事実上の一次情報**。データシートより先にここを見る |
| M5GFX | https://github.com/m5stack/M5GFX | LCD・タッチ周りの実績値(ピン、色順、輝度制御) |

## 1. 開発環境構築

手順は [setup.md](setup.md) の通り(brew → rustup → espup → cargo generate)。

- **つまずき**: `.zshrc` へのecho追記が既存行と連結して壊れる([troubleshooting](troubleshooting.md))
- **つまずき**: 初回ビルドは10〜20分かかる。フリーズではない
- **一次情報**: The Rust on ESP Book の "Setting Up a Development Environment" 章

## 2. 実機書き込みとシリアルモニタ

```sh
ls /dev/cu.*                                  # ポート確認(CoreS3 → /dev/cu.usbmodemXXXX)
cargo run -- --port /dev/cu.usbmodem1101      # 書き込み+モニタ
```

- **つまずき**: espflashのオプションは `--` の後ろに書く(cargoのオプションと区別される)
- **つまずき**: モニタ出力をパイプで加工するとバッファリングで読めなくなる。`script -q /dev/null` でも解決しない。**デバイスの画面にステータスを描くのが最も確実**([troubleshooting](troubleshooting.md))
- **一次情報**: espflash Book https://esp-rs.github.io/espflash/

## 3. 画面点灯(AXP2101 / AW9523 / ILI9342C)

CoreS3の画面は「電源IC・IOエキスパンダをI2Cで初期化しないと真っ暗のまま」という初見殺し構造。

手順の骨子([src/main.rs](../src/main.rs) のセクション1〜6):

1. I2Cドライバ初期化(SDA=GPIO12, SCL=GPIO11, 400kHz)
2. AXP2101(addr 0x34)へレジスタ書き込み — DLDO1(バックライト電源)有効化が肝
3. AW9523(addr 0x58)へレジスタ書き込み — P1_1がLCDリセット線
4. SPI(SCK=36, MOSI=37, CS=3, DC=35)+ mipidsi でILI9342C初期化
5. `ColorOrder::Bgr` + `ColorInversion::Inverted` を指定(RGB3色バーで目視検証)

- **つまずき**: esp-idf-hal 0.46で `prelude` が廃止。`use esp_idf_hal::units::FromValueType;` が必要([troubleshooting](troubleshooting.md))
- **つまずき**: LCDリセット線はESP32のGPIOにつながっていない(AW9523経由)。mipidsiにリセットピンは渡さずソフトリセットで動く
- **一次情報**: 初期化レジスタ値は M5Unified `src/M5Unified.cpp`(AXP2101/AW9523)および M5GFX `src/M5GFX.cpp`(パネル設定)。輝度はAXP2101のDLDO1電圧(reg 0x99)で決まる
- **一次情報**: mipidsi https://docs.rs/mipidsi / embedded-graphics https://docs.rs/embedded-graphics

## 4. タッチ入力(FT6336)

同じI2Cバスのaddr 0x38。追加クレート不要。

1. レジスタ0x02から5バイト連続読み(点数、X上位/下位、Y上位/下位)
2. 座標は12bit。上位バイトは下位4bitのみ有効(上位2bitはイベントフラグ)
3. 20ms前後のポーリングで十分(INT線=GPIO21は未使用でも動く)

- **つまずき**: FT6336のリセット線もAW9523(P0_0)。画面点灯の初期化を済ませていれば追加作業なし
- **一次情報**: FT6336のレジスタマップは FocalTech アプリケーションノート(FT6x36 Datasheet)。実装参考は M5GFX `Touch_FT5x06`

## 5. Wi-Fi接続

1. 認証情報は `toml-cfg` + gitignore済み `cfg.toml`(雛形: [cfg.toml.example](../cfg.toml.example))
2. `BlockingWifi` + `EspWifi` で接続、**リトライ必須**(初回タイムアウトは正常系)
3. 結果(IP)は画面に描画して確認

- **つまずき**: `cfg.toml` の変更だけでは再コンパイルされない。`touch src/main.rs` してからビルド
- **つまずき**: 「認証OK→アソシエーションで切断」はパスワード間違いではなく電波・AP側の問題。パスワード間違いは4-wayハンドシェイクで落ちる。今回は中継器SSID(`_EXT`)への変更で解決([troubleshooting](troubleshooting.md))
- **つまずき**: ESP32-S3は2.4GHz帯のみ。5GHz専用SSIDには繋がらない
- **一次情報**: esp-idf-svc のwifi例 https://github.com/esp-rs/esp-idf-svc/blob/master/examples/wifi.rs / ESP-IDF Wi-Fi Driver docs(切断理由コード表)

## 6. マイク録音(ES7210 + I2S)※検証中

1. ES7210(addr 0x40)をI2Cで初期化 — レジスタ値25個はM5Unifiedの実績値をそのまま使用
2. I2S受信: BCLK=GPIO34, WS=GPIO33, DIN=GPIO14, MCLK=GPIO0。ESP32-S3がマスター、16kHz/16bit/ステレオ
3. 動作確認は「RMS音量を計算して画面にレベルメーター描画」(声で緑バーが動けば成立)

- **つまずき**: ES7210のレジスタ初期化順序は意味がある(リセット→設定→最後にクロック確定 0x01=0x14)。順序ごとM5Unifiedに従う
- **つまずき(予防)**: esp-idf-halのI2S APIはバージョン差が大きい。手元のソース(`~/.cargo/git/checkouts/esp-idf-hal-*/src/i2s/std.rs`)で `new_std_rx` のシグネチャを確認してから書いた
- **一次情報**: ES7210初期化値は M5Unified `src/M5Unified.cpp` の CoreS3 分岐(WebFetchで抽出)。ES7210 Datasheet(Everest Semiconductor)はレジスタの意味の確認用
