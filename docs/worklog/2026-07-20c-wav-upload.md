# 2026-07-20(午後の部): 録音WAVのMacへのHTTP送信

## やったこと

音声パイプラインの縦一本のうち「録音→ネットワーク送信」区間を貫通。タッチ→3秒録音→WAV化→Mac上の受信サーバへPOST→`afplay`で内容確認、まで成功。

## 手順

```sh
# Mac側受信サーバ(tools/wav_receiver.py を新規作成)
python3 tools/wav_receiver.py 8000    # recordings/ に保存

# MacのLAN IP確認 → cfg.toml に server_url として追記
ipconfig getifaddr en0                # → 192.168.1.13

# 検証
afinfo recordings/<file>.wav          # 1ch / 16000Hz / Int16 / 3.000sec を確認
afplay recordings/<file>.wav          # 発話内容を耳で確認
```

デバイス側: タッチ→ビープ→ビープ終了後から3秒録音(混入防止)→`EspHttpConnection`でPOST。送信結果とms数を画面表示。

## 事故と解決(書籍のハイライト級)

1. **録音後に黙って再起動**: `memory allocation of 96044 bytes failed` → 録音96KB+WAV結合96KBの二重確保が内蔵RAMに収まらなかった。ヘッダ44バイトと本体を別書き込みにして解決
2. **PSRAM有効化に失敗**(1回目): `CONFIG_SPIRAM_MODE_OCT=y` は CoreS3 では誤り。`octal_psram: PSRAM chip is not connected, or wrong PSRAM line mode` → **Quadモード**に変更で `Found 8MB PSRAM device / 80MHz / memory test OK`
3. `CONFIG_SPIRAM_IGNORE_NOTFOUND=y` を入れておいたため、モード間違い状態でも起動は継続していた(フェイルセーフの効用)

詳細は troubleshooting.md の同日エントリ参照。

## 計測値

- **WAV送信所要時間: 332ms**(96,044バイト、Wi-Fi中継器経由、HTTP/平文)— 往復レイテンシ予算の最初の実測点
- Mac→デバイスのping RTT: 平均186ms(中継器経由。ばらつき大: 94〜278ms)
- 録音データ: 16kHz/16bit/モノラル3秒 = 96,000バイト+44バイトヘッダ

## 音質メモ

- 発話内容は明瞭に聞き取れる。多少のノイズあり(マイク品質由来と推定)。増幅・DCオフセット除去などの前処理は今後の課題。まずこのままSTTに通して認識精度で判断する方針

## 次のマイルストーン

クラウド区間: Cloudflare Workers + STT/LLM/TTS(BYOK)。APIキー取得とCloudflareアカウント作成から
