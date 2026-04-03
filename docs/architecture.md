# Architecture

この文書は、ClawPod の実装構造をコードベースに沿って俯瞰するためのものです。

## 全体像

ClawPod は「永続的に動く daemon」と「各機能を分割した Rust workspace」で構成されています。

高レベルの流れは次です。

```text
Channel Connector
  -> queue/incoming
  -> Queue Processor
  -> Routing / Session Resolution
  -> Runner
  -> queue/outgoing
  -> Channel Connector
```

これと並行して、Office サーバ、heartbeat ループ、イベント記録が動きます。

## 主要クレート

| Crate | 役割 |
| --- | --- |
| `runtime` | CLI エントリポイント。daemon、doctor、service、auth、heartbeat 操作 |
| `config` | `clawpod.toml` の読み書き、検証、secret 解決 |
| `domain` | 共通型。agent / team / run / heartbeat / routing 関連 |
| `queue` | incoming / processing / outgoing / dead-letter の処理本体 |
| `routing` | `@agent`、`@team`、binding、handoff タグの解釈 |
| `runner` | `claude` / `codex` / custom / mock の実行抽象 |
| `agent` | agent ワークスペース作成、session ワークスペース作成、prompt 構築 |
| `team` | leader から teammate への chain 実行、fan-out |
| `session` | session key 生成 |
| `store` | JSON ファイルベースの状態保存 |
| `observer` | イベント記録、health snapshot |
| `server` | Office UI / API |
| `heartbeat` | 定期実行、policy 解決、delivery、dedup |
| `plugins` | incoming / outgoing / event hook |
| `telegram` / `discord` / `slack` | 各チャネル接続 |
| `pairing` | sender pairing の検証 |

## runtime の構成

`runtime` の `daemon` サブコマンドは主に次のコンポーネントを起動します。

- Queue Processor
- HeartbeatService
- Office server
- Telegram connector
- Discord connector
- Slack connector

Office を無効化した場合は `server.enabled = false` で起動できます。

## メッセージ処理の流れ

### 1. 受信

チャネルコネクタは受信イベントを `queue/incoming/*.json` に書き込みます。CLI の `enqueue` も同じ流れです。

### 2. Queue Processor

`queue::QueueProcessor` が `incoming` を監視し、以下を担当します。

- stale な `processing` ファイルの復旧
- 同時実行数の制御
- リトライと backoff
- dead-letter への退避

### 3. ルーティング

入力イベントに対して次を順番に解決します。

- `@agent` / `@team`
- binding rule
- 既定 agent
- ルーティング affinity 更新用の `[route_to: agent]`

team 宛ての依頼は leader agent から開始されます。

### 4. ワークスペース解決

agent ごとに次のワークスペースを持ちます。

- agent root: `workspace/<agent_id>/`
- session root: `workspace/<agent_id>/sessions/<session_key>/`

agent root には次が初回自動生成されます。

- `AGENTS.md`
- `heartbeat.md`
- `.clawpod/SOUL.md`
- `memory/`
- `.claude/`
- `.codex/`
- `.agents/`

session ディレクトリは、共有すべきものを symlink / copy しつつ会話ごとの状態を分けます。

### 5. Prompt 構築

prompt はおおむね次で組み立てられます。

- agent 設定の `system_prompt`
- `prompt_file`
- agent identity / teammate 情報
- heartbeat 実行時の補足指示
- ユーザーメッセージ

### 6. Runner 実行

`runner::CliRunner` は provider ごとに外部 CLI を起動します。

- Anthropic: `claude`
- OpenAI: `codex`
- Custom: harness に応じた互換実行
- Mock: 依存なしのローカル応答

OpenAI 実行時は Codex の JSONL を parse し、run event として扱います。Claude 側も stream-json 出力を読める前提で parse を試みます。

### 7. Team chain

レスポンスに `[@agent: ...]` が含まれると team chain を継続します。

- 1件なら次 agent へ handoff
- 複数件なら fan-out で並列実行
- `chain.max_chain_steps` に達すると停止

### 8. 出力

通常チャネルへの応答は `queue/outgoing` に書かれます。`heartbeat` と `chatroom` は通常 outbound queue には出しません。

## Session モデル

session key は chat type と `session.dm_scope` に応じて変わります。

例:

- `main`: `agent:<id>:main`
- `per-channel-peer`: `agent:<id>:telegram:direct:alice_1`
- group: `agent:<id>:slack:group:C123`
- thread: `agent:<id>:slack:thread:T123`

これにより agent ごとの継続会話を分離します。

## 状態保存

`store::StateStore` は JSON ファイルに次を保存します。

- runs
- chain steps
- chatroom messages
- heartbeat runs
- events
- sessions
- sender access
- routing affinity

設計上は軽量ですが、単一ノード前提のファイルベース永続化です。

## ランタイムディレクトリ

既定では `~/.clawpod/` 配下に次を作ります。

```text
~/.clawpod/
├── clawpod.toml
├── queue/
│   ├── incoming/
│   ├── processing/
│   ├── outgoing/
│   └── dead_letter/
├── workspace/
├── files/
├── logs/
│   ├── daemon.log
│   ├── daemon.stderr.log
│   └── events.jsonl
├── runs/
├── events/
└── state/
    └── clawpod-state.json
```

## Office server

`server` クレートは Axum ベースの API と単一 HTML の Office UI を提供します。

主な API 群:

- `/api/health`
- `/api/settings`
- `/api/agents`
- `/api/teams`
- `/api/bindings`
- `/api/runs`
- `/api/runs/:run_id/events`
- `/api/sessions`
- `/api/heartbeat/runs`
- `/api/browser/profiles`
- `/api/events/stream`

Office からは設定編集、agent ファイル編集、heartbeat 実行、run 履歴閲覧ができます。

## Heartbeat

`heartbeat::HeartbeatService` は agent ごとの schedule を持ちます。単なる全 agent 一括 tick ではなく、agent ごとの `every` に従って due 判定します。

heartbeat 実行時には次が加わります。

- active hours 判定
- 重複 suppression
- delivery target 解決
- indicator 判定
- 履歴記録

## Browser profile

browser profile は config で定義され、実行時には metadata と環境変数に展開されます。

主に注入される値:

- `DISPLAY`
- `AGENT_BROWSER_PROFILE`
- `AGENT_BROWSER_CDP_PORT`
- `AGENT_BROWSER_PROFILE_DIR`
- `AGENT_BROWSER_KASM_PORT`
- `AGENT_BROWSER_VIEW_PATH`

この仕組みにより、agent ごとに安定したブラウザ状態を持たせられます。

## Plugin hook

`plugins/` 配下の plugin は次の3種類の hook を持てます。

- incoming transform
- outgoing transform
- event dispatch

hook は標準入出力で JSON をやりとりする単純なコマンド実行モデルです。

## 並行性と安全性

並行実行は2段で制御されます。

- 全体並列数: `daemon.max_concurrent_runs`
- agent 単位の直列化: shared `memory/` を守るための per-agent lock

これにより、全体 throughput を維持しつつ agent 内の競合を抑えています。

## この文書の次に読むもの

- 利用側の導入: [getting-started.md](./getting-started.md)
- 設定詳細: [configuration.md](./configuration.md)
- 仕様検討中の論点: `docs/` 内の日時付き設計メモ
