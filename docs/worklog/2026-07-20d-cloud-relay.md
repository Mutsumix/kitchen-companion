# 2026-07-20(午後の部2): クラウド中継の構築と、コンパイラバグとの戦い

## やったこと

1. Cloudflare Worker中継(WAV受信→Whisper→gpt-4o-mini→tts-1→16kHz PCM返却)を実装・デプロイ
2. **curlでの全チェーンテスト成功**(デバイス実装前にクラウド側を単体検証する作戦が的中)
3. デバイス側の音声往復実装 → esp版rustcのコンパイラバグに阻まれ `wip/voice-loop` ブランチに保全
4. 「文鎮化?!」パニックからの復旧手順を確立

## クラウド操作の記録

- `npx wrangler login` → ブラウザでOAuth認可(アカウント2つ持ちのため、事前にブラウザで対象アカウントにログインしてから実施)
- wranglerの「Cloudflareスキルを入れるか?」プロンプト → 中身を調査してから承諾(~/.claude/skills/ にMarkdown群+Turnstile用スクリプト。全ファイル監査済み、通信先は公式APIのみで安全と確認)
- `npx wrangler deploy` → https://kitchen-companion-relay.mkajihara-dev.workers.dev
- `npx wrangler secret put OPENAI_API_KEY` → APIキーはユーザー自身のターミナルで入力(チャット経由でキーを扱わない方針を貫徹)

## 計測値(curl経由・実録音3秒WAV 96KB)

| 段 | 所要時間 |
|---|---|
| STT (whisper-1) | 1667ms |
| LLM (gpt-4o-mini) | 1193ms |
| TTS (tts-1) | 3187ms |
| **Worker内合計** | **6047ms** |
| curl計測の総往復 | 6.5秒 |

- 認識結果: 「今日のご飯何にしよう」(正確) → 応答「今夜はカレーなんてどう?簡単で美味しいよ!」(3.35秒の音声、107KB)
- **TTSが最大のボトルネック**。将来はストリーミング化 or 短文分割が候補

## 事故: OpenAI 429 insufficient_quota

ChatGPT課金≠API課金の罠。$10チャージで解決(troubleshooting.md参照)

## 事故: esp版rustcのコンパイラバグ(本日のボス戦)

デバイス側の音声往復コードがLLVM ICE → 回避すると起動クラッシュ → opt-level変更で画面点灯前ハング、という三段変化。詳細な経過と教訓は troubleshooting.md の当日エントリ参照。

- 切り分けに使った技法: git stashで既知の良い状態に戻す / クリーンビルドで再現確認 / opt-levelの変更 / 動くコードとの二分探索
- ケーブル抜け事故と重なって「文鎮化した」と誤認しパニック → 復旧手順を確立して troubleshooting.md に記録
- WIPコード(レビュー上は完成)は `wip/voice-loop` ブランチに保全。**次回: espup updateでツールチェーンを更新して再検証**

## 現在の状態

- main: WAV送信まで動く安定版(実機で画面復活確認済み)
- Worker: デプロイ済み・全チェーン動作確認済み(クラウド側は完成)
- 残作業: デバイス側の音声往復(ツールチェーン更新待ち)
