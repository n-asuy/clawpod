---
name: hetzner-deploy
description: Hetzner Cloud上にClawPodをデプロイする。新規サーバー作成、Rustビルド、systemdサービス登録、Tailscaleによるリモートアクセス設定までを実行。「ClawPodデプロイ」「clawpod deploy」「Hetznerにclawpod」「clawpodサーバー作成」「clawpodアップデート」などがトリガー。
allowed-tools:
  - Bash
---

# ClawPod Hetzner Deploy

Hetzner Cloud上にClawPodインスタンスをプロビジョニングしてデプロイする。

## Safety Rules

- **NEVER execute delete commands.** `hcloud server delete` 等の破壊的操作は禁止。
- **NEVER expose or log API tokens, keys, or credentials.**
- **ALWAYS ask for confirmation** before create/modify operations. コマンドを提示して承認を待つ。
- **ALWAYS suggest a snapshot** before any modification:

```bash
hcloud server create-image <server> --type snapshot --description "Backup before changes"
```

## Prerequisites

- `hcloud` CLIがインストール・設定済み
- Hetznerに SSH鍵が登録済み
- ローカルマシンからSSH接続が可能
- ClawPodリポジトリへのアクセス

## Workflow

### Step 0: Prerequisites Check

#### hcloud CLI

```bash
hcloud version
```

未インストールの場合:

```bash
# macOS
brew install hcloud

# Linux (Debian/Ubuntu)
sudo apt update && sudo apt install hcloud-cli
```

#### hcloud context

```bash
hcloud context list
```

コンテキストがない場合、APIトークンを作成して設定する:

1. https://console.hetzner.cloud/ にログイン
2. プロジェクトを選択 -> Security -> API Tokens
3. Generate API token (Read & Write)
4. CLIに登録:

```bash
hcloud context create clawpod
# プロンプトでトークンを入力（トークンはローカルに保存される）
```

#### SSH key

```bash
ls -la ~/.ssh/id_ed25519.pub 2>/dev/null || ls -la ~/.ssh/id_rsa.pub 2>/dev/null
```

SSH鍵がない場合は生成する:

```bash
ssh-keygen -t ed25519 -C "clawpod-deploy" -f ~/.ssh/id_ed25519
```

Hetznerに鍵が登録されているか確認:

```bash
hcloud ssh-key list
```

未登録の場合は登録する:

```bash
hcloud ssh-key create --name clawpod-key --public-key-from-file ~/.ssh/id_ed25519.pub
```

全て揃ったら Step 1 に進む。

### Step 1: Current State Check

```bash
hcloud context active
hcloud ssh-key list
hcloud server list
```

既存サーバーの有無で分岐:

- **A: New server** -> Step 2
- **B: Existing server (install/update)** -> Step 3 (skip Step 2)

### Step 2: Create Server (new only)

Defaults (user can override):

| Parameter   | Default        |
|-------------|----------------|
| Server type | cpx22          |
| Image       | ubuntu-24.04   |
| Location    | nbg1           |
| SSH key     | first from list |

Server name: ask user. Default `clawpod`.

```bash
hcloud server create \
  --name <server-name> \
  --type cpx22 \
  --image ubuntu-24.04 \
  --location nbg1 \
  --ssh-key "<ssh-key-name>"
```

Get IPv4 from output. Wait for SSH:

```bash
sleep 10
ssh -o StrictHostKeyChecking=no -o ConnectTimeout=15 root@<ip> "uname -a"
```

Retry once after 10s on failure. If still failing, report to user.

> **Note**: On server recreation, IP changes. Run `ssh-keygen -R <old-ip>` to remove stale known_hosts entry.

### Step 3: Server Diagnostics

```bash
ssh -o StrictHostKeyChecking=no root@<ip> "\
  echo '=== OS ===' && cat /etc/os-release | head -3 && \
  echo '=== Rust ===' && (rustc --version 2>/dev/null || echo 'not installed') && \
  echo '=== Cargo ===' && (cargo --version 2>/dev/null || echo 'not installed') && \
  echo '=== Tailscale ===' && (tailscale version 2>/dev/null || echo 'not installed') && \
  echo '=== Node.js ===' && (node --version 2>/dev/null || echo 'not installed') && \
  echo '=== Codex CLI ===' && (npx codex --version 2>/dev/null || echo 'not installed') && \
  echo '=== ClawPod binary ===' && (which clawpod 2>/dev/null || echo 'not installed') && \
  echo '=== ClawPod service ===' && (systemctl is-active clawpod 2>/dev/null || echo 'not active') && \
  echo '=== ClawPod config ===' && (test -f /root/.clawpod/clawpod.toml && echo 'exists' || echo 'not found')"
```

Branch by result:

- **Clean server** -> Step 4A (full setup)
- **Rust installed, no clawpod** -> Step 4B (build only)
- **ClawPod already installed** -> Step 4C (update)

### Step 4A: Full Setup (clean server)

#### System packages

```bash
ssh root@<ip> "apt-get update -qq && apt-get install -y \
  build-essential pkg-config libssl-dev git"
```

Timeout: 120s.

#### Install Rust

```bash
ssh root@<ip> "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && \
  source /root/.cargo/env && rustc --version"
```

Timeout: 120s.

#### Install Tailscale

```bash
ssh root@<ip> "curl -fsSL https://tailscale.com/install.sh | sh"
```

Tailscale認証。ブラウザでURLを開いてログインする必要がある:

```bash
ssh root@<ip> "tailscale up"
```

> **Important**: `tailscale up` は認証URLを出力する。ユーザーにURLを提示し、ブラウザで認証を完了するよう伝える。

認証完了後、Tailscale IPを確認:

```bash
ssh root@<ip> "tailscale ip -4"
```

#### Install Node.js and Codex CLI

`clawpod auth openai` requires the Codex CLI (`@openai/codex`), which requires Node.js.

```bash
ssh root@<ip> "curl -fsSL https://deb.nodesource.com/setup_22.x | bash - && \
  apt-get install -y nodejs && \
  npm install -g @openai/codex"
```

Timeout: 120s.

#### Clone and build

```bash
ssh root@<ip> "source /root/.cargo/env && \
  git clone <repo-url> /opt/clawpod-src && \
  cd /opt/clawpod-src && \
  cargo build --release -p runtime"
```

Timeout: 600s. Release build with `opt-level = "z"` and LTO takes time.

> **Important**: Ask user for the repository URL. The clawpod repo root IS the project root (not under `experiments/`). If private, SSH key or token-based clone is needed.

#### Install binary

```bash
ssh root@<ip> "cp /opt/clawpod-src/target/release/clawpod /usr/local/bin/clawpod && \
  chmod +x /usr/local/bin/clawpod && \
  clawpod --help"
```

> **Note**: `clawpod --version` is not implemented. Use `clawpod --help` to verify the binary works.

Continue to Step 5.

### Step 4B: Build Only (Rust exists, no clawpod)

```bash
ssh root@<ip> "source /root/.cargo/env && \
  cd /opt/clawpod-src && git pull && \
  cargo build --release -p runtime && \
  cp target/release/clawpod /usr/local/bin/clawpod"
```

If `/opt/clawpod-src` does not exist, fall back to Step 4A clone step.

Timeout: 600s.

### Step 4C: Update (clawpod already installed)

```bash
ssh root@<ip> "source /root/.cargo/env && \
  cd /opt/clawpod-src && git pull && \
  cargo build --release -p runtime && \
  systemctl stop clawpod 2>/dev/null; true && \
  cp target/release/clawpod /usr/local/bin/clawpod && \
  systemctl start clawpod 2>/dev/null; true && \
  clawpod --help | head -1"
```

Timeout: 600s.

### Step 4D: Provider Authentication

Authenticate with AI providers. Ask user which provider(s) they want to use.

#### OpenAI (via Codex CLI)

`clawpod auth openai` delegates to `codex` CLI for OAuth login. The default mode requires a local browser callback, which fails on headless servers. Use `codex login --device-auth` instead:

```bash
ssh root@<ip> "codex login --device-auth"
```

This outputs:
1. A URL (`https://auth.openai.com/codex/device`)
2. A one-time code (expires in 15 minutes)

Present both to the user. The user opens the URL in their browser, signs in, and enters the code. The command completes once authenticated.

> **Note**: The previous code expires when a new `codex login --device-auth` is invoked. Do not retry rapidly.

#### Anthropic

Set `ANTHROPIC_API_KEY` in the env file (Step 6).

#### Check auth status

```bash
ssh root@<ip> "clawpod auth status"
```

### Step 5: Initial Configuration

Create minimal `clawpod.toml` if not exists. Ask user for:

- Slack/Discord/Telegram tokens (optional)
- AI provider preference (openai/anthropic) — auth is handled in Step 4D
- Agent names and roles

```bash
ssh root@<ip> "mkdir -p /root/.clawpod/workspace/default && \
  cat > /root/.clawpod/clawpod.toml << 'TOML'
[daemon]
home_dir = \"/root/.clawpod\"
workspace_dir = \"/root/.clawpod/workspace\"
poll_interval_ms = 1000
max_concurrent_runs = 4

[server]
enabled = true
api_port = 3777
host = \"127.0.0.1\"
allow_public_bind = false

[queue]
mode = \"collect\"
max_retries = 3
backoff_base_ms = 500
dead_letter_enabled = true

[session]
dm_scope = \"per-channel-peer\"
main_key = \"main\"

[runner]
default_provider = \"openai\"  # or \"anthropic\"
timeout_sec = 120

[pairing]
code_length = 8
code_ttl_secs = 3600

# OpenAI models (via codex): gpt-5.4, gpt-5.3-codex, gpt-5.1-codex, etc.
# Anthropic models: claude-sonnet-4-6, claude-opus-4-6, etc.
[agents.default]
name = \"Default\"
provider = \"openai\"  # or \"anthropic\" — match runner.default_provider
model = \"gpt-5.4\"

[teams.main]
name = \"Main\"
leader_agent = \"default\"
agents = [\"default\"]

# Channels — uncomment and configure as needed.
# Token values are read from the env file (Step 6) via *_env keys.
# [channels.slack]
# bot_token_env = \"SLACK_BOT_TOKEN\"
# app_token_env = \"SLACK_APP_TOKEN\"

# [channels.discord]
# bot_token_env = \"DISCORD_BOT_TOKEN\"

# [channels.telegram]
# bot_token_env = \"TELEGRAM_BOT_TOKEN\"
TOML"
```

> **Important**: Channel tokens are NOT read from the env file automatically. The `[channels.*]` section in TOML must exist with `*_env` keys pointing to the env var names. Without this, the channel stays `disabled` even if env vars are set.

### Step 6: systemd Service

```bash
ssh root@<ip> "cat > /etc/systemd/system/clawpod.service << 'EOF'
[Unit]
Description=ClawPod agent runtime daemon
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
ExecStart=/usr/local/bin/clawpod daemon
WorkingDirectory=/root/.clawpod
Restart=on-failure
RestartSec=5
EnvironmentFile=-/root/.clawpod/env

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload && systemctl enable clawpod"
```

Create env file for secrets:

```bash
ssh root@<ip> "cat > /root/.clawpod/env << 'EOF'
ANTHROPIC_API_KEY=sk-ant-...
# SLACK_BOT_TOKEN=xoxb-...
# SLACK_APP_TOKEN=xapp-...
# DISCORD_BOT_TOKEN=...
# TELEGRAM_BOT_TOKEN=...
EOF
chmod 600 /root/.clawpod/env"
```

> **Important**: Ask user for actual API keys. Never hardcode placeholder values in production.

Start service:

```bash
ssh root@<ip> "systemctl start clawpod && systemctl status clawpod --no-pager"
```

### Step 7: Tailscale Serve (Office remote access)

Tailscale ServeでOfficeをtailnet内に公開する。loopbindバインドのまま、Tailscaleが自動でHTTPS終端する:

```bash
ssh root@<ip> "tailscale serve --bg 3777"
```

公開URLを確認:

```bash
ssh root@<ip> "tailscale serve status"
```

> ローカルPC側にもTailscaleが必要。未インストールの場合: https://tailscale.com/download

Office access URL: `https://<hostname>.tail*****.ts.net/office`

### Step 8: Firewall

```bash
ssh root@<ip> "ufw allow OpenSSH && ufw --force enable && ufw status"
```

Office (port 3777) is NOT opened in the firewall. Access is via Tailscale tailnet only.

### Step 9: Verify

```bash
ssh root@<ip> "clawpod status && clawpod health && curl -s http://127.0.0.1:3777/health"
```

### Step 10: Report

Present to user:

1. Server info (name, IP, specs)
2. ClawPod version
3. Service status
4. Tailscale hostname
5. Next steps:

```
Office access (recommended):

  https://<tailscale-hostname>/office

  Requires Tailscale on your local machine.
  Install: https://tailscale.com/download

Fallback (SSH tunnel):

  ssh -N -L 3777:127.0.0.1:3777 root@<ip>
  Then open http://localhost:3777/office

Useful commands on the server:

  clawpod status                # runtime status
  clawpod health                # health check
  clawpod logs --source events  # event log
  clawpod pairing list          # pending sender approvals
  clawpod pairing approve <code> # approve sender by code
  systemctl status clawpod      # service status
  systemctl restart clawpod     # restart
  journalctl -u clawpod -f      # live logs
```

## KasmVNC (Virtual Display + Browser Visualization)

エージェントがagent-browser経由で操作するChromeの画面をリモートから確認するため、KasmVNCをセットアップする。KasmVNCはVNCサーバー・Webクライアント・仮想ディスプレイを一体化したweb-nativeなリモートデスクトップ。

### KasmVNC Install

```bash
ssh root@<ip> "wget -q 'https://github.com/kasmtech/KasmVNC/releases/download/v1.3.4/kasmvncserver_noble_1.3.4_amd64.deb' -O /tmp/kasmvnc.deb && \
  apt-get install -y /tmp/kasmvnc.deb && \
  apt-get install -y ssl-cert"
```

### Google Chrome Install

```bash
ssh root@<ip> "wget -q -O /tmp/chrome.deb https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb && \
  apt-get install -y /tmp/chrome.deb"
```

### agent-browser Install

```bash
ssh root@<ip> "npm install -g agent-browser"
```

### VNC Password

ユーザーにVNCパスワードを確認してから設定する:

```bash
ssh root@<ip> "mkdir -p /root/.vnc && \
  echo -e '<password>\n<password>\n' | kasmvncpasswd -u root -w /root/.vnc/kasmpasswd && \
  cp /root/.vnc/kasmpasswd /root/.kasmpasswd && \
  chmod 600 /root/.kasmpasswd"
```

### KasmVNC Config

```bash
ssh root@<ip> "cat > /root/.vnc/kasmvnc.yaml << 'EOF'
desktop:
  resolution:
    width: 1280
    height: 720
  allow_resize: true

network:
  protocol: http
  websocket_port: 6901
  ssl:
    require_ssl: false
  udp:
    public_ip: auto

runtime_configuration:
  allow_client_to_override_kasm_server_settings: true
  allow_override_standard_vnc_server_settings: true

logging:
  log_writer_name: all
EOF"
```

### Desktop Environment

ユーザーに確認して分岐する:

- **A: XFCE**（推奨） — パネル・壁紙付きのフルデスクトップ。操作性重視。
- **B: openbox** — 最小限のWM。メモリ消費が最小でヘッドレス用途向け。

#### A: XFCE

```bash
ssh root@<ip> "apt-get install -y xfce4 xfce4-terminal dbus-x11"
```

```bash
ssh root@<ip> "cat > /root/.vnc/xstartup << 'EOF'
#!/bin/bash
export XDG_SESSION_TYPE=x11
startxfce4 &
EOF
chmod +x /root/.vnc/xstartup && \
  touch /root/.vnc/.de-was-selected"
```

#### B: openbox

```bash
ssh root@<ip> "apt-get install -y openbox"
```

```bash
ssh root@<ip> "cat > /root/.vnc/xstartup << 'EOF'
#!/bin/bash
export XDG_SESSION_TYPE=x11
openbox-session &
EOF
chmod +x /root/.vnc/xstartup && \
  touch /root/.vnc/.de-was-selected"
```

### KasmVNC systemd Service

```bash
ssh root@<ip> "cat > /etc/systemd/system/kasmvnc.service << 'EOF'
[Unit]
Description=KasmVNC virtual desktop
After=network.target

[Service]
Type=forking
User=root
ExecStartPre=/usr/bin/bash -c '/usr/bin/vncserver -kill :1 2>/dev/null || true'
ExecStart=/usr/bin/vncserver :1 -geometry 1280x720 -depth 24 -websocketPort 6901
ExecStop=/usr/bin/vncserver -kill :1
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload && systemctl enable kasmvnc.service && systemctl start kasmvnc.service"
```

確認:

```bash
ssh root@<ip> "systemctl status kasmvnc --no-pager && ss -tlnp | grep 6901"
```

`Active: active (running)` かつポート6901がLISTENしていれば成功。

### DISPLAY=:1 Integration with ClawPod

ClawPodのsystemdサービスに`DISPLAY=:1`を追加し、子プロセス（claude CLI → agent-browser → Chrome）がKasmVNCのXvncディスプレイに描画するようにする:

```bash
ssh root@<ip> "sed -i '/EnvironmentFile=/i Environment=DISPLAY=:1' /etc/systemd/system/clawpod.service && \
  systemctl daemon-reload && systemctl restart clawpod"
```

確認:

```bash
ssh root@<ip> "cat /proc/\$(pgrep -f 'clawpod daemon' | head -1)/environ | tr '\0' '\n' | grep DISPLAY"
```

`DISPLAY=:1` が表示されれば成功。

> **重要**: この設定をしないとagent-browserはChromeをヘッドレスで起動し、KasmVNC画面には何も表示されない。ClawPodが見ている画面とKasmVNCで見る画面を一致させるにはこの統合設定が必須。

> **Note**: KasmVNC（kasmvnc.service）はClawPodより先に起動している必要がある。問題発生時は `systemctl status kasmvnc` でDISPLAY :1の稼働を確認する。

### agent-browser Skill Deploy

エージェントのワークスペースにagent-browserスキルを配置する。agent-browserのSKILL.mdをai-workspaceリポジトリからコピーするか、手動で作成する:

```bash
ssh root@<ip> "mkdir -p /root/.clawpod/workspace/<agent>/sessions/<session>/.claude/skills/agent-browser/scripts"
```

> **Note**: agent-browserスキルのstart_chrome_cdp_profile.shは `.claude/skills/agent-browser/scripts/` に配置される必要がある。claude CLIのスキル探索は `.claude/skills/` を参照する。

### Tailscale Serve (Remote Access)

Tailscale Serveで6901ポートをtailnet内に公開する。Office(3777)と同じホスト名でポート違いでアクセス可能:

```bash
ssh root@<ip> "tailscale serve --bg --https 6901 http://localhost:6901"
```

確認:

```bash
ssh root@<ip> "tailscale serve status"
```

アクセス: `https://<tailscale-hostname>:6901`

ユーザー名: `root`、パスワード: 設定したVNCパスワード。

> **Fallback (SSH tunnel)**:
> ```
> ssh -N -L 6901:127.0.0.1:6901 root@<ip>
> ```
> ブラウザで `https://localhost:6901` を開く。

## KasmVNC (Virtual Display + Browser Visualization)

エージェントがagent-browser経由で操作するChromeの画面をリモートから確認するため、KasmVNCをセットアップする。KasmVNCはVNCサーバー・Webクライアント・仮想ディスプレイを一体化したweb-nativeなリモートデスクトップ。

### KasmVNC Install

```bash
ssh root@<ip> "wget -q 'https://github.com/kasmtech/KasmVNC/releases/download/v1.3.4/kasmvncserver_noble_1.3.4_amd64.deb' -O /tmp/kasmvnc.deb && \
  apt-get install -y /tmp/kasmvnc.deb && \
  apt-get install -y ssl-cert"
```

### Google Chrome Install

```bash
ssh root@<ip> "wget -q -O /tmp/chrome.deb https://dl.google.com/linux/direct/google-chrome-stable_current_amd64.deb && \
  apt-get install -y /tmp/chrome.deb"
```

### Node.js and agent-browser Install

Node.jsが未インストールの場合はインストールしてからagent-browserを入れる:

```bash
ssh root@<ip> "curl -fsSL https://deb.nodesource.com/setup_22.x | bash - && \
  apt-get install -y nodejs && \
  npm install -g agent-browser"
```

### VNC Password

ユーザーにVNCパスワードを確認してから設定する:

```bash
ssh root@<ip> "mkdir -p /root/.vnc && \
  echo -e '<password>\n<password>\n' | kasmvncpasswd -u root -w /root/.vnc/kasmpasswd && \
  cp /root/.vnc/kasmpasswd /root/.kasmpasswd && \
  chmod 600 /root/.kasmpasswd"
```

### KasmVNC Config

```bash
ssh root@<ip> "cat > /root/.vnc/kasmvnc.yaml << 'EOF'
desktop:
  resolution:
    width: 1280
    height: 720
  allow_resize: true

network:
  protocol: http
  websocket_port: 6901
  ssl:
    require_ssl: false
  udp:
    public_ip: auto

runtime_configuration:
  allow_client_to_override_kasm_server_settings: true
  allow_override_standard_vnc_server_settings: true

logging:
  log_writer_name: all
EOF"
```

### Desktop Environment

ユーザーに確認して分岐する:

- **A: XFCE**（推奨） — パネル・壁紙付きのフルデスクトップ。操作性重視。
- **B: openbox** — 最小限のWM。メモリ消費が最小でヘッドレス用途向け。

#### A: XFCE

```bash
ssh root@<ip> "apt-get install -y xfce4 xfce4-terminal dbus-x11"
```

```bash
ssh root@<ip> "cat > /root/.vnc/xstartup << 'EOF'
#!/bin/bash
export XDG_SESSION_TYPE=x11
startxfce4 &
EOF
chmod +x /root/.vnc/xstartup && \
  touch /root/.vnc/.de-was-selected"
```

#### B: openbox

```bash
ssh root@<ip> "apt-get install -y openbox"
```

```bash
ssh root@<ip> "cat > /root/.vnc/xstartup << 'EOF'
#!/bin/bash
export XDG_SESSION_TYPE=x11
openbox-session &
EOF
chmod +x /root/.vnc/xstartup && \
  touch /root/.vnc/.de-was-selected"
```

### KasmVNC systemd Service

```bash
ssh root@<ip> "cat > /etc/systemd/system/kasmvnc.service << 'EOF'
[Unit]
Description=KasmVNC virtual desktop
After=network.target

[Service]
Type=forking
User=root
ExecStartPre=/usr/bin/bash -c '/usr/bin/vncserver -kill :1 2>/dev/null || true'
ExecStart=/usr/bin/vncserver :1 -geometry 1280x720 -depth 24 -websocketPort 6901
ExecStop=/usr/bin/vncserver -kill :1
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
systemctl daemon-reload && systemctl enable kasmvnc.service && systemctl start kasmvnc.service"
```

確認:

```bash
ssh root@<ip> "systemctl status kasmvnc --no-pager && ss -tlnp | grep 6901"
```

`Active: active (running)` かつポート6901がLISTENしていれば成功。

### DISPLAY=:1 Integration with ClawPod

ClawPodのsystemdサービスに`DISPLAY=:1`を追加し、子プロセス（claude CLI → agent-browser → Chrome）がKasmVNCのXvncディスプレイに描画するようにする:

```bash
ssh root@<ip> "sed -i '/EnvironmentFile=/i Environment=DISPLAY=:1' /etc/systemd/system/clawpod.service && \
  systemctl daemon-reload && systemctl restart clawpod"
```

確認:

```bash
ssh root@<ip> "cat /proc/\$(pgrep -f 'clawpod daemon' | head -1)/environ | tr '\0' '\n' | grep DISPLAY"
```

`DISPLAY=:1` が表示されれば成功。

> **重要**: この設定をしないとagent-browserはChromeをヘッドレスで起動し、KasmVNC画面には何も表示されない。ClawPodが見ている画面とKasmVNCで見る画面を一致させるにはこの統合設定が必須。

> **Note**: KasmVNC（kasmvnc.service）はClawPodより先に起動している必要がある。問題発生時は `systemctl status kasmvnc` でDISPLAY :1の稼働を確認する。

### agent-browser Skill Deploy

エージェントのワークスペースにagent-browserスキルを配置する。agent-browserのSKILL.mdをai-workspaceリポジトリからコピーするか、手動で作成する:

```bash
ssh root@<ip> "mkdir -p /root/.clawpod/workspace/<agent>/sessions/<session>/.claude/skills/agent-browser/scripts"
```

> **Note**: agent-browserスキルのstart_chrome_cdp_profile.shは `.claude/skills/agent-browser/scripts/` に配置される必要がある。claude CLIのスキル探索は `.claude/skills/` を参照する。

### Tailscale Serve (Remote Access)

Tailscale Serveで6901ポートをtailnet内に公開する。Office(3777)と同じホスト名でポート違いでアクセス可能:

```bash
ssh root@<ip> "tailscale serve --bg --https 6901 http://localhost:6901"
```

確認:

```bash
ssh root@<ip> "tailscale serve status"
```

アクセス: `https://<tailscale-hostname>:6901`

ユーザー名: `root`、パスワード: 設定したVNCパスワード。

> **Fallback (SSH tunnel)**:
> ```
> ssh -N -L 6901:127.0.0.1:6901 root@<ip>
> ```
> ブラウザで `https://localhost:6901` を開く。

## Update Workflow

For subsequent updates after initial deployment:

```bash
ssh root@<ip> "source /root/.cargo/env && \
  cd /opt/clawpod-src && git pull && \
  cargo build --release -p runtime && \
  systemctl stop clawpod && \
  cp target/release/clawpod /usr/local/bin/clawpod && \
  systemctl start clawpod && \
  clawpod --help | head -1 && systemctl status clawpod --no-pager"
```

## Troubleshooting

| Issue | Root Cause | Solution |
|-------|-----------|----------|
| Build fails: linker errors | Missing build-essential/libssl-dev | `apt-get install -y build-essential pkg-config libssl-dev` |
| Build fails: out of memory | cpx22 (4GB) insufficient for LTO build | Build with `--jobs 1` or upgrade to cpx32 |
| Service won't start | Missing env file or bad TOML | Check `journalctl -u clawpod -f` and validate TOML |
| Office unreachable via Tailscale | Tailscale not authenticated or serve not running | `tailscale status` and `tailscale serve status` |
| Slack not connecting | Missing or invalid bot/app tokens | Check env file, verify Socket Mode is enabled in Slack app |
| `clawpod status` shows no agents | Config not loaded | Verify `/root/.clawpod/clawpod.toml` exists and is valid |
| Permission denied on workspace | Running as wrong user | Ensure service runs as same user who owns `~/.clawpod` |
| Tailscale serve fails | Port already in use or tailscaled not running | `systemctl status tailscaled` and `ss -tlnp | grep 3777` |

## Backup

```bash
ssh root@<ip> "mkdir -p /root/backups && \
  tar czf /root/backups/clawpod_$(date +%Y%m%d).tar.gz \
    -C /root/.clawpod \
    clawpod.toml state/ logs/ workspace/"
```

## Fallback: SSH Tunnel (without Tailscale)

If Tailscale is not available, use SSH tunnel for Office access:

```bash
ssh -N -L 3777:127.0.0.1:3777 root@<ip>
```

Then open `http://localhost:3777/office` in your browser.

For convenience, add to `~/.ssh/config`:

```
Host clawpod
    HostName <ip>
    User root
    LocalForward 3777 127.0.0.1:3777
```

Then `ssh clawpod` opens the tunnel.
