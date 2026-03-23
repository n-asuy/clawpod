#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage:
  collect_x_following_raw.sh \
    --target-dir <analysis-dir> \
    --username <x-handle> \
    [--url <https://x.com/<handle>/following>] \
    [--cdp-url <http://localhost:9300>] \
    [--wait-ms <3000>] \
    [--scroll-steps <45>] \
    [--scroll-px <1700>] \
    [--session-name <collector-session>] \
    [--max-duration-sec <600>] \
    [--input-json <pre-collected.json>] \
    [--source <cdp|apify>]

When --input-json is provided, CDP collection is skipped and the given
JSON file is used directly. This allows Apify-sourced data (normalized
via normalize_apify_following.py) to enter the same pipeline.

Example (CDP):
  collect_x_following_raw.sh \
    --target-dir "23_SNS_X_セキュリティ系/follow_analysis/my_account" \
    --username "my_account" \
    --max-duration-sec 480

Example (Apify pre-collected):
  collect_x_following_raw.sh \
    --target-dir "23_SNS_X_セキュリティ系/follow_analysis/my_account" \
    --username "my_account" \
    --input-json "/path/to/normalized.json" \
    --source apify
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
USERNAME=""
URL=""
CDP_URL="http://localhost:9300"
WAIT_MS="3000"
SCROLL_STEPS="45"
SCROLL_PX="1700"
INPUT_JSON=""
SOURCE="cdp"
SESSION_NAME=""
MAX_DURATION_SEC="600"
AB_SESSION_ACTIVE="0"
DEADLINE_TS="0"
TMP_ROWS=""

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
  if [[ -n "$TMP_ROWS" ]]; then
    rm -f "$TMP_ROWS"
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
    --username)
      USERNAME="${2:-}"
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
    --scroll-px)
      SCROLL_PX="${2:-}"
      shift 2
      ;;
    --input-json)
      INPUT_JSON="${2:-}"
      shift 2
      ;;
    --source)
      SOURCE="${2:-}"
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

if [[ -z "$TARGET_DIR" || -z "$USERNAME" ]]; then
  echo "Required arguments are missing." >&2
  usage
  exit 1
fi

USERNAME="${USERNAME#@}"
if ! [[ "$USERNAME" =~ ^[A-Za-z0-9_]{1,15}$ ]]; then
  echo "--username must match X handle format (letters/digits/underscore, max 15 chars)." >&2
  exit 1
fi

if [[ -z "$URL" ]]; then
  URL="https://x.com/${USERNAME}/following"
fi

if [[ "$SOURCE" != "cdp" && "$SOURCE" != "apify" ]]; then
  echo "--source must be 'cdp' or 'apify'" >&2
  exit 1
fi

if ! [[ "$MAX_DURATION_SEC" =~ ^[0-9]+$ ]]; then
  echo "--max-duration-sec must be an integer seconds (0 disables timeout)" >&2
  exit 1
fi

if [[ -z "$SESSION_NAME" ]]; then
  SESSION_NAME="xfa-$(sanitize_token "${USERNAME}")-$(now_epoch)-$$"
fi

require_cmd python3
mkdir -p "$TARGET_DIR/raw" "$TARGET_DIR/log"

TS="$(date +%Y%m%d_%H%M)"
COLLECTED_AT="$(date +%Y-%m-%dT%H:%M:%S%z)"
RAW_JSON="$TARGET_DIR/raw/${TS}_x_${USERNAME}_following_raw.json"
RAW_MD="$TARGET_DIR/raw/${TS}_x_${USERNAME}_following_raw.md"
SHOT_FILE="$TARGET_DIR/log/${TS}_x_${USERNAME}_following.png"
MANIFEST="$TARGET_DIR/log/manifest.tsv"

if [[ -n "$INPUT_JSON" ]]; then
  # --- Apify pre-collected path: skip CDP, copy normalized JSON ---
  if [[ ! -f "$INPUT_JSON" ]]; then
    echo "Input JSON not found: $INPUT_JSON" >&2
    exit 1
  fi

  COUNT="$(
    python3 - "$INPUT_JSON" <<'PY'
import json, sys
try:
    rows = json.loads(open(sys.argv[1], encoding="utf-8").read())
    print(len(rows) if isinstance(rows, list) else 0)
except Exception:
    print(0)
PY
  )"

  if [[ -z "$COUNT" || "$COUNT" == "0" ]]; then
    echo "No valid accounts in input JSON." >&2
    exit 1
  fi

  cp "$INPUT_JSON" "$RAW_JSON"
  RESOLVED_URL=""
  TITLE="(pre-collected via ${SOURCE})"

else
  # --- CDP collection path ---
  if ! [[ "$WAIT_MS" =~ ^[0-9]+$ ]]; then
    echo "--wait-ms must be an integer (milliseconds)" >&2
    exit 1
  fi

  if ! [[ "$SCROLL_STEPS" =~ ^[0-9]+$ ]]; then
    echo "--scroll-steps must be an integer" >&2
    exit 1
  fi

  if ! [[ "$SCROLL_PX" =~ ^[0-9]+$ ]]; then
    echo "--scroll-px must be an integer" >&2
    exit 1
  fi

  require_cmd agent-browser

  TMP_ROWS="$(mktemp)"
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

  check_deadline
  cat <<'EOF' | ab eval --stdin >/dev/null
(() => {
  window.__xfaSeen = {};
  window.__xfaRows = [];
  return true;
})();
EOF

  for ((i=0; i<=SCROLL_STEPS; i++)); do
    check_deadline
    cat <<'EOF' | ab eval --stdin >/dev/null
(() => {
  const seen = window.__xfaSeen || (window.__xfaSeen = {});
  const rows = window.__xfaRows || (window.__xfaRows = []);

  const clean = (value) => String(value || "")
    .replace(/\u00a0/g, " ")
    .replace(/[ \t]+/g, " ")
    .trim();

  const metaPatterns = [
    /^@[A-Za-z0-9_]{1,15}$/,
    /^(Follows you|フォローされています)$/i,
    /^(Following|Follow|Unfollow|フォロー中|フォローする|フォロー解除)$/i,
    /^(Promoted|プロモーション)$/i,
  ];

  const parseCell = (cell) => {
    const rawText = String(cell.innerText || "");
    if (!rawText.trim()) return null;

    const lines = rawText
      .split("\n")
      .map((line) => clean(line))
      .filter(Boolean);
    if (!lines.length) return null;
    const text = clean(rawText);

    let username = "";
    for (const anchor of Array.from(cell.querySelectorAll('a[href]'))) {
      const href = String(anchor.getAttribute("href") || "").split("?")[0];
      const match = href.match(/^\/([A-Za-z0-9_]{1,15})$/);
      if (match) {
        username = match[1];
        break;
      }
    }

    if (!username) {
      const handleLine = lines.find((line) => /^@[A-Za-z0-9_]{1,15}$/.test(line));
      if (handleLine) username = handleLine.slice(1);
    }

    if (!username) return null;
    const key = username.toLowerCase();
    if (seen[key]) return null;
    seen[key] = true;

    const displayNameRaw = lines[0] || username;
    const displayName = /^@[A-Za-z0-9_]{1,15}$/.test(displayNameRaw)
      ? username
      : displayNameRaw;

    const bioParts = [];
    for (let idx = 1; idx < lines.length; idx += 1) {
      const line = lines[idx];
      if (metaPatterns.some((pattern) => pattern.test(line))) continue;
      bioParts.push(line);
    }

    const buttonNode = cell.querySelector('button,[role="button"]');
    const followsYou = /(Follows you|フォローされています)/i.test(text);
    const verified = !!cell.querySelector('[data-testid="icon-verified"]');
    const protectedAccount = !!cell.querySelector('[data-testid="icon-lock"]');
    const avatar = cell.querySelector("img[src]");

    rows.push({
      username,
      display_name: displayName,
      bio: clean(bioParts.join(" ")),
      profile_url: `https://x.com/${username}`,
      follows_you: followsYou,
      verified,
      protected: protectedAccount,
      button_text: clean(buttonNode ? buttonNode.innerText : ""),
      avatar_url: avatar ? String(avatar.getAttribute("src") || "") : "",
      card_text: text,
      captured_at: new Date().toISOString(),
    });
    return true;
  };

  const primary = document.querySelector('[data-testid="primaryColumn"]') || document;
  for (const cell of Array.from(primary.querySelectorAll('[data-testid="UserCell"]'))) {
    parseCell(cell);
  }

  return rows.length;
})();
EOF

    if [[ "$i" -lt "$SCROLL_STEPS" ]]; then
      check_deadline
      ab scroll down "$SCROLL_PX" >/dev/null || true
      check_deadline
      ab wait 900 >/dev/null || true
    fi
  done

  check_deadline
  cat <<'EOF' | ab eval --stdin >"$TMP_ROWS"
(() => window.__xfaRows || [])();
EOF

  COUNT="$(
    python3 - "$TMP_ROWS" <<'PY'
import json
import sys

path = sys.argv[1]
try:
    with open(path, "r", encoding="utf-8") as f:
        rows = json.load(f)
except Exception:
    print(0)
    raise SystemExit(0)

print(len(rows) if isinstance(rows, list) else 0)
PY
  )"

  if [[ -z "$COUNT" || "$COUNT" == "0" ]]; then
    echo "No following accounts were extracted. Check X session and rerun." >&2
    exit 1
  fi

  python3 - "$TMP_ROWS" "$RAW_JSON" <<'PY'
import json
import pathlib
import sys

src = pathlib.Path(sys.argv[1])
dst = pathlib.Path(sys.argv[2])
rows = json.loads(src.read_text(encoding="utf-8"))
if not isinstance(rows, list):
    rows = []
dst.write_text(json.dumps(rows, ensure_ascii=False, indent=2), encoding="utf-8")
PY

fi

SUMMARY="$(
  python3 .agents/skills/x-follow-analysis/scripts/render_x_following_archive.py \
    --input-json "$RAW_JSON" \
    --output "$RAW_MD" \
    --username "$USERNAME" \
    --source-url "$URL" \
    --resolved-url "$RESOLVED_URL" \
    --title "$TITLE" \
    --collected-at "$COLLECTED_AT" \
    --screenshot-file "$(basename "$SHOT_FILE")"
)"

TOTAL_ACCOUNTS="$(printf "%s\n" "$SUMMARY" | awk -F= '/^total_accounts=/{print $2}')"

if [[ ! -f "$MANIFEST" ]]; then
  echo -e "timestamp\tplatform\tid\tsource_url\tresolved_url\ttitle\ttotal_accounts\traw_json\traw_md\tscreenshot_file\tcollection_source" >"$MANIFEST"
fi

printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" \
  "$COLLECTED_AT" \
  "x" \
  "$USERNAME" \
  "$(printf "%s" "$URL" | clean_tsv)" \
  "$(printf "%s" "$RESOLVED_URL" | clean_tsv)" \
  "$(printf "%s" "$TITLE" | clean_tsv)" \
  "${TOTAL_ACCOUNTS:-0}" \
  "$RAW_JSON" \
  "$RAW_MD" \
  "$SHOT_FILE" \
  "$SOURCE" \
  >>"$MANIFEST"

echo "raw_json=$RAW_JSON"
echo "raw_md=$RAW_MD"
echo "screenshot_file=$SHOT_FILE"
echo "manifest_file=$MANIFEST"
echo "total_accounts=${TOTAL_ACCOUNTS:-0}"
echo "collection_source=$SOURCE"
