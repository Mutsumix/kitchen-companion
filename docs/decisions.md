# 設計判断の記録

プロジェクトで下した判断と、その理由を時系列で残す。本文の「判断の言語化」の一次素材。

## 2026-07-19: Cargo.lock をgit追跡対象にする

esp-idf-template の .gitignore は `Cargo.lock` を除外していたが、追跡対象に変更した。

- esp-idf-hal 系は破壊的変更が多く、「動いた組み合わせ」を固定することが最優先
- 特に Cargo.toml の `[patch.crates-io]` で esp-idf-sys / esp-idf-hal / esp-idf-svc を gitリポジトリ直接参照にしているため、Cargo.lock が無いと再ビルドのたびに最新コミットを拾い、読者環境で再現しない恐れがある
- バイナリクレートではロックファイルをコミットするのが Cargo の公式推奨

## 2026-07-19: テンプレート由来の `[patch.crates-io]`(git直接参照)は当面維持

cargo generate 時の選択で esp-idf-sys / hal / svc が git の最新を参照する構成になっている。「バージョン完全固定」の方針とは緊張関係にあるが、初回ビルドが完走した実績のある構成なので崩さない。Cargo.lock の追跡によって実質的にコミット単位で固定されている。crates.io のリリース版だけで動くことが確認できたタイミングで patch を外すことを検討する。
