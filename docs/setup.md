# 開発環境構築手順(Mac / Apple Silicon)

M5Stack CoreS3 向け Rust(std環境)開発のセットアップ。macOS(Apple Silicon)+ zsh 前提。

## 1. ビルド補助ツール

ESP-IDF(C世界)のビルドに必要。

```sh
brew install cmake ninja
```

## 2. Rust本体

標準インストールを選択。

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

## 3. ESP32開発ツール4点

```sh
cargo install espup espflash ldproxy cargo-generate
```

- `espup`: Xtensa対応Rustツールチェーンのインストーラ
- `espflash`: 書き込み+シリアルモニタ
- `ldproxy`: ESP-IDFのリンカを仲介するツール
- `cargo-generate`: テンプレートからのプロジェクト生成

## 4. Xtensa対応Rustツールチェーン

```sh
espup install
```

これで `rustup toolchain list` に `esp` が追加される。

## 5. 環境変数の永続化

```sh
# 追記前にファイル末尾に改行があるか確認すること
# (無いと既存行と連結して .zshrc が壊れる → troubleshooting.md 参照)
echo 'source $HOME/export-esp.sh' >> ~/.zshrc
```

新しいシェルで `echo $LIBCLANG_PATH` にパスが出れば成功。

## 6. プロジェクト生成

```sh
cargo generate esp-rs/esp-idf-template cargo
# Project Name: kitchen-companion / MCU: esp32s3 / advanced options: false
```

## 7. 初回ビルド(10〜20分かかる)

```sh
cd kitchen-companion && cargo build
```

## 8. Wi-Fi認証情報の設定

```sh
cp cfg.toml.example cfg.toml
# cfg.toml に自宅の2.4GHz帯Wi-FiのSSID/パスワードを記入(gitignore済み)
```

## 9. 実機書き込み

CoreS3をUSB-Cで接続し、`ls /dev/cu.*` でポート(例: `/dev/cu.usbmodem1101`)を確認して:

```sh
cargo run
# ポート明示なら: cargo run -- --port /dev/cu.usbmodem1101
```

## 検証済みバージョン

- espup 0.17.1 / ESP-IDF v5.5.3 / Rust stable-aarch64-apple-darwin + espツールチェーン
- 依存クレートは Cargo.toml / Cargo.lock 参照(バージョン完全固定)
