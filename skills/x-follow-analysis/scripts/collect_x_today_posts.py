#!/usr/bin/env python3
import argparse
import concurrent.futures
import datetime as dt
import json
import os
import pathlib
import re
import subprocess
import sys
import time
from typing import Any, Dict, Iterable, List, Optional
from urllib.parse import quote


TZ_TOKYO = dt.timezone(dt.timedelta(hours=9))
AGENT_BROWSER_SESSION = ""
AGENT_BROWSER_TIMEOUT_SEC = 45
RUN_DEADLINE_MONOTONIC = 0.0
DEFAULT_TRANSLATION_PLACEHOLDER = "（ここに和訳を記入）"
TRANSLATION_FAILURE_PLACEHOLDER = "（和訳取得失敗）"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Collect today's X posts for all CRM accounts and write per-account markdown files."
    )
    parser.add_argument(
        "--accounts-dir",
        required=True,
        help=(
            "Path to crm/accounts directory "
            "(supports accounts/<handle>/ directories or accounts/<handle>.md files)."
        ),
    )
    parser.add_argument(
        "--owner",
        required=True,
        help="Owner handle (for frontmatter).",
    )
    parser.add_argument(
        "--handle",
        action="append",
        default=[],
        help=(
            "Collect only this handle. Can be specified multiple times. "
            "Accepts with/without leading '@'."
        ),
    )
    parser.add_argument(
        "--date",
        default="",
        help="Target date in YYYY-MM-DD (default: today in JST).",
    )
    parser.add_argument(
        "--cdp-url",
        default="http://localhost:9300",
        help="CDP endpoint for agent-browser connect.",
    )
    parser.add_argument(
        "--no-cdp-connect",
        action="store_true",
        help=(
            "Skip explicit CDP connect and run on agent-browser managed session. "
            "Useful when remote debugging Chrome is unavailable."
        ),
    )
    parser.add_argument(
        "--wait-ms",
        type=int,
        default=1800,
        help="Initial wait after opening profile page.",
    )
    parser.add_argument(
        "--scroll-steps",
        type=int,
        default=18,
        help="Max scroll steps per account.",
    )
    parser.add_argument(
        "--scroll-px",
        type=int,
        default=1700,
        help="Scroll amount per step.",
    )
    parser.add_argument(
        "--scroll-wait-ms",
        type=int,
        default=700,
        help="Wait after each scroll.",
    )
    parser.add_argument(
        "--min-steps-before-stop",
        type=int,
        default=3,
        help="Minimum scroll steps before early stop.",
    )
    parser.add_argument(
        "--max-accounts",
        type=int,
        default=0,
        help="Optional limit for dry-runs. 0 means all accounts.",
    )
    parser.add_argument(
        "--start-index",
        type=int,
        default=0,
        help="0-based start index in sorted account list.",
    )
    parser.add_argument(
        "--open-retries",
        type=int,
        default=4,
        help="Retry count for profile open on transient browser errors.",
    )
    parser.add_argument(
        "--retry-wait-ms",
        type=int,
        default=900,
        help="Wait between profile open retries.",
    )
    parser.add_argument(
        "--skip-existing",
        action="store_true",
        help="Skip accounts that already have posts/YYYYMMDD.md for target date.",
    )
    parser.add_argument(
        "--quiet-skips",
        action="store_true",
        help="Suppress per-account skip logs when --skip-existing is enabled.",
    )
    parser.add_argument(
        "--session-name",
        default="",
        help="agent-browser session name (default: auto-generated).",
    )
    parser.add_argument(
        "--command-timeout-sec",
        type=int,
        default=45,
        help="Timeout seconds per agent-browser command.",
    )
    parser.add_argument(
        "--max-duration-sec",
        type=int,
        default=1800,
        help="Hard cap for total runtime in seconds (0 disables).",
    )
    parser.add_argument(
        "--translation-mode",
        choices=["auto", "placeholder", "none"],
        default="auto",
        help=(
            "Translation mode: auto (required, default), "
            "placeholder (legacy), none (disable translation block)."
        ),
    )
    parser.add_argument(
        "--translation-openai-model",
        default=os.getenv("OPENAI_TRANSLATION_MODEL", "gpt-4.1-mini"),
        help="OpenAI model name used for translation when OPENAI_API_KEY is set.",
    )
    parser.add_argument(
        "--translation-openai-timeout-sec",
        type=int,
        default=120,
        help="Timeout seconds for each OpenAI translation batch request.",
    )
    parser.add_argument(
        "--translation-http-timeout-sec",
        type=int,
        default=60,
        help="Timeout seconds for HTTP fallback translation requests.",
    )
    parser.add_argument(
        "--translation-batch-size",
        type=int,
        default=8,
        help="Batch size for OpenAI translation requests.",
    )
    parser.add_argument(
        "--translation-max-workers",
        type=int,
        default=6,
        help="Max workers for fallback translation requests.",
    )
    parser.add_argument(
        "--allow-translation-fallback-placeholder",
        action="store_true",
        help=(
            "Allow unresolved translations to be written as placeholders "
            f"({TRANSLATION_FAILURE_PLACEHOLDER}). "
            "By default unresolved translations cause failure."
        ),
    )
    parser.add_argument(
        "--translation-placeholder",
        dest="translation_mode",
        action="store_const",
        const="placeholder",
        help="Legacy flag: equivalent to --translation-mode placeholder.",
    )
    parser.add_argument(
        "--no-translation-placeholder",
        dest="translation_mode",
        action="store_const",
        const="none",
        help="Legacy flag: equivalent to --translation-mode none.",
    )
    parser.add_argument(
        "--translation-placeholder-text",
        default=DEFAULT_TRANSLATION_PLACEHOLDER,
        help="Placeholder text for manual Japanese translation blocks.",
    )
    parser.set_defaults(translation_mode="auto")
    return parser.parse_args()


def sanitize_token(value: str) -> str:
    return re.sub(r"[^A-Za-z0-9_-]+", "-", value).strip("-") or "session"


def check_deadline() -> None:
    if RUN_DEADLINE_MONOTONIC and time.monotonic() >= RUN_DEADLINE_MONOTONIC:
        raise TimeoutError("run exceeded --max-duration-sec")


def run_agent_browser(
    args: List[str], stdin_text: str = "", *, ignore_deadline: bool = False
) -> str:
    if not ignore_deadline:
        check_deadline()
    cmd = ["agent-browser"]
    if AGENT_BROWSER_SESSION:
        cmd.extend(["--session", AGENT_BROWSER_SESSION])
    cmd.extend(args)
    try:
        proc = subprocess.run(
            cmd,
            input=stdin_text if stdin_text else None,
            text=True,
            capture_output=True,
            timeout=max(1, AGENT_BROWSER_TIMEOUT_SEC),
        )
    except subprocess.TimeoutExpired as exc:
        raise TimeoutError(f"command timeout: {' '.join(cmd)}") from exc
    if proc.returncode != 0:
        raise RuntimeError(f"command failed: {' '.join(cmd)}\n{proc.stderr.strip()}")
    if not ignore_deadline:
        check_deadline()
    return proc.stdout.strip()


def parse_target_date(raw: str) -> dt.date:
    if raw:
        return dt.date.fromisoformat(raw)
    return dt.datetime.now(TZ_TOKYO).date()


def list_handles(accounts_dir: pathlib.Path) -> List[str]:
    handles_set = set()
    for child in sorted(accounts_dir.iterdir(), key=lambda p: p.name.lower()):
        if child.is_dir():
            name = child.name
        elif child.is_file() and child.suffix.lower() == ".md":
            name = child.stem
        else:
            continue
        if re.fullmatch(r"[A-Za-z0-9_]{1,15}", name):
            handles_set.add(name)
    return sorted(handles_set, key=str.lower)


def normalize_handle(raw: str) -> str:
    value = raw.strip()
    if value.startswith("@"):
        value = value[1:]
    return value


def select_requested_handles(
    all_handles: List[str], requested: List[str]
) -> List[str]:
    if not requested:
        return list(all_handles)

    lookup = {h.lower(): h for h in all_handles}
    out: List[str] = []
    missing: List[str] = []
    seen = set()
    for raw in requested:
        handle = normalize_handle(raw)
        if not re.fullmatch(r"[A-Za-z0-9_]{1,15}", handle):
            raise SystemExit(f"invalid --handle value: {raw}")
        key = handle.lower()
        if key in seen:
            continue
        seen.add(key)
        matched = lookup.get(key)
        if matched is None:
            missing.append(handle)
            continue
        out.append(matched)

    if missing:
        raise SystemExit(
            "requested handles not found under --accounts-dir: " + ", ".join(missing)
        )
    return out


def resolve_posts_dir(accounts_dir: pathlib.Path, handle: str) -> pathlib.Path:
    dir_style = accounts_dir / handle
    file_style = accounts_dir / f"{handle}.md"
    if dir_style.is_dir():
        return dir_style / "posts"
    if file_style.is_file():
        return accounts_dir.parent / "posts" / handle
    return dir_style / "posts"


def parse_iso_datetime(value: str) -> Optional[dt.datetime]:
    if not value:
        return None
    try:
        return dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def parse_compact_number(value: str) -> int:
    v = value.strip().replace(",", "")
    if not v:
        return 0
    mul = 1.0
    if v.lower().endswith("k"):
        mul = 1_000.0
        v = v[:-1]
    elif v.lower().endswith("m"):
        mul = 1_000_000.0
        v = v[:-1]
    elif v.lower().endswith("b"):
        mul = 1_000_000_000.0
        v = v[:-1]
    try:
        return int(float(v) * mul)
    except ValueError:
        return 0


def metric_from_labels(labels: List[str], pattern: str) -> int:
    rx = re.compile(pattern, re.IGNORECASE)
    for label in labels:
        m = rx.search(label or "")
        if m:
            return parse_compact_number(m.group(1))
    return 0


def classify_type(text: str, social_context: str, has_quote: bool) -> str:
    if has_quote:
        return "引用RT"
    if text.lstrip().startswith("@"):
        return "リプライ"
    if re.search(r"Replying to|返信先", social_context, re.IGNORECASE):
        return "リプライ"
    return "投稿"


def extract_display_name(title: str, handle: str) -> str:
    title = (title or "").strip()
    m = re.match(rf"^(.*?)\s+\(@?{re.escape(handle)}\)\s*/\s*X\s*$", title)
    if m:
        cand = re.sub(r"^\(\d+\)\s*", "", m.group(1).strip()).strip()
        if cand:
            return cand
    return handle


def init_capture_state(handle: str) -> None:
    js = f"""
(() => {{
  window.__xfpTarget = "{handle.lower()}";
  window.__xfpSeen = {{}};
  window.__xfpRows = [];
  return true;
}})();
"""
    run_agent_browser(["eval", "--stdin"], js)


def open_profile_with_retry(
    profile_url: str,
    cdp_url: str,
    use_cdp_connect: bool,
    open_retries: int,
    retry_wait_ms: int,
) -> None:
    max_attempts = max(1, open_retries)
    last_exc: Optional[Exception] = None
    for attempt in range(1, max_attempts + 1):
        try:
            run_agent_browser(["open", profile_url])
            return
        except Exception as exc:
            last_exc = exc
            if attempt >= max_attempts:
                break
            # Transient X/CDP navigation failures (e.g., ERR_ABORTED) are common.
            if use_cdp_connect and cdp_url.strip():
                try:
                    run_agent_browser(["connect", cdp_url])
                except Exception:
                    pass
            run_agent_browser(["wait", str(max(100, retry_wait_ms * attempt))])
    if last_exc is not None:
        raise last_exc


def capture_visible_tweets() -> Dict[str, Any]:
    js = """
(() => {
  const target = (window.__xfpTarget || "").toLowerCase();
  const seen = window.__xfpSeen || (window.__xfpSeen = {});
  const rows = window.__xfpRows || (window.__xfpRows = []);
  let newCount = 0;

  const normalizeStatus = (href) => {
    if (!href) return null;
    const m = href.match(/^https:\\/\\/x\\.com\\/([^/]+)\\/status\\/(\\d+)/i);
    if (!m) return null;
    return {
      account: m[1],
      accountLower: m[1].toLowerCase(),
      statusId: m[2],
      url: `https://x.com/${m[1]}/status/${m[2]}`
    };
  };

  const articleNodes = Array.from(document.querySelectorAll('article[data-testid="tweet"]'));
  for (const article of articleNodes) {
    const linkEl = article.querySelector('a[href*="/status/"]');
    const n = normalizeStatus(linkEl ? linkEl.href : "");
    if (!n) continue;
    if (n.accountLower !== target) continue;
    if (seen[n.statusId]) continue;
    seen[n.statusId] = true;

    const timeEl = article.querySelector("time");
    const text = Array.from(article.querySelectorAll('[data-testid="tweetText"]'))
      .map((x) => (x.innerText || "").trim())
      .filter(Boolean)
      .join("\\n")
      .trim();
    const socialContext = article.querySelector('[data-testid="socialContext"]');
    const metricsLabels = Array.from(article.querySelectorAll('[aria-label]'))
      .map((x) => x.getAttribute('aria-label') || "")
      .filter(Boolean);

    rows.push({
      status_id: n.statusId,
      account: n.account,
      url: n.url,
      datetime: timeEl ? (timeEl.getAttribute("datetime") || "") : "",
      text,
      social_context: socialContext ? (socialContext.innerText || "").trim() : "",
      has_quote: !!article.querySelector('[data-testid="quoteTweet"]'),
      has_media: !!article.querySelector('[data-testid="tweetPhoto"], [data-testid="videoPlayer"], [data-testid="card.wrapper"]'),
      metrics_labels: metricsLabels
    });
    newCount += 1;
  }

  let minDatetime = "";
  for (const row of rows) {
    if (!row.datetime) continue;
    if (!minDatetime || row.datetime < minDatetime) {
      minDatetime = row.datetime;
    }
  }

  return {
    new_count: newCount,
    total_rows: rows.length,
    min_datetime: minDatetime
  };
})();
"""
    raw = run_agent_browser(["eval", "--stdin"], js)
    try:
        obj = json.loads(raw)
        if isinstance(obj, dict):
            return obj
    except json.JSONDecodeError:
        pass
    return {"new_count": 0, "total_rows": 0, "min_datetime": ""}


def fetch_rows() -> List[Dict[str, Any]]:
    raw = run_agent_browser(["eval", "--stdin"], "(() => window.__xfpRows || [])();")
    try:
        obj = json.loads(raw)
        if isinstance(obj, list):
            return [x for x in obj if isinstance(x, dict)]
    except json.JSONDecodeError:
        pass
    return []


def filter_today_rows(rows: List[Dict[str, Any]], target_date: dt.date) -> List[Dict[str, Any]]:
    out: List[Dict[str, Any]] = []
    for row in rows:
        dt_raw = str(row.get("datetime", "")).strip()
        dt_obj = parse_iso_datetime(dt_raw)
        if not dt_obj:
            continue
        if dt_obj.astimezone(TZ_TOKYO).date() != target_date:
            continue
        labels = [str(v) for v in row.get("metrics_labels", []) if str(v).strip()]
        text = str(row.get("text", "")).strip()
        social_context = str(row.get("social_context", "")).strip()
        has_quote = bool(row.get("has_quote", False))
        out.append(
            {
                "status_id": str(row.get("status_id", "")).strip(),
                "url": str(row.get("url", "")).strip(),
                "datetime_utc": dt_obj.astimezone(dt.timezone.utc),
                "datetime_jst": dt_obj.astimezone(TZ_TOKYO),
                "text": text,
                "social_context": social_context,
                "has_quote": has_quote,
                "has_media": bool(row.get("has_media", False)),
                "likes": metric_from_labels(labels, r"([0-9][0-9,\\.]*[KMBkmb]?)\\s+Likes?"),
                "reposts": metric_from_labels(labels, r"([0-9][0-9,\\.]*[KMBkmb]?)\\s+Reposts?"),
                "replies": metric_from_labels(labels, r"([0-9][0-9,\\.]*[KMBkmb]?)\\s+Replies?"),
                "type_label": classify_type(text, social_context, has_quote),
            }
        )
    out.sort(key=lambda x: (x["datetime_jst"], x["status_id"]), reverse=True)
    return out


def quote_lines(text: str) -> List[str]:
    if not text:
        return ["> (本文なし)"]
    lines = text.splitlines()
    rendered: List[str] = []
    for line in lines:
        if line.strip():
            rendered.append(f"> {line.rstrip()}")
        else:
            rendered.append(">")
    return rendered


def read_existing_total(md_path: pathlib.Path) -> int:
    try:
        text = md_path.read_text(encoding="utf-8")
    except Exception:
        return 0
    m = re.search(r"^\s*total_today:\s*(\d+)\s*$", text, re.MULTILINE)
    if not m:
        return 0
    try:
        return int(m.group(1))
    except ValueError:
        return 0


def flatten_text(text: str) -> str:
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    return " ".join(lines).strip()


def is_probably_japanese(text: str) -> bool:
    if not text.strip():
        return False
    return bool(re.search(r"[ぁ-んァ-ヶー]", text))


def chunked(items: List[Dict[str, str]], size: int) -> Iterable[List[Dict[str, str]]]:
    batch_size = max(1, size)
    for i in range(0, len(items), batch_size):
        yield items[i : i + batch_size]


def parse_model_json(content: str) -> Dict[str, Any]:
    if not content:
        return {}
    try:
        parsed = json.loads(content)
        if isinstance(parsed, dict):
            return parsed
    except json.JSONDecodeError:
        pass
    m = re.search(r"\{.*\}", content, re.DOTALL)
    if not m:
        return {}
    try:
        parsed = json.loads(m.group(0))
    except json.JSONDecodeError:
        return {}
    return parsed if isinstance(parsed, dict) else {}


def request_openai_translations(
    batch: List[Dict[str, str]],
    model: str,
    timeout_sec: int,
) -> Dict[str, str]:
    api_key = os.getenv("OPENAI_API_KEY", "").strip()
    if not api_key or not batch:
        return {}
    base_url = os.getenv("OPENAI_BASE_URL", "https://api.openai.com").strip().rstrip("/")
    if base_url.endswith("/v1"):
        endpoint = f"{base_url}/chat/completions"
    else:
        endpoint = f"{base_url}/v1/chat/completions"

    user_prompt = (
        "以下の投稿テキストを自然な日本語に翻訳してください。"
        "URL・@メンション・#タグ・改行はできるだけ保持し、要約せず意味を変えないでください。"
        "JSONのみで返し、形式は "
        '{"translations":[{"status_id":"...","ja":"..."}]}'
        " としてください。\n"
        + json.dumps(batch, ensure_ascii=False)
    )
    body = {
        "model": model,
        "temperature": 0,
        "messages": [
            {
                "role": "system",
                "content": "You are a precise translator. Return valid JSON only.",
            },
            {
                "role": "user",
                "content": user_prompt,
            },
        ],
    }
    cmd = [
        "curl",
        "-sS",
        "--max-time",
        str(max(10, timeout_sec)),
        endpoint,
        "-H",
        f"Authorization: Bearer {api_key}",
        "-H",
        "Content-Type: application/json",
        "-d",
        json.dumps(body, ensure_ascii=False),
    ]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=max(15, timeout_sec + 15))
    except Exception:
        return {}
    if proc.returncode != 0:
        return {}
    try:
        parsed = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return {}
    if isinstance(parsed, dict) and parsed.get("error"):
        return {}
    choices = parsed.get("choices")
    if not isinstance(choices, list) or not choices:
        return {}
    content = str((choices[0].get("message") or {}).get("content") or "")
    content_dict = parse_model_json(content)
    rows = content_dict.get("translations")
    if not isinstance(rows, list):
        return {}

    out: Dict[str, str] = {}
    for row in rows:
        if not isinstance(row, dict):
            continue
        sid = str(row.get("status_id", "")).strip()
        ja = str(row.get("ja", "")).strip()
        if sid and ja:
            out[sid] = ja
    return out


def translate_text_via_google(text: str, timeout_sec: int) -> str:
    src = text.strip()
    if not src:
        return ""
    endpoint = (
        "https://translate.googleapis.com/translate_a/single"
        f"?client=gtx&sl=auto&tl=ja&dt=t&q={quote(src, safe='')}"
    )
    cmd = [
        "curl",
        "-sS",
        "--max-time",
        str(max(5, timeout_sec)),
        endpoint,
    ]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=max(10, timeout_sec + 10))
    except Exception:
        return ""
    if proc.returncode != 0:
        return ""
    try:
        payload = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return ""
    if not isinstance(payload, list) or not payload:
        return ""
    parts = payload[0]
    if not isinstance(parts, list):
        return ""
    out_parts: List[str] = []
    for piece in parts:
        if not isinstance(piece, list) or not piece:
            continue
        text_piece = str(piece[0]).strip()
        if text_piece:
            out_parts.append(text_piece)
    return flatten_text("".join(out_parts))


def build_ja_translations(
    posts: List[Dict[str, Any]],
    *,
    openai_model: str,
    openai_timeout_sec: int,
    http_timeout_sec: int,
    batch_size: int,
    max_workers: int,
    allow_failure_placeholder: bool,
) -> Dict[str, str]:
    translations: Dict[str, str] = {}
    pending: List[Dict[str, str]] = []
    text_by_status: Dict[str, str] = {}

    for post in posts:
        status_id = str(post.get("status_id", "")).strip()
        if not status_id:
            continue
        text = str(post.get("text", "")).strip()
        normalized = flatten_text(text)
        text_by_status[status_id] = normalized
        if not normalized:
            translations[status_id] = "（本文なし）"
            continue
        if is_probably_japanese(normalized):
            translations[status_id] = normalized
            continue
        pending.append({"status_id": status_id, "text": text})

    for batch in chunked(pending, batch_size):
        check_deadline()
        got = request_openai_translations(
            batch=batch,
            model=openai_model,
            timeout_sec=openai_timeout_sec,
        )
        for sid, ja in got.items():
            if sid in text_by_status and ja.strip():
                translations[sid] = ja.strip()

    unresolved = [
        row for row in pending if row["status_id"] not in translations
    ]
    if unresolved:
        worker_count = max(1, max_workers)
        with concurrent.futures.ThreadPoolExecutor(max_workers=worker_count) as executor:
            futures = {
                executor.submit(
                    translate_text_via_google,
                    str(row.get("text", "")),
                    http_timeout_sec,
                ): str(row.get("status_id"))
                for row in unresolved
            }
            for future in concurrent.futures.as_completed(futures):
                check_deadline()
                sid = futures[future]
                try:
                    ja = future.result().strip()
                except Exception:
                    ja = ""
                if ja:
                    translations[sid] = ja

    missing_ids = [
        str(post.get("status_id", "")).strip()
        for post in posts
        if str(post.get("status_id", "")).strip() and not translations.get(str(post.get("status_id", "")).strip(), "").strip()
    ]
    if missing_ids and allow_failure_placeholder:
        for sid in missing_ids:
            translations[sid] = TRANSLATION_FAILURE_PLACEHOLDER
        missing_ids = []

    if missing_ids:
        preview = ", ".join(missing_ids[:10])
        more = "" if len(missing_ids) <= 10 else f" ... (+{len(missing_ids) - 10})"
        raise RuntimeError(f"translation unresolved status_ids: {preview}{more}")

    return translations


def write_daily_markdown(
    out_path: pathlib.Path,
    owner: str,
    handle: str,
    display_name: str,
    collected_at: dt.datetime,
    target_date: dt.date,
    posts: List[Dict[str, Any]],
    translation_mode: str,
    ja_translations: Dict[str, str],
    translation_placeholder_text: str,
) -> None:
    lines: List[str] = []
    lines.extend(
        [
            "---",
            "tags:",
            "  - x-daily-posts",
            f"  - {handle}",
            f"owner: {owner}",
            f"username: {handle}",
            f"display_name: {display_name}",
            f"collected_at: {collected_at.isoformat()}",
            f"target_date: {target_date.isoformat()}",
            f"translation_mode: {translation_mode}",
            "stats:",
            f"  total_today: {len(posts)}",
            "source: agent-browser",
            "---",
            "",
            f"# @{handle} 本日投稿アーカイブ ({target_date.isoformat()})",
            "",
            "## 概要",
            "",
            f"- アカウント: [@{handle}](https://x.com/{handle})",
            f"- 表示名: {display_name}",
            f"- 収集日時: {collected_at.astimezone(TZ_TOKYO).strftime('%Y-%m-%d %H:%M:%S %z')}",
            f"- 本日投稿数: {len(posts)}",
            "",
        ]
    )
    if not posts:
        lines.extend(
            [
                "## 投稿一覧",
                "",
                "- 本日投稿は検出されませんでした（取得時点）。",
                "",
            ]
        )
    else:
        lines.extend(["## 投稿一覧", ""])
        for post in posts:
            time_label = post["datetime_jst"].strftime("%H:%M")
            lines.extend(
                [
                    f"### {time_label} - {post['likes']} いいね",
                    "",
                    f"**URL**: {post['url']}",
                    f"**種別**: {post['type_label']}",
                    f"**RT**: {post['reposts']} / **リプライ**: {post['replies']}",
                    "",
                    *quote_lines(post["text"]),
                    "",
                    *(
                        []
                        if translation_mode == "none"
                        else (
                            [
                                "**和訳（手動）**",
                                f"> {translation_placeholder_text}",
                                "",
                            ]
                            if translation_mode == "placeholder"
                            else [
                                "**和訳**",
                                *quote_lines(
                                    ja_translations.get(
                                        str(post.get("status_id", "")).strip(),
                                        TRANSLATION_FAILURE_PLACEHOLDER,
                                    )
                                ),
                                "",
                            ]
                        )
                    ),
                    "---",
                    "",
                ]
            )
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(lines), encoding="utf-8")


def write_posts_index(posts_dir: pathlib.Path, handle: str) -> None:
    files = sorted(
        [
            p
            for p in posts_dir.glob("*.md")
            if p.name != "index.md" and re.fullmatch(r"\d{8}\.md", p.name)
        ],
        reverse=True,
    )
    lines: List[str] = []
    lines.extend(
        [
            "---",
            f"username: {handle}",
            f"updated: {dt.datetime.now(TZ_TOKYO).isoformat()}",
            "---",
            "",
            f"# @{handle} 投稿アーカイブ",
            "",
            "## 日次ファイル",
            "",
        ]
    )
    if not files:
        lines.append("- 投稿ファイルはまだありません。")
    else:
        for f in files:
            lines.append(f"- [{f.name}]({f.name})")
    lines.append("")
    (posts_dir / "index.md").write_text("\n".join(lines), encoding="utf-8")


def main() -> None:
    global AGENT_BROWSER_SESSION, AGENT_BROWSER_TIMEOUT_SEC, RUN_DEADLINE_MONOTONIC

    args = parse_args()
    if args.command_timeout_sec <= 0:
        raise SystemExit("--command-timeout-sec must be > 0")
    if args.max_duration_sec < 0:
        raise SystemExit("--max-duration-sec must be >= 0")
    if args.translation_openai_timeout_sec <= 0:
        raise SystemExit("--translation-openai-timeout-sec must be > 0")
    if args.translation_http_timeout_sec <= 0:
        raise SystemExit("--translation-http-timeout-sec must be > 0")
    if args.translation_batch_size <= 0:
        raise SystemExit("--translation-batch-size must be > 0")
    if args.translation_max_workers <= 0:
        raise SystemExit("--translation-max-workers must be > 0")

    accounts_dir = pathlib.Path(args.accounts_dir).resolve()
    if not accounts_dir.exists():
        raise SystemExit(f"accounts dir not found: {accounts_dir}")

    target_date = parse_target_date(args.date)
    target_tag = target_date.strftime("%Y%m%d")
    AGENT_BROWSER_TIMEOUT_SEC = args.command_timeout_sec
    if args.max_duration_sec > 0:
        RUN_DEADLINE_MONOTONIC = time.monotonic() + args.max_duration_sec

    if args.session_name.strip():
        AGENT_BROWSER_SESSION = args.session_name.strip()
    else:
        AGENT_BROWSER_SESSION = sanitize_token(
            f"x-daily-{args.owner}-{target_tag}-{os.getpid()}"
        )

    handles = list_handles(accounts_dir)
    handles = select_requested_handles(handles, args.handle)
    if args.start_index and args.start_index > 0:
        handles = handles[args.start_index :]
    if args.max_accounts and args.max_accounts > 0:
        handles = handles[: args.max_accounts]

    if not handles:
        raise SystemExit("no accounts found under --accounts-dir")

    print(f"accounts_total={len(handles)}")
    print(f"target_date={target_date.isoformat()}")
    print(f"session_name={AGENT_BROWSER_SESSION}")
    print(f"translation_mode={args.translation_mode}")
    use_cdp_connect = not args.no_cdp_connect

    success = 0
    failures: List[str] = []
    summary_rows: List[Dict[str, Any]] = []
    timeout_reached = False

    try:
        try:
            if use_cdp_connect and args.cdp_url.strip():
                try:
                    run_agent_browser(["connect", args.cdp_url])
                except Exception as exc:
                    print(
                        f"warn: cdp connect failed, fallback to native session: {exc}",
                        file=sys.stderr,
                    )
                    use_cdp_connect = False

            for idx, handle in enumerate(handles, start=1):
                profile_url = f"https://x.com/{handle}"
                posts_dir = resolve_posts_dir(accounts_dir, handle)
                out_md = posts_dir / f"{target_tag}.md"

                if args.skip_existing and out_md.exists():
                    existing_count = read_existing_total(out_md)
                    summary_rows.append(
                        {
                            "handle": handle,
                            "count": existing_count,
                            "file": str(out_md),
                        }
                    )
                    success += 1
                    if not args.quiet_skips:
                        print(f"[{idx}/{len(handles)}] collecting @{handle} ...")
                        print(f"  -> skip existing posts={existing_count} file={out_md}")
                    continue

                print(f"[{idx}/{len(handles)}] collecting @{handle} ...")
                try:
                    open_profile_with_retry(
                        profile_url=profile_url,
                        cdp_url=args.cdp_url,
                        use_cdp_connect=use_cdp_connect,
                        open_retries=args.open_retries,
                        retry_wait_ms=args.retry_wait_ms,
                    )
                    run_agent_browser(["wait", str(args.wait_ms)])
                    title = run_agent_browser(["get", "title"])
                    display_name = extract_display_name(title, handle)

                    init_capture_state(handle)

                    for step in range(args.scroll_steps):
                        snap = capture_visible_tweets()
                        min_dt_raw = str(snap.get("min_datetime", "") or "").strip()
                        if min_dt_raw:
                            min_dt = parse_iso_datetime(min_dt_raw)
                            if min_dt is not None:
                                min_date_jst = min_dt.astimezone(TZ_TOKYO).date()
                                if (
                                    step + 1 >= args.min_steps_before_stop
                                    and min_date_jst < target_date
                                ):
                                    break

                        if step < args.scroll_steps - 1:
                            run_agent_browser(["scroll", "down", str(args.scroll_px)])
                            run_agent_browser(["wait", str(args.scroll_wait_ms)])

                    rows = fetch_rows()
                    today_posts = filter_today_rows(rows, target_date)
                    collected_at = dt.datetime.now(dt.timezone.utc)
                    ja_translations: Dict[str, str] = {}
                    if args.translation_mode == "auto" and today_posts:
                        ja_translations = build_ja_translations(
                            today_posts,
                            openai_model=args.translation_openai_model,
                            openai_timeout_sec=args.translation_openai_timeout_sec,
                            http_timeout_sec=args.translation_http_timeout_sec,
                            batch_size=args.translation_batch_size,
                            max_workers=args.translation_max_workers,
                            allow_failure_placeholder=args.allow_translation_fallback_placeholder,
                        )

                    write_daily_markdown(
                        out_path=out_md,
                        owner=args.owner,
                        handle=handle,
                        display_name=display_name,
                        collected_at=collected_at,
                        target_date=target_date,
                        posts=today_posts,
                        translation_mode=args.translation_mode,
                        ja_translations=ja_translations,
                        translation_placeholder_text=args.translation_placeholder_text,
                    )
                    write_posts_index(posts_dir, handle)

                    success += 1
                    summary_rows.append(
                        {
                            "handle": handle,
                            "count": len(today_posts),
                            "file": str(out_md),
                        }
                    )
                    print(f"  -> ok posts={len(today_posts)} file={out_md}")
                except TimeoutError as exc:
                    failures.append(f"{handle}: {exc}")
                    print(f"  -> timeout @{handle}: {exc}", file=sys.stderr)
                    timeout_reached = True
                    break
                except Exception as exc:
                    failures.append(f"{handle}: {exc}")
                    print(f"  -> fail @{handle}: {exc}", file=sys.stderr)
        except TimeoutError as exc:
            failures.append(f"runtime: {exc}")
            print(f"  -> timeout: {exc}", file=sys.stderr)
            timeout_reached = True
    finally:
        try:
            run_agent_browser(["close"], ignore_deadline=True)
        except Exception:
            pass

    summary_dir = accounts_dir.parent / "daily_post_reports"
    summary_dir.mkdir(parents=True, exist_ok=True)
    summary_path = summary_dir / f"{target_tag}_summary.md"
    lines: List[str] = []
    lines.extend(
        [
            "---",
            f"owner: {args.owner}",
            f"target_date: {target_date.isoformat()}",
            f"generated_at: {dt.datetime.now(TZ_TOKYO).isoformat()}",
            f"accounts_total: {len(handles)}",
            f"success_count: {success}",
            f"failure_count: {len(failures)}",
            "---",
            "",
            f"# @{args.owner} フォロー先 本日投稿収集レポート",
            "",
            f"- 対象日: {target_date.isoformat()}",
            f"- 成功: {success}",
            f"- 失敗: {len(failures)}",
            "",
            "## 投稿件数",
            "",
            "| Handle | 本日投稿数 | ファイル |",
            "|---|---:|---|",
        ]
    )
    for row in sorted(summary_rows, key=lambda x: x["handle"].lower()):
        rel = pathlib.Path(row["file"]).relative_to(accounts_dir.parent)
        lines.append(f"| @{row['handle']} | {row['count']} | `{rel}` |")
    if failures:
        lines.extend(["", "## 失敗一覧", ""])
        for f in failures:
            lines.append(f"- {f}")
    lines.append("")
    summary_path.write_text("\n".join(lines), encoding="utf-8")

    print(f"summary_file={summary_path}")
    print(f"success_count={success}")
    print(f"failure_count={len(failures)}")
    if timeout_reached:
        print("timeout_reached=true")
        raise SystemExit(124)


if __name__ == "__main__":
    main()
