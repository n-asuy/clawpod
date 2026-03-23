#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  collect_profile_raw.sh \
    --target-dir <profile-dir> \
    --platform <x|web> \
    --id <identifier> \
    --url <url> \
    [--cdp-url <http://localhost:9222>] \
    [--wait-ms <3000>] \
    [--scroll-steps <24>] \
    [--session-name <collector-session>] \
    [--max-duration-sec <600>]

Example:
  collect_profile_raw.sh \
    --target-dir "02_全社_競合・ベンチマーク/プロファイル/jasonzhou1993" \
    --platform x \
    --id jasonzhou1993 \
    --url "https://x.com/jasonzhou1993" \
    --max-duration-sec 480
USAGE
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing command: $1" >&2
    exit 1
  fi
}

clean_tsv() {
  tr '\t\r\n' '   '
}

sanitize_token() {
  printf "%s" "$1" | tr -cs 'A-Za-z0-9_-' '-'
}

now_epoch() {
  date +%s
}

TARGET_DIR=""
PLATFORM=""
ID=""
URL=""
CDP_URL="http://localhost:9222"
WAIT_MS="3000"
SCROLL_STEPS="24"
SESSION_NAME=""
MAX_DURATION_SEC="600"
AB_SESSION_ACTIVE="0"
DEADLINE_TS="0"
TMP_TEXT=""
TMP_POSTS=""

ab() {
  agent-browser --session "$SESSION_NAME" "$@"
}

check_deadline() {
  if [[ "$MAX_DURATION_SEC" == "0" ]]; then
    return 0
  fi
  if [[ "$(now_epoch)" -ge "$DEADLINE_TS" ]]; then
    echo "Collection timed out after ${MAX_DURATION_SEC}s (--max-duration-sec)." >&2
    exit 124
  fi
}

cleanup() {
  local ec=$?
  trap - EXIT INT TERM
  if [[ -n "$TMP_TEXT" || -n "$TMP_POSTS" ]]; then
    rm -f "$TMP_TEXT" "$TMP_POSTS"
  fi
  if [[ "$AB_SESSION_ACTIVE" == "1" ]]; then
    ab close >/dev/null 2>&1 || true
  fi
  exit "$ec"
}

trap cleanup EXIT INT TERM

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target-dir)
      TARGET_DIR="${2:-}"
      shift 2
      ;;
    --platform)
      PLATFORM="${2:-}"
      shift 2
      ;;
    --id)
      ID="${2:-}"
      shift 2
      ;;
    --url)
      URL="${2:-}"
      shift 2
      ;;
    --cdp-url)
      CDP_URL="${2:-}"
      shift 2
      ;;
    --wait-ms)
      WAIT_MS="${2:-}"
      shift 2
      ;;
    --scroll-steps)
      SCROLL_STEPS="${2:-}"
      shift 2
      ;;
    --session-name)
      SESSION_NAME="${2:-}"
      shift 2
      ;;
    --max-duration-sec)
      MAX_DURATION_SEC="${2:-}"
      shift 2
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

if [[ -z "$TARGET_DIR" || -z "$PLATFORM" || -z "$ID" || -z "$URL" ]]; then
  echo "Required arguments are missing." >&2
  usage
  exit 1
fi

if [[ "$PLATFORM" != "x" && "$PLATFORM" != "web" ]]; then
  echo "--platform must be 'x' or 'web'" >&2
  exit 1
fi

if ! [[ "$WAIT_MS" =~ ^[0-9]+$ ]]; then
  echo "--wait-ms must be an integer (milliseconds)" >&2
  exit 1
fi

if ! [[ "$SCROLL_STEPS" =~ ^[0-9]+$ ]]; then
  echo "--scroll-steps must be an integer" >&2
  exit 1
fi

if ! [[ "$MAX_DURATION_SEC" =~ ^[0-9]+$ ]]; then
  echo "--max-duration-sec must be an integer seconds (0 disables timeout)" >&2
  exit 1
fi

if [[ -z "$SESSION_NAME" ]]; then
  SESSION_NAME="cpm-$(sanitize_token "${PLATFORM}-${ID}")-$(now_epoch)-$$"
fi

require_cmd agent-browser

mkdir -p "$TARGET_DIR/raw" "$TARGET_DIR/log"

TS="$(date +%Y%m%d_%H%M)"
COLLECTED_AT="$(date +%Y-%m-%dT%H:%M:%S%z)"
RAW_FILE="$TARGET_DIR/raw/${TS}_${PLATFORM}_${ID}_raw.md"
SHOT_FILE="$TARGET_DIR/log/${TS}_${PLATFORM}_${ID}.png"
MANIFEST="$TARGET_DIR/log/manifest.tsv"
TMP_TEXT="$(mktemp)"
TMP_POSTS="$(mktemp)"
if [[ "$MAX_DURATION_SEC" != "0" ]]; then
  DEADLINE_TS="$(( $(now_epoch) + MAX_DURATION_SEC ))"
fi

SHOT_DIR_ABS="$(cd "$(dirname "$SHOT_FILE")" && pwd)"
SHOT_FILE_ABS="$SHOT_DIR_ABS/$(basename "$SHOT_FILE")"

AB_SESSION_ACTIVE="1"
check_deadline
ab connect "$CDP_URL"
check_deadline
ab open "$URL"
check_deadline
ab wait "$WAIT_MS"

RESOLVED_URL="$(ab get url || true)"
TITLE="$(ab get title || true)"
ab screenshot "$SHOT_FILE_ABS"

if [[ "$PLATFORM" == "x" ]]; then
  # Initialize in-page buffers so we can aggregate posts across many scroll steps.
  check_deadline
  cat <<'EOF' | ab eval --stdin >/dev/null
(() => {
  window.__cpmSeen = {};
  window.__cpmRows = [];
  return true;
})();
EOF

  for ((i=0; i<=SCROLL_STEPS; i++)); do
    check_deadline
    cat <<EOF | ab eval --stdin >/dev/null
(() => {
  const targetId = "${ID}".toLowerCase();
  const seen = window.__cpmSeen || (window.__cpmSeen = {});
  const rows = window.__cpmRows || (window.__cpmRows = []);

  const normalizeStatus = (href) => {
    if (!href) return null;
    const match = href.match(/^https:\/\/x\.com\/([^\/]+)\/status\/(\d+)/i);
    if (!match) return null;
    return {
      account: match[1],
      accountLower: match[1].toLowerCase(),
      statusId: match[2],
      url: \`https://x.com/\${match[1]}/status/\${match[2]}\`,
    };
  };

  const visibleTweets = Array.from(document.querySelectorAll('article[data-testid="tweet"]'));
  for (const article of visibleTweets) {
    const linkEl = article.querySelector('a[href*="/status/"]');
    const normalized = normalizeStatus(linkEl ? linkEl.href : '');
    if (!normalized) continue;
    if (normalized.accountLower !== targetId) continue;
    if (seen[normalized.statusId]) continue;
    seen[normalized.statusId] = true;

    const timeEl = article.querySelector('time');
    const text = Array.from(article.querySelectorAll('[data-testid="tweetText"]'))
      .map((node) => node.innerText.trim())
      .filter(Boolean)
      .join('\n')
      .trim();
    const socialContext = article.querySelector('[data-testid="socialContext"]');
    const metricsLabels = Array.from(article.querySelectorAll('[aria-label]'))
      .map((node) => node.getAttribute('aria-label'))
      .filter(Boolean);

    rows.push({
      account: normalized.account,
      status_id: normalized.statusId,
      datetime: timeEl ? (timeEl.getAttribute('datetime') || '') : '',
      url: normalized.url,
      text,
      social_context: socialContext ? socialContext.innerText.trim() : '',
      has_quote: !!article.querySelector('[data-testid="quoteTweet"]'),
      has_media: !!article.querySelector('[data-testid="tweetPhoto"], [data-testid="videoPlayer"], [data-testid="card.wrapper"]'),
      metrics_labels: metricsLabels
    });
  }

  return rows.length;
})();
EOF
    if [[ "$i" -lt "$SCROLL_STEPS" ]]; then
      check_deadline
      ab scroll down 1700 >/dev/null || true
      check_deadline
      ab wait 900 >/dev/null || true
    fi
  done

  check_deadline
  cat <<EOF | ab eval --stdin >"$TMP_POSTS"
(() => window.__cpmRows || [])();
EOF

  SUMMARY="$(
    python3 .agents/skills/competitor-profile-maintenance/scripts/render_x_archive.py \
      --input-json "$TMP_POSTS" \
      --output "$RAW_FILE" \
      --username "$ID" \
      --source-url "$URL" \
      --resolved-url "$RESOLVED_URL" \
      --title "$TITLE" \
      --collected-at "$COLLECTED_AT" \
      --screenshot-file "$(basename "$SHOT_FILE")"
  )"
  POST_COUNT="$(printf "%s\n" "$SUMMARY" | awk -F= '/^post_count=/{print $2}')"

  if [[ -z "$POST_COUNT" || "$POST_COUNT" == "0" ]]; then
    echo "No posts were extracted from X timeline. Check login/session and rerun." >&2
    exit 1
  fi
else
  check_deadline
  ab get text body >"$TMP_TEXT"
  cat >"$RAW_FILE" <<EOF
---
collected_at: $COLLECTED_AT
platform: $PLATFORM
id: $ID
source_url: $URL
resolved_url: $RESOLVED_URL
title: $TITLE
collector: agent-browser
---

# Raw Capture

- id: $ID
- platform: $PLATFORM
- source_url: $URL
- resolved_url: $RESOLVED_URL
- title: $TITLE
- screenshot: $(basename "$SHOT_FILE")

## Text

\`\`\`text
$(cat "$TMP_TEXT")
\`\`\`
EOF
fi

if [[ ! -f "$MANIFEST" ]]; then
  echo -e "timestamp\tplatform\tid\tsource_url\tresolved_url\ttitle\traw_file\tscreenshot_file" >"$MANIFEST"
fi

printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" \
  "$COLLECTED_AT" \
  "$PLATFORM" \
  "$ID" \
  "$(printf "%s" "$URL" | clean_tsv)" \
  "$(printf "%s" "$RESOLVED_URL" | clean_tsv)" \
  "$(printf "%s" "$TITLE" | clean_tsv)" \
  "$RAW_FILE" \
  "$SHOT_FILE" \
  >>"$MANIFEST"

echo "raw_file=$RAW_FILE"
echo "screenshot_file=$SHOT_FILE"
echo "manifest_file=$MANIFEST"
