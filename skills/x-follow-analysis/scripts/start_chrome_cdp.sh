#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  start_chrome_cdp.sh \
    [--port-base <9300>] \
    [--port-max <9399>] \
    [--profile-prefix </tmp/nasuy-debug-profile>] \
    [--chrome-bin </Applications/Google Chrome.app/Contents/MacOS/Google Chrome>] \
    [--open-url <https://x.com/home>] \
    [--wait-ms <12000>] \
    [--foreground]

Behavior:
- Finds the first available port in [--port-base, --port-max].
- Launches Chrome with:
  --remote-debugging-port=<picked port>
  --user-data-dir=<profile-prefix>-<picked port>
- Prints key=value lines for downstream scripts.
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

PORT_BASE=9300
PORT_MAX=9399
PROFILE_PREFIX="/tmp/nasuy-debug-profile"
CHROME_BIN="${CHROME_BIN:-$(command -v google-chrome 2>/dev/null || command -v chromium-browser 2>/dev/null || command -v chromium 2>/dev/null || echo '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome')}"
OPEN_URL="https://x.com/home"
WAIT_MS=12000
FOREGROUND=0

while [[ $# -gt 0 ]]; do
  case "$1" in
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

if ! [[ "$PORT_BASE" =~ ^[0-9]+$ && "$PORT_MAX" =~ ^[0-9]+$ ]]; then
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

if [[ ! -x "$CHROME_BIN" ]]; then
  echo "Chrome binary not executable: $CHROME_BIN" >&2
  exit 1
fi

PICKED_PORT=""
for ((port=PORT_BASE; port<=PORT_MAX; port++)); do
  if ! is_listening "$port"; then
    PICKED_PORT="$port"
    break
  fi
done

if [[ -z "$PICKED_PORT" ]]; then
  echo "No available port in range ${PORT_BASE}-${PORT_MAX}" >&2
  exit 1
fi

PROFILE_DIR="${PROFILE_PREFIX}-${PICKED_PORT}"
mkdir -p "$PROFILE_DIR"

if [[ "$FOREGROUND" == "1" ]]; then
  echo "cdp_port=$PICKED_PORT"
  echo "cdp_url=http://localhost:${PICKED_PORT}"
  echo "profile_dir=$PROFILE_DIR"
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

DEADLINE_MS=$(( WAIT_MS ))
STEP_MS=300
ELAPSED=0
READY=0
while (( ELAPSED <= DEADLINE_MS )); do
  if curl -fsS "http://localhost:${PICKED_PORT}/json/version" >/dev/null 2>&1; then
    READY=1
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
