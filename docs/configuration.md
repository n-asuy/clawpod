# Configuration

ClawPod の設定は TOML で管理します。既定パスは `~/.clawpod/clawpod.toml` です。

## 基本ルール

- 設定ファイルが存在しなければ、起動時にデフォルト設定が生成される
- secret は直書きもできるが、通常は `*_env` を使って環境変数から読む
- browser profile は `agents.<id>.browser.profile` が優先され、未指定なら `browser.default_profile` を使う
- heartbeat policy は `agents.<id>.heartbeat` が最優先で、未指定時は `agent_defaults.heartbeat` を参照する

## トップレベル構成

主なトップレベルキーは次です。

| セクション | 用途 |
| --- | --- |
| `[daemon]` | ホームディレクトリ、ワークスペース、並列度 |
| `[server]` | Office API / UI の待受設定 |
| `[queue]` | リトライや dead-letter の設定 |
| `[session]` | DM の session スコープ |
| `[chain]` | team handoff の最大段数 |
| `[runner]` | 実行タイムアウト、既定 provider |
| `[browser]` | browser profile 定義 |
| `[heartbeat]` | heartbeat ループ全体の有効化と間隔 |
| `[agent_defaults]` | agent 共通の既定値 |
| `[agents.<id>]` | agent 定義 |
| `[custom_providers.<id>]` | custom provider 定義 |
| `[teams.<id>]` | team 定義 |
| `[[bindings]]` | チャネル / 相手ごとの固定ルーティング |
| `[channels.defaults]` | チャネル横断の既定値 |
| `[channels.*]` | Slack / Telegram / Discord 設定 |
| `[pairing]` | pairing コードの長さ、TTL、ロックアウト |

## 最小構成例

```toml
[daemon]
home_dir = "~/.clawpod"
workspace_dir = "~/.clawpod/workspace"
poll_interval_ms = 1000
max_concurrent_runs = 4

[server]
enabled = true
api_port = 3777
host = "127.0.0.1"
allow_public_bind = false

[runner]
default_provider = "anthropic"
timeout_sec = 120

[heartbeat]
enabled = false
interval_sec = 3600
sender = "Heartbeat"

[agents.default]
name = "Default"
provider = "anthropic"
model = "claude-sonnet-4-6"

[channels.telegram]
bot_token_env = "TELEGRAM_BOT_TOKEN"
```

## `[daemon]`

| キー | 説明 |
| --- | --- |
| `home_dir` | 状態、キュー、ログ、ファイルを置くベースディレクトリ |
| `workspace_dir` | agent ワークスペースの配置先 |
| `poll_interval_ms` | incoming queue のポーリング間隔 |
| `max_concurrent_runs` | 全体の同時実行上限 |

## `[server]`

| キー | 説明 |
| --- | --- |
| `enabled` | daemon 起動時に Office サーバを立ち上げるか |
| `api_port` | HTTP ポート |
| `host` | bind するホスト |
| `allow_public_bind` | `0.0.0.0` など公開 bind を明示的に許可するか |

`host` を公開向けにする場合は、`allow_public_bind = true` が必要です。

## `[queue]`

| キー | 説明 |
| --- | --- |
| `mode` | キュー挙動のモード定義。既定は `collect` |
| `max_retries` | 失敗時の再試行回数 |
| `backoff_base_ms` | リトライ待機の基準時間 |
| `dead_letter_enabled` | 失敗メッセージを dead-letter に送るか |

`mode` は列挙として `collect`, `followup`, `steer`, `steer-backlog` を持ちますが、現時点では `collect` 前提で理解してよい実装です。

## `[session]`

| キー | 説明 |
| --- | --- |
| `dm_scope` | DM セッションの切り方 |
| `main_key` | `dm_scope = "main"` のときに使う固定キー |

`dm_scope` の候補:

- `main`
- `per-peer`
- `per-channel-peer`
- `per-account-channel-peer`

## `[chain]`

| キー | 説明 |
| --- | --- |
| `max_chain_steps` | team handoff の最大ステップ数 |

## `[runner]`

| キー | 説明 |
| --- | --- |
| `default_provider` | agent に provider がない場合の既定値 |
| `timeout_sec` | 1 run あたりの実行タイムアウト |

provider の候補:

- `anthropic`
- `openai`
- `custom`
- `mock`

## `[browser]`

### 全体設定

| キー | 説明 |
| --- | --- |
| `default_profile` | agent 側で未指定のときに使う既定プロファイル |

### `browser.profiles.<name>`

| キー | 説明 |
| --- | --- |
| `cdp_port` | Chrome DevTools Protocol のポート |
| `profile_dir` | ブラウザプロフィール保存先 |
| `display` | 実行時に注入する `DISPLAY` |
| `kasm_port` | KasmVNC の待受ポート |
| `view_path` | Office から開く viewer path。未指定時は `/view/<name>` |
| `os_user` | 将来的な OS ユーザ分離用のメタデータ |
| `home_dir` | ブラウザ実行用ホームディレクトリ |
| `driver` | 任意のドライバ識別子 |

注意:

- `cdp_port`, `display`, `kasm_port`, `view_path` は profile 間で重複不可
- agent が参照する profile は必ず存在している必要がある

## `[heartbeat]`

これは heartbeat ループ全体の設定です。

| キー | 説明 |
| --- | --- |
| `enabled` | 自動 heartbeat を動かすか |
| `interval_sec` | ループ確認間隔の基準秒数 |
| `sender` | synthetic sender 名 |

個別 agent の heartbeat 詳細は `agents.<id>.heartbeat` で設定します。

## `[agent_defaults]`

現時点で主に使うのは heartbeat の既定値です。

```toml
[agent_defaults.heartbeat]
every = "30m"
target = "none"
ack_max_chars = 300
```

## `[agents.<id>]`

| キー | 説明 |
| --- | --- |
| `name` | 表示名 |
| `provider` | `anthropic` / `openai` / `custom` / `mock` |
| `model` | モデル ID |
| `think_level` | `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `adaptive` |
| `provider_id` | `provider = "custom"` のときに使う custom provider ID |
| `system_prompt` | インライン system prompt |
| `prompt_file` | agent ワークスペース相対または絶対パスの追加 prompt |
| `heartbeat` | agent 個別 heartbeat policy |
| `browser.profile` | 割り当てる browser profile |

### `agents.<id>.heartbeat`

| キー | 説明 |
| --- | --- |
| `every` | 実行間隔。例: `30m`, `1h` |
| `model` | heartbeat 時だけ使うモデル上書き |
| `prompt` | heartbeat 専用 prompt 上書き |
| `target` | `none`, `last`, `telegram`, `discord`, `slack`, `chatroom` |
| `to` | 配送先 ID の明示指定 |
| `account_id` | 配送に使う account ID |
| `ack_max_chars` | `HEARTBEAT_OK` 系の短い応答制限 |
| `direct_policy` | direct 宛 delivery の扱い。`allow` または `block` |
| `include_reasoning` | 推論内容を含めるか |
| `light_context` | 軽い context だけで heartbeat を回すか |
| `isolated_session` | 通常会話と分離した session を使うか |
| `active_hours.*` | 稼働時間帯の制御 |

`active_hours` には次を指定します。

- `start`
- `end`
- `timezone`

### `agents.<id>.browser`

```toml
[agents.reviewer.browser]
profile = "reviewer"
```

## `[custom_providers.<id>]`

| キー | 説明 |
| --- | --- |
| `name` | 表示名 |
| `harness` | `anthropic` または `openai` |
| `base_url` | API ベース URL |
| `api_key` | API キー直書き |
| `api_key_env` | API キーの環境変数名 |
| `model` | 既定モデル |

`api_key` か `api_key_env` のどちらかは実質必須です。

## `[teams.<id>]`

```toml
[teams.dev]
name = "Development"
leader_agent = "default"
agents = ["default", "reviewer"]
```

| キー | 説明 |
| --- | --- |
| `name` | 表示名 |
| `leader_agent` | team 入口になる agent |
| `agents` | team に所属する agent 一覧 |

## `[[bindings]]`

明示メンションがなくても routing を固定したい場合に使います。

```toml
[[bindings]]
agent_id = "ops"

[bindings.match]
channel = "slack"
peer_id = "C0123456789"
```

`match` に使える主なキー:

- `channel`
- `account_id`
- `peer_id`
- `group_id`
- `thread_id`

一致スコアがもっとも高い binding が採用されます。

## `[channels.defaults]`

チャネル個別設定の前に効く共通既定値です。現時点では heartbeat 表示設定に使います。

```toml
[channels.defaults.heartbeat]
show_ok = false
show_alerts = true
use_indicator = true
```

## `[channels.*]`

### Telegram

```toml
[channels.telegram]
bot_token_env = "TELEGRAM_BOT_TOKEN"
```

### Discord

```toml
[channels.discord]
bot_token_env = "DISCORD_BOT_TOKEN"
guild_id = "1234567890"
mention_only = true
```

### Slack

```toml
[channels.slack]
bot_token_env = "SLACK_BOT_TOKEN"
app_token_env = "SLACK_APP_TOKEN"
```

各チャネルには次を持てます。

- token 直書き
- `*_env` による環境変数参照
- `access`
- `heartbeat`

## `channels.<name>.access`

受信許可ポリシーです。

| キー | 説明 |
| --- | --- |
| `dm_policy` | `open`, `allowlist`, `pairing`, `disabled` |
| `group_policy` | `disabled`, `mention_only`, `allowlist`, `open` |
| `allow_from` | DM 許可 sender 一覧 |
| `group_allow_from` | グループ許可 sender 一覧 |
| `channels.<id>.allow` | 特定チャネルの許可 / 拒否 |
| `channels.<id>.require_mention` | 特定チャネルで mention 必須にするか |

例:

```toml
[channels.telegram.access]
dm_policy = "pairing"
group_policy = "mention_only"

[channels.telegram.access.channels."-1001234567890"]
allow = true
require_mention = false
```

## `channels.<name>.heartbeat`

チャネル側の heartbeat 表示ポリシーです。

| キー | 説明 |
| --- | --- |
| `show_ok` | OK 系 heartbeat を表示するか |
| `show_alerts` | alert 系を表示するか |
| `use_indicator` | indicator を使うか |

## `[pairing]`

| キー | 説明 |
| --- | --- |
| `code_length` | pairing code の長さ |
| `code_ttl_secs` | code の有効期限 |
| `max_failed_attempts` | 失敗許容回数 |
| `lockout_secs` | lockout 秒数 |

## 環境変数

よく使うもの:

- `TELEGRAM_BOT_TOKEN`
- `DISCORD_BOT_TOKEN`
- `SLACK_BOT_TOKEN`
- `SLACK_APP_TOKEN`
- custom provider 用の任意の `*_API_KEY`

## 実運用で最初に調整する項目

優先度が高いのは次です。

1. `agents.*`
2. `channels.*`
3. `session.dm_scope`
4. `heartbeat`
5. `browser.profiles.*`

実装構造まで見たい場合は [architecture.md](./architecture.md) を参照してください。
