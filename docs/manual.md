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

## 6. マイク録音(ES7210 + I2S)

1. ES7210(addr 0x40)をI2Cで初期化 — レジスタ値25個はM5Unifiedの実績値をそのまま使用
2. I2S受信: BCLK=GPIO34, WS=GPIO33, DIN=GPIO14, MCLK=GPIO0。ESP32-S3がマスター、16kHz/16bit/ステレオ
3. 動作確認は「RMS音量を計算して画面にレベルメーター描画」(声で緑バーが動けば成立)

- **つまずき**: ES7210のレジスタ初期化順序は意味がある(リセット→設定→最後にクロック確定 0x01=0x14)。順序ごとM5Unifiedに従う
- **つまずき(予防)**: esp-idf-halのI2S APIはバージョン差が大きい。手元のソース(`~/.cargo/git/checkouts/esp-idf-hal-*/src/i2s/std.rs`)で `new_std_rx` のシグネチャを確認してから書いた
- **一次情報**: ES7210初期化値は M5Unified `src/M5Unified.cpp` の CoreS3 分岐(WebFetchで抽出)。ES7210 Datasheet(Everest Semiconductor)はレジスタの意味の確認用
- **結果**: 2026-07-20実機検証済み。声・拍手で緑のレベルメーターが反応し、録音パイプライン成立(この方式で一発動作)

## 7. スピーカー再生(AW88298 + I2S)

マイクと同一I2Sバスの送信側。ドライバを `new_std_bidir` にすると送受同時に動く。

1. AW88298(addr 0x36)をI2Cで初期化 — **レジスタは16bit幅・ビッグエンディアン**(`value.to_be_bytes()`)
2. サンプルレート依存レジスタ0x06: `(sample_rate+1102)/2205` を計算し、レートテーブル `{4,5,6,8,10,11,15,20,22,44}` で最初に収まるインデックスを `0x14C0` にOR(16kHz → `0x14C3`)
3. I2S: `new_std_bidir` でDOUT=GPIO13を追加(BCLK/WS/MCLKはマイクと共用)
4. 動作確認は1kHzサイン波150msのビープ再生

- **つまずき**: AW88298だけレジスタが16bit幅。8bitのつもりで書くと動かない
- **つまずき**: シリアルポート名はUSB再列挙で変わる(`usbmodem1101`→`usbmodem101`)。接続エラー時は `ls /dev/cu.*` で再確認
- **つまずき(重要)**: I2S送信DMAはデータが尽きると最後のバッファを繰り返す → **再生データは必ず無音で終わらせる**。鳴りっぱなしの緊急停止は `espflash reset`([troubleshooting](troubleshooting.md))
- **つまずき**: ブロッキング書き込みは再生が終わるまでループを止める → 再生カーソル+タイムアウト0の小分け書き込みで他処理と並行させる
- **一次情報**: AW88298初期化値は M5Unified `src/M5Unified.cpp` の `_speaker_enabled_cb_core_s3`(WebFetchで抽出)。AW9523のP0_2がスピーカーイネーブル
- **結果**: 2026-07-20実機検証済み。タッチ→ビープ、再生中のマイク拾いも確認(送受同時動作)

## 8. 録音WAVのHTTP送信(Mac受信サーバで音質確認)

クラウドに行く前に、APIキー不要で録音品質と通信を固める段。

1. Mac側: `python3 tools/wav_receiver.py`(ポート8000、`recordings/` に保存)。MacのIPは `ipconfig getifaddr en0`
2. `cfg.toml` に `server_url = "http://<MacのIP>:8000/upload"` を追記
3. デバイス側: タッチ→ビープ→**ビープ終了後から**3秒録音(ビープ混入防止)→44バイトWAVヘッダ+PCMを`EspHttpConnection`でPOST
4. 送信結果と所要時間(ms)を画面表示。Macで `afinfo` / `afplay` で検証

- **つまずき(重要)**: 録音バッファとWAV結合バッファの二重確保で96KB×2 → メモリ確保失敗でパニック再起動。ヘッダと本体を別書き込みにして回避([troubleshooting](troubleshooting.md))
- **つまずき(重要)**: CoreS3のPSRAM 8MBは**Quadモード**。`CONFIG_SPIRAM_MODE_OCT` では認識しない([troubleshooting](troubleshooting.md))
- **つまずき**: 「画面が起動画面に戻る」=パニック再起動のサイン。シリアルでパニックメッセージを取るのが先決
- **一次情報**: HTTPクライアントは esp-idf-svc の `examples/http_request.rs` / WAVフォーマットは RIFF仕様(44バイト標準ヘッダ) / PSRAM設定は ESP-IDF「SPI RAM config」ドキュメントと PlatformIO の CoreS3 ボード定義(`qio_qspi`)
- **結果**: 2026-07-20実機検証済み。3秒96KBのWAVがMacに到着、`afplay` で発話内容を確認(多少のノイズあり、前処理は今後)。**送信所要: 332ms**(初の実測レイテンシ)

## 9. クラウド中継(Cloudflare Worker + OpenAI)

デバイスからのWAVを Whisper→gpt-4o-mini→tts-1 に中継し、16kHz PCM(+末尾無音)を返す。コードは [cloud/src/index.js](../cloud/src/index.js)。

1. `npx wrangler login`(アカウント複数持ちの場合、先にブラウザで対象アカウントへログイン)
2. `cd cloud && npx wrangler deploy`
3. `npx wrangler secret put OPENAI_API_KEY`(キーは自分のターミナルで入力。チャットやファイルに書かない)
4. **デバイスを触る前にcurlで全チェーンを検証**: `curl -X POST --data-binary @recordings/xxx.wav -H "Content-Type: audio/wav" https://<worker>/talk` — X-Transcript/X-Reply/X-Timingヘッダで各段を確認

- **つまずき**: ChatGPT課金≠API課金。APIはplatform.openai.comで別途チャージが必要([troubleshooting](troubleshooting.md))
- **つまずき**: 16kHzへのリサンプルと末尾無音付加はWorker側で行う(デバイスを単純に保つ+デバイス側コンパイラバグの回避)
- **一次情報**: OpenAI API リファレンス(audio/transcriptions, chat/completions, audio/speech。TTSの `response_format: "pcm"` は24kHz/16bit/モノラル) / Cloudflare Workers ドキュメント
- **結果**: 2026-07-20 curl検証済み。Worker内訳 stt=1667ms / llm=1193ms / tts=3187ms(合計6秒。TTSがボトルネック)

## 10. 緊急復旧手順(画面真っ黒・反応なしのとき)

1. `ls /dev/cu.*` — USBが見えていればチップは生きている(ケーブル変更でポート名は変わる)
2. `espflash reset --port <port>` でソフトリセット
3. 電源ボタン6秒長押し(電源断)→短押し(起動)
4. 最終手段: **リセットボタン(側面)を緑LED点灯まで長押し=ダウンロードモード** → `espflash flash <既知の良いバイナリ> --port <port>` → `espflash reset`

- **つまずき(最重要)**: コンパイラのICEを回避してビルドを通すと、静かなミスコンパイルで「起動しない/画面が点かない」ファームができることがある。**ICEを見たらそのコードは信用しない**([troubleshooting](troubleshooting.md)の実録参照)
