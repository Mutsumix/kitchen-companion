# 2026-07-15: 開発環境構築とプロジェクト生成

## やったこと

Mac(MacBook Air M4)にESP32-S3向けRust開発環境を構築し、テンプレートからプロジェクトを生成、初回ビルドまで。

## 実行コマンド

```sh
# 1. ビルド補助ツール(ESP-IDFのC世界用)
brew install cmake ninja

# 2. Rust本体(標準インストールを選択)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# 3. ESP32開発ツール4点
cargo install espup espflash ldproxy cargo-generate

# 4. Xtensa対応Rustツールチェーン(espup 0.17.1)
espup install

# 5. 環境変数の永続化(.zshrcに追記)
#    ※ここで事故: 既存最終行に改行が無く連結されて壊れた → troubleshooting.md 参照
source $HOME/export-esp.sh   # .zshrc に記載

# 6. プロジェクト生成
cargo generate esp-rs/esp-idf-template cargo
#    Project Name: kitchen-companion / MCU: esp32s3 / advanced options: false

# 7. 初回ビルド
cd kitchen-companion && cargo build
```

## 結果・確認事項

- `espup --version` → 0.17.1
- `rustup toolchain list` → `stable-aarch64-apple-darwin (default)` + `esp`
- `echo $LIBCLANG_PATH` → `~/.rustup/toolchains/esp/xtensa-esp32-elf-clang/esp-20.1.1_20250829/esp-clang/lib`
- 初回ビルドは完走(所要10〜20分程度)

## ハマりどころ

- `.zshrc` 追記時の改行事故 → [troubleshooting.md](../troubleshooting.md) に記録
