#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  start_chrome_cdp_profile.sh \
    [--port <9400>] \
    [--port-base <9400>] \
    [--port-max <9499>] \
    [--profile-prefix </tmp/nasuy-debug-profile>] \
    [--profile-dir </tmp/nasuy-debug-profile-9400>] \
    [--chrome-bin </Applications/Google Chrome.app/Contents/MacOS/Google Chrome>] \
    [--open-url <https://x.com/home>] \
    [--wait-ms <12000>] \
    [--foreground] \
    [--no-reuse]

Behavior:
- Scans the port range for an existing Chrome CDP instance. If found, reuses it.
- If no existing instance is found, launches a new Chrome.
- If --port is set, checks that specific port first.
- Use --no-reuse to always launch a new instance (skips detection).
- Print key=value lines so downstream scripts can parse cdp_port/profile_dir/chrome_pid/reused.
USAGE
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing command: $1" >&2
    exit 1
  fi
}

is_listening() {
  local port="$1"
  lsof -n -P -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1
}

is_cdp_ready() {
  local port="$1"
  curl -fsS --max-time 2 "http://localhost:${port}/json/version" >/dev/null 2>&1
}

PORT_FIXED=""
PORT_BASE=9400
PORT_MAX=9499
PROFILE_PREFIX="/tmp/nasuy-debug-profile"
PROFILE_DIR_OVERRIDE=""
CHROME_BIN="${CHROME_BIN:-$(command -v google-chrome 2>/dev/null || echo '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome')}"
OPEN_URL="about:blank"
WAIT_MS=12000
FOREGROUND=0
NO_REUSE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --port)
      PORT_FIXED="${2:-}"
      shift 2
      ;;
    --port-base)
      PORT_BASE="${2:-}"
      shift 2
      ;;
    --port-max)
      PORT_MAX="${2:-}"
      shift 2
      ;;
    --profile-prefix)
      PROFILE_PREFIX="${2:-}"
      shift 2
      ;;
    --profile-dir)
      PROFILE_DIR_OVERRIDE="${2:-}"
      shift 2
      ;;
    --chrome-bin)
      CHROME_BIN="${2:-}"
      shift 2
      ;;
    --open-url)
      OPEN_URL="${2:-}"
      shift 2
      ;;
    --wait-ms)
      WAIT_MS="${2:-}"
      shift 2
      ;;
    --foreground)
      FOREGROUND=1
      shift
      ;;
    --no-reuse)
      NO_REUSE=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown arg: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ -n "$PORT_FIXED" ]]; then
  if ! [[ "$PORT_FIXED" =~ ^[0-9]+$ ]]; then
    echo "--port must be an integer" >&2
    exit 1
  fi
elif ! [[ "$PORT_BASE" =~ ^[0-9]+$ && "$PORT_MAX" =~ ^[0-9]+$ ]]; then
  echo "--port-base/--port-max must be integers" >&2
  exit 1
fi

if (( PORT_BASE > PORT_MAX )); then
  echo "--port-base must be <= --port-max" >&2
  exit 1
fi

if ! [[ "$WAIT_MS" =~ ^[0-9]+$ ]]; then
  echo "--wait-ms must be an integer milliseconds" >&2
  exit 1
fi

if [[ ! -x "$CHROME_BIN" ]]; then
  echo "Chrome binary is not executable: $CHROME_BIN" >&2
  exit 1
fi

require_cmd lsof
require_cmd curl

# Extra Chrome flags for Linux / running as root
EXTRA_CHROME_FLAGS=()
if [[ "$(uname -s)" == "Linux" ]]; then
  EXTRA_CHROME_FLAGS+=(--disable-dev-shm-usage)
  if [[ "$(id -u)" == "0" ]]; then
    EXTRA_CHROME_FLAGS+=(--no-sandbox)
  fi
fi

if [[ -n "$PROFILE_DIR_OVERRIDE" && -z "$PORT_FIXED" ]]; then
  echo "--profile-dir requires --port to avoid shared profile collisions." >&2
  exit 1
fi

# --- Phase 1: Detect existing CDP Chrome ---
REUSED_PORT=""
if [[ "$NO_REUSE" == "0" ]]; then
  if [[ -n "$PORT_FIXED" ]]; then
    # Check the specific port
    if is_cdp_ready "$PORT_FIXED"; then
      REUSED_PORT="$PORT_FIXED"
    fi
  else
    # Scan the range for an existing CDP instance
    for ((port=PORT_BASE; port<=PORT_MAX; port++)); do
      if is_cdp_ready "$port"; then
        REUSED_PORT="$port"
        break
      fi
    done
  fi
fi

# --- Phase 1b: Reuse existing Chrome ---
if [[ -n "$REUSED_PORT" ]]; then
  REUSED_PID=""
  REUSED_PID=$(lsof -n -P -iTCP:"$REUSED_PORT" -sTCP:LISTEN -t 2>/dev/null | head -1) || true

  if [[ -n "$PROFILE_DIR_OVERRIDE" ]]; then
    REUSED_PROFILE="$PROFILE_DIR_OVERRIDE"
  else
    REUSED_PROFILE="${PROFILE_PREFIX}-${REUSED_PORT}"
  fi

  echo "cdp_port=$REUSED_PORT"
  echo "cdp_url=http://localhost:${REUSED_PORT}"
  echo "profile_dir=$REUSED_PROFILE"
  echo "chrome_pid=${REUSED_PID:-unknown}"
  echo "log_file="
  echo "cdp_ready=true"
  echo "reused=true"
  echo "agent_browser_connect=agent-browser --cdp ${REUSED_PORT}"
  exit 0
fi

# --- Phase 2: Launch new Chrome ---
PICKED_PORT=""
if [[ -n "$PORT_FIXED" ]]; then
  if is_listening "$PORT_FIXED"; then
    echo "Port $PORT_FIXED is in use but not responding to CDP." >&2
    exit 1
  fi
  PICKED_PORT="$PORT_FIXED"
else
  for ((port=PORT_BASE; port<=PORT_MAX; port++)); do
    if ! is_listening "$port"; then
      PICKED_PORT="$port"
      break
    fi
  done
fi

if [[ -z "$PICKED_PORT" ]]; then
  echo "No available port in range ${PORT_BASE}-${PORT_MAX}" >&2
  exit 1
fi

if [[ -n "$PROFILE_DIR_OVERRIDE" ]]; then
  PROFILE_DIR="$PROFILE_DIR_OVERRIDE"
else
  PROFILE_DIR="${PROFILE_PREFIX}-${PICKED_PORT}"
fi

mkdir -p "$PROFILE_DIR"

if [[ "$FOREGROUND" == "1" ]]; then
  echo "cdp_port=$PICKED_PORT"
  echo "cdp_url=http://localhost:${PICKED_PORT}"
  echo "profile_dir=$PROFILE_DIR"
  echo "reused=false"
  exec "$CHROME_BIN" \
    --remote-debugging-port="$PICKED_PORT" \
    --user-data-dir="$PROFILE_DIR" \
    "${EXTRA_CHROME_FLAGS[@]}" \
    "$OPEN_URL"
fi

LOG_DIR="${PROFILE_DIR}/logs"
mkdir -p "$LOG_DIR"
LOG_FILE="${LOG_DIR}/chrome_cdp_${PICKED_PORT}.log"

nohup "$CHROME_BIN" \
  --remote-debugging-port="$PICKED_PORT" \
  --user-data-dir="$PROFILE_DIR" \
  "${EXTRA_CHROME_FLAGS[@]}" \
  "$OPEN_URL" \
  >"$LOG_FILE" 2>&1 &
CHROME_PID=$!

DEADLINE_MS="$WAIT_MS"
STEP_MS=300
ELAPSED=0
READY=0
while (( ELAPSED <= DEADLINE_MS )); do
  if curl -fsS "http://localhost:${PICKED_PORT}/json/version" >/dev/null 2>&1; then
    READY=1
    break
  fi
  if ! kill -0 "$CHROME_PID" >/dev/null 2>&1; then
    break
  fi
  sleep 0.3
  ELAPSED=$(( ELAPSED + STEP_MS ))
done

echo "cdp_port=$PICKED_PORT"
echo "cdp_url=http://localhost:${PICKED_PORT}"
echo "profile_dir=$PROFILE_DIR"
echo "chrome_pid=$CHROME_PID"
echo "log_file=$LOG_FILE"
if [[ "$READY" == "1" ]]; then
  echo "cdp_ready=true"
else
  echo "cdp_ready=false"
fi
echo "reused=false"
echo "agent_browser_connect=agent-browser --cdp ${PICKED_PORT}"
