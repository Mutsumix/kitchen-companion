# ハマりどころ記録

遭遇したエラー・ハマりどころ・回避策を時系列で記録する。本の「ハマりどころコラム」の一次素材。エラーメッセージは原文のまま残すこと。

## 2026-07-14: `.zshrc` への追記が既存行と連結して壊れた

環境変数の永続化のため `.zshrc` に `source $HOME/export-esp.sh` を追記した際、既存の最終行に改行が無かったため、追記内容が前の行と連結されて `.zshrc` が壊れた。

- **症状**: 新しいシェルで環境変数が読み込まれない(壊れた行がエラーになる)
- **回避策**: `.zshrc` を開いて手動で2行に分離
- **教訓**: `echo >> ~/.zshrc` で追記する前に、ファイル末尾に改行があるか確認する(`tail -c 1 ~/.zshrc | xxd` で確認できる)

## 2026-07-19: `cargo run` に espflash のオプションを渡すには `--` が必要

シリアルポートを明示指定しようと `cargo run --port /dev/cu.usbmodem1101` と実行したら cargo に拒否された。

```
error: unexpected argument '--port' found

  tip: to pass '--port' as a value, use '-- --port'
```

- **原因**: `--port` は cargo のオプションではなく、runner(espflash)のオプション。cargo に渡す引数と runner に渡す引数は `--` で区切る
- **回避策**: `cargo run -- --port /dev/cu.usbmodem1101` とする。ポートが1つしか無い場合は espflash が自動検出するので省略も可

## (事前調査で判明)SDカードは FAT32 必須

CoreS3 で使う SD カードは FAT32 でフォーマットされている必要がある。64GB 以上のカードは標準で exFAT のため、そのままでは認識しない。16GB(FAT32)を使用する。
