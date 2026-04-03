# ClawPod 概要

ClawPod は、Slack / Telegram / Discord から受け取ったメッセージを AI エージェント群に渡し、継続セッションを保ちながら応答を返す Rust 製の常駐ランタイムです。

1つのバイナリで次をまとめて扱います。

- チャネル接続
- メッセージキュー
- エージェント / チームへのルーティング
- LLM 実行
- セッション管理
- 状態保存
- Office UI / API
- heartbeat 自動実行

## 何を解決するか

ClawPod は「単発のチャットボット」ではなく、「複数のエージェントが継続的に動く運用ランタイム」を目指しています。

典型的な用途は次です。

- 業務チャネルからの問い合わせをエージェントに振り分ける
- エージェントごとに独立した作業ディレクトリと記憶を持たせる
- チーム構成でレビュー、引き継ぎ、並列相談をさせる
- Web UI から状態確認、設定編集、heartbeat 実行、ファイル編集を行う

## 主要概念

### Agent

個別の人格と実行環境を持つ単位です。各 agent は次を持てます。

- `provider`
- `model`
- `think_level`
- `system_prompt`
- `prompt_file`
- `heartbeat`
- `browser.profile`

各 agent には専用ワークスペースが作られ、`AGENTS.md`、`heartbeat.md`、`.clawpod/SOUL.md`、`memory/`、`sessions/` などが配置されます。

### Team

複数 agent を束ねる単位です。チームには `leader_agent` があり、`@team` で入った依頼はまず leader に渡されます。

レスポンス中に `[@reviewer: ...]` のようなタグがあると、次の agent へハンドオフします。複数タグがあれば fan-out で並列実行されます。

### Session

会話の継続単位です。DM / グループ / スレッドごとに session key を生成し、各 agent の `sessions/<session_key>/` に状態を分離します。

DM のスコープは `session.dm_scope` で切り替えられます。

### Binding

`@agent` や `@team` を書かなくても、特定チャネルや相手を固定の agent に結びつけるルールです。

### Heartbeat

定期実行タスクです。agent ごとの `heartbeat.md` と heartbeat policy に基づき、自動巡回や手動実行を行います。

### Browser Profile

agent に紐づくブラウザ実行環境です。CDP ポート、表示ディスプレイ、KasmVNC ポート、プロフィールディレクトリをプロファイル単位で持てます。

## 処理フロー

通常メッセージはおおむね次の流れで処理されます。

1. Slack / Telegram / Discord コネクタ、または `enqueue` からメッセージを受け取る
2. `queue/incoming` に JSON として積む
3. Queue Processor が取り出して routing を決定する
4. session key を作り、agent ワークスペースと session ワークスペースを用意する
5. system prompt とユーザーメッセージを組み立てて runner を起動する
6. 必要なら team handoff / fan-out を行う
7. 応答を `queue/outgoing` に書き出す
8. 各チャネルコネクタが外部サービスへ返送する
9. 実行履歴、イベント、セッション情報を state / logs に保存する

## ルーティングの基本

### 直接 agent を指定

```text
@default 調査して
@reviewer この差分を見て
```

### team を指定

```text
@dev この不具合を直して
```

### team 内でハンドオフ

```text
[@reviewer: この差分の懸念点を確認して]
```

### 固定バインディング

```toml
[[bindings]]
agent_id = "ops"

[bindings.match]
channel = "slack"
peer_id = "C0123456789"
```

## 対応プロバイダ

現時点での実装上の provider 種別は次です。

- `anthropic`
- `openai`
- `custom`
- `mock`

`mock` を使うと API キーなしでローカル疎通確認ができます。

## Office UI

Office UI は運用用の Web 画面です。主に次を扱えます。

- health / queue / runs / sessions の確認
- agent / team / binding の編集
- agent の `AGENTS.md` / `heartbeat.md` / `SOUL.md` の編集
- heartbeat の有効化、手動実行、履歴確認
- browser profile の閲覧

## 関連文書

- 導入手順: [getting-started.md](./getting-started.md)
- 設定詳細: [configuration.md](./configuration.md)
- 実装構成: [architecture.md](./architecture.md)
