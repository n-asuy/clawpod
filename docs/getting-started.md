# Getting Started

この文書は、ClawPod をローカルで立ち上げて疎通確認するための最短手順です。

## 前提

- Rust ツールチェーンが入っていること
- リポジトリ直下で作業していること
- 実プロバイダを使う場合は `claude` または `codex` CLI が使えること

最初の起動確認だけなら、`mock` provider で十分です。API キーも不要です。

## 1. 診断を実行する

```bash
cargo run -p runtime -- doctor
```

`doctor` は次を確認します。

- ランタイム用ディレクトリの有無
- Office サーバ設定
- browser profile の待受状態
- `claude` / `codex` CLI の有無
- 認証状態

## 2. mock 構成で daemon を起動する

このリポジトリには簡易確認用の設定が入っています。

ファイル: `examples/local-smoke.toml`

```bash
cargo run -p runtime -- --config examples/local-smoke.toml daemon
```

この設定では次を使います。

- ホームディレクトリ: `./.clawpod-local`
- provider: `mock`
- agent: `default`, `reviewer`
- team: `dev`

## 3. 別ターミナルからメッセージを投入する

```bash
cargo run -p runtime -- --config examples/local-smoke.toml enqueue \
  --channel local \
  --sender alice \
  --sender-id alice_1 \
  --peer-id alice_1 \
  --message "hello from local smoke"
```

## 4. 結果を確認する

確認ポイントは次です。

- `./.clawpod-local/queue/outgoing/*.json` に応答が出る
- `http://127.0.0.1:3777/office` で Office UI が開く
- `curl http://127.0.0.1:3777/api/runs` で実行履歴が取れる

## 5. 主要コマンド

普段よく使うのは次です。

```bash
cargo run -p runtime -- daemon
cargo run -p runtime -- status
cargo run -p runtime -- health
cargo run -p runtime -- office
cargo run -p runtime -- logs --follow
cargo run -p runtime -- doctor
```

agent 管理:

```bash
cargo run -p runtime -- agent list
cargo run -p runtime -- agent add reviewer --provider openai --model gpt-5.4
cargo run -p runtime -- agent show reviewer
```

heartbeat 管理:

```bash
cargo run -p runtime -- heartbeat enable
cargo run -p runtime -- heartbeat last
cargo run -p runtime -- heartbeat run --agent default
```

認証:

```bash
cargo run -p runtime -- auth status
cargo run -p runtime -- auth claude
cargo run -p runtime -- auth openai
```

## 6. デフォルト設定ファイル

設定ファイルの既定パスは次です。

```text
~/.clawpod/clawpod.toml
```

ClawPod は起動時にこのファイルがなければ、デフォルト設定を自動生成します。

実運用ではまずこのファイルを開いて、agents / channels / tokens を整える形になります。

## 7. 実プロバイダを使う

### Anthropic

- `clawpod auth claude` で `claude login` を実行する
- agent の `provider = "anthropic"` を設定する

### OpenAI

- `clawpod auth openai` で `codex login` を実行する
- agent の `provider = "openai"` を設定する

### Custom Provider

`custom_providers.<id>` に `base_url` と認証情報を設定し、agent 側で `provider = "custom"` と `provider_id = "<id>"` を指定します。

## 8. チャネル接続を有効にする

Slack / Telegram / Discord は、設定ファイルにトークン参照を書いた上で daemon を起動します。

例:

```toml
[channels.telegram]
bot_token_env = "TELEGRAM_BOT_TOKEN"

[channels.slack]
bot_token_env = "SLACK_BOT_TOKEN"
app_token_env = "SLACK_APP_TOKEN"
```

## 9. 日常運用の入口

まず見る場所はこの3つです。

- `clawpod doctor`: 依存と待受状態の確認
- Office UI: 実行履歴、設定、heartbeat、セッションの確認
- `clawpod logs --follow`: daemon の継続監視

次に設定を詰める場合は [configuration.md](./configuration.md) を参照してください。
