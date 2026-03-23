# CDP Profile (Chrome DevTools Protocol)

Launch Chrome with `--remote-debugging-port` and isolated `--user-data-dir`, then connect agent-browser via CDP. Useful for persistent login state, parallel workers, and debugging with a real Chrome instance.

**Related**: [commands.md](commands.md) for `--cdp` flag details, [session-management.md](session-management.md) for session isolation, [SKILL.md](../SKILL.md) for quick start.

## Contents

- [When to Use](#when-to-use)
- [Setup Script](#setup-script)
- [Connecting agent-browser](#connecting-agent-browser)
- [Fixed Port / Fixed Profile](#fixed-port--fixed-profile)
- [Parallel Workers](#parallel-workers)
- [Shell Alias](#shell-alias)
- [Troubleshooting](#troubleshooting)

## When to Use

- **Persistent login**: Keep cookies/localStorage across restarts by reusing the same `--user-data-dir`
- **Port collision avoidance**: Auto-pick an available port in a range instead of hardcoding
- **Parallel execution**: Each worker gets a separate port + profile, no conflicts
- **Manual 2FA / extension-based auth**: Login manually in the headed Chrome first, then automate

## Setup Script

`scripts/start_chrome_cdp_profile.sh` launches Chrome with an available CDP port and isolated profile.

```bash
# Auto-pick port in range 9400-9499, create isolated profile
.agents/skills/agent-browser/scripts/start_chrome_cdp_profile.sh \
  --port-base 9400 \
  --port-max 9499 \
  --profile-prefix /tmp/cdp-profile \
  --open-url https://x.com/home
```

Output (key=value for parsing):

```text
cdp_port=9404
cdp_url=http://localhost:9404
profile_dir=/tmp/cdp-profile-9404
chrome_pid=12345
log_file=/tmp/cdp-profile-9404/logs/chrome_cdp_9404.log
cdp_ready=true
agent_browser_connect=agent-browser --cdp 9404
```

### Script Options

| Flag | Default | Description |
|------|---------|-------------|
| `--port <N>` | - | Use a fixed port (fails if in use) |
| `--port-base <N>` | 9400 | Start of auto-pick range |
| `--port-max <N>` | 9499 | End of auto-pick range |
| `--profile-prefix <path>` | /tmp/nasuy-debug-profile | Profile dir prefix (appends `-<port>`) |
| `--profile-dir <path>` | - | Explicit profile dir (requires `--port`) |
| `--chrome-bin <path>` | /Applications/Google Chrome.app/... | Chrome executable |
| `--open-url <url>` | about:blank | URL to open on launch |
| `--wait-ms <N>` | 12000 | Max wait for CDP ready (ms) |
| `--foreground` | - | Run Chrome in foreground (no background daemon) |

## Connecting agent-browser

```bash
CDP_PORT=9404

agent-browser --cdp "$CDP_PORT" open https://x.com/home
agent-browser --cdp "$CDP_PORT" snapshot -i
agent-browser --cdp "$CDP_PORT" click @e1
```

Or use `--auto-connect` to discover a running Chrome automatically:

```bash
agent-browser --auto-connect open https://example.com
agent-browser --auto-connect snapshot -i
```

## Fixed Port / Fixed Profile

When you want a stable port and profile directory:

```bash
.agents/skills/agent-browser/scripts/start_chrome_cdp_profile.sh \
  --port 9400 \
  --profile-dir /tmp/my-chrome-profile \
  --open-url https://x.com/home
```

## Parallel Workers

Each worker uses a different port. The script auto-creates `<prefix>-<port>` profile dirs.

```bash
# Worker 1
.agents/skills/agent-browser/scripts/start_chrome_cdp_profile.sh --port 9400 --open-url https://site-a.com
agent-browser --cdp 9400 snapshot -i

# Worker 2
.agents/skills/agent-browser/scripts/start_chrome_cdp_profile.sh --port 9401 --open-url https://site-b.com
agent-browser --cdp 9401 snapshot -i

# Worker 3
.agents/skills/agent-browser/scripts/start_chrome_cdp_profile.sh --port 9402 --open-url https://site-c.com
agent-browser --cdp 9402 snapshot -i
```

Verify CDP readiness before automating:

```bash
curl -s http://localhost:9400/json/version
```

## Shell Alias

Avoid repeating `--cdp` on every command:

```bash
CDP_PORT=9404
ab() { agent-browser --cdp "$CDP_PORT" --session "cdp-$CDP_PORT" "$@"; }

ab open https://x.com/home
ab snapshot -i
ab click @e1
ab close
```

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `No page found` | Add `--open-url` to ensure at least one tab is open |
| `cdp_ready=false` | Check `log_file` output. Wait a few seconds and retry |
| Login required | Launch with `--headed` or `--foreground`, complete login/2FA manually, then automate |
| Port already in use | Use `--port-base`/`--port-max` for auto-pick, or pick a different `--port` |
| Profile collision | Use `--profile-prefix` (auto-appends port) or explicit `--profile-dir` per worker |
