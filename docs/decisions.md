# 設計判断の記録

プロジェクトで下した判断と、その理由を時系列で残す。本文の「判断の言語化」の一次素材。

## 2026-07-20: 録音操作はプッシュ・トゥ・トーク(押している間録音)

- 3秒固定録音は発話が打ち切られて実用にならない(ユーザーフィードバック)
- VAD(無音検出)より実装が単純で誤動作がなく、「話し終わり」をユーザーが明示できる
- 上限15秒の安全弁+0.5秒未満キャンセル。将来VADを載せる場合も、このタッチ起点設計は残る

## 2026-07-20: ツールチェーンは1.97.0.0に固定(1.95.0.0のコンパイラバグ回避)

- 1.95.0.0はLLVM ICE(Cannot select: Constant)を起こす。1.97.0.0で解消を確認
- `espup install --toolchain-version 1.97.0.0` で明示インストール(espup updateはstableマークまでしか上げない)
- opt-levelはテンプレート既定の "z" に復帰。メインタスクスタックは20000(talk()の4KBバッファ対応)

## 2026-07-19: Cargo.lock をgit追跡対象にする

esp-idf-template の .gitignore は `Cargo.lock` を除外していたが、追跡対象に変更した。

- esp-idf-hal 系は破壊的変更が多く、「動いた組み合わせ」を固定することが最優先
- 特に Cargo.toml の `[patch.crates-io]` で esp-idf-sys / esp-idf-hal / esp-idf-svc を gitリポジトリ直接参照にしているため、Cargo.lock が無いと再ビルドのたびに最新コミットを拾い、読者環境で再現しない恐れがある
- バイナリクレートではロックファイルをコミットするのが Cargo の公式推奨

## 2026-07-19: 画面点灯の実装方針

- **LCDドライバは `mipidsi` クレートを採用**(`embedded-graphics` + `mipidsi` + `esp-idf-hal` を追加、バージョン完全固定)。自前でSPIコマンドを叩くより読者がコピペで動く可能性が高い。ILI9342C は `ColorOrder::Bgr` + `ColorInversion::Inverted` が正解(実機のRGB3色バーで検証済み)
- **AXP2101 / AW9523 は専用クレートを使わずI2Cレジスタ直叩き**。依存を増やさず、「何をしているか」を書籍で解説しやすい。レジスタ値は M5Unified / M5GFX の実績値をそのまま使用
- **動作検証は単色塗りでなくRGB3色バー**。色順設定(RGB/BGR)の誤りを目視で検出できる

## 2026-07-20: Wi-Fi認証情報は toml-cfg + gitignore した cfg.toml で管理

- リポジトリはGitHub公開のため、SSID/パスワードのソース直書きは事故のもと
- esp-rs公式ブックでも使われる `toml-cfg` を採用。`cfg.toml`(gitignore済み)に実値、`cfg.toml.example`(コミット)に雛形
- 注意: cfg.toml の変更だけでは再コンパイルが走らないことがあるため、変更後は `touch src/main.rs` してからビルドする

## 2026-07-19: テンプレート由来の `[patch.crates-io]`(git直接参照)は当面維持

cargo generate 時の選択で esp-idf-sys / hal / svc が git の最新を参照する構成になっている。「バージョン完全固定」の方針とは緊張関係にあるが、初回ビルドが完走した実績のある構成なので崩さない。Cargo.lock の追跡によって実質的にコミット単位で固定されている。crates.io のリリース版だけで動くことが確認できたタイミングで patch を外すことを検討する。
