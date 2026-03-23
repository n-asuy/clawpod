#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  run_x_daily_post_collection.sh \
    --accounts-dir <crm/accounts> \
    --owner <x-handle> \
    [--date <YYYY-MM-DD>] \
    [--cdp-url <http://localhost:9300>] \
    [--no-cdp-connect] \
    [--skip-existing|--no-skip-existing] \
    [--session-name <name>] \
    [--max-duration-sec <seconds>] \
    [--command-timeout-sec <seconds>]

Example:
  .agents/skills/x-daily-post-collector/scripts/run_x_daily_post_collection.sh \
    --accounts-dir "24_SNS_X/n_asuy/crm/accounts" \
    --owner "n_asuy" \
    --date "2026-03-07" \
    --no-cdp-connect
USAGE
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing command: $1" >&2
    exit 1
  fi
}

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../../.." && pwd)"
X_FOLLOW_SCRIPTS_DIR="${REPO_ROOT}/.agents/skills/x-follow-analysis/scripts"

ACCOUNTS_DIR=""
OWNER=""
DATE_RAW=""
CDP_URL="http://localhost:9300"
NO_CDP_CONNECT=0
SKIP_EXISTING=1
SESSION_NAME=""
MAX_DURATION_SEC=5400
COMMAND_TIMEOUT_SEC=45

while [[ $# -gt 0 ]]; do
  case "$1" in
    --accounts-dir)
      ACCOUNTS_DIR="${2:-}"
      shift 2
      ;;
    --owner)
      OWNER="${2:-}"
      shift 2
      ;;
    --date)
      DATE_RAW="${2:-}"
      shift 2
      ;;
    --cdp-url)
      CDP_URL="${2:-}"
      shift 2
      ;;
    --no-cdp-connect)
      NO_CDP_CONNECT=1
      shift
      ;;
    --skip-existing)
      SKIP_EXISTING=1
      shift
      ;;
    --no-skip-existing)
      SKIP_EXISTING=0
      shift
      ;;
    --session-name)
      SESSION_NAME="${2:-}"
      shift 2
      ;;
    --max-duration-sec)
      MAX_DURATION_SEC="${2:-}"
      shift 2
      ;;
    --command-timeout-sec)
      COMMAND_TIMEOUT_SEC="${2:-}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ -z "$ACCOUNTS_DIR" || -z "$OWNER" ]]; then
  echo "--accounts-dir and --owner are required." >&2
  usage
  exit 1
fi

require_cmd python3

if [[ -n "$DATE_RAW" ]]; then
  TARGET_DATE="$DATE_RAW"
else
  TARGET_DATE="$(
    python3 -c 'import datetime as dt; tz=dt.timezone(dt.timedelta(hours=9)); print(dt.datetime.now(tz).date().isoformat())'
  )"
fi

TARGET_TAG="$(
  python3 -c 'import datetime as dt, sys; print(dt.date.fromisoformat(sys.argv[1]).strftime("%Y%m%d"))' "$TARGET_DATE"
)"

COLLECT_CMD=(
  python3
  "${X_FOLLOW_SCRIPTS_DIR}/collect_x_today_posts.py"
  --accounts-dir "$ACCOUNTS_DIR"
  --owner "$OWNER"
  --date "$TARGET_DATE"
  --translation-mode auto
  --max-duration-sec "$MAX_DURATION_SEC"
  --command-timeout-sec "$COMMAND_TIMEOUT_SEC"
)

if [[ "$NO_CDP_CONNECT" == "1" ]]; then
  COLLECT_CMD+=(--no-cdp-connect)
else
  COLLECT_CMD+=(--cdp-url "$CDP_URL")
fi

if [[ "$SKIP_EXISTING" == "1" ]]; then
  COLLECT_CMD+=(--skip-existing)
fi

if [[ -n "$SESSION_NAME" ]]; then
  COLLECT_CMD+=(--session-name "$SESSION_NAME")
fi

echo "# collect_x_today_posts"
"${COLLECT_CMD[@]}"

echo "# build_x_daily_summary --require-all"
SUMMARY_OUT="$(
  python3 "${X_FOLLOW_SCRIPTS_DIR}/build_x_daily_summary.py" \
    --accounts-dir "$ACCOUNTS_DIR" \
    --owner "$OWNER" \
    --date "$TARGET_DATE" \
    --require-all
)"
printf "%s\n" "$SUMMARY_OUT"

SUMMARY_FILE="$(printf "%s\n" "$SUMMARY_OUT" | awk -F= '/^summary_file=/{print $2}')"
if [[ -z "$SUMMARY_FILE" ]]; then
  SUMMARY_FILE="$(dirname "$ACCOUNTS_DIR")/daily_post_reports/${TARGET_TAG}_summary.md"
fi

echo "# check_x_translation_completion"
python3 "${X_FOLLOW_SCRIPTS_DIR}/check_x_translation_completion.py" \
  --summary-file "$SUMMARY_FILE"

echo "target_date=$TARGET_DATE"
echo "summary_file=$SUMMARY_FILE"
