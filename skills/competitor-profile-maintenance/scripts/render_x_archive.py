#!/usr/bin/env python3
import argparse
import concurrent.futures
import datetime as dt
import json
import os
import pathlib
import re
import subprocess
from collections import Counter, defaultdict
from dataclasses import dataclass
from typing import Any, Dict, Iterable, List, Optional, Tuple
from urllib.parse import quote, urlparse


METRIC_PATTERNS = {
    "replies": re.compile(r"([0-9][0-9,\.]*[KMBkmb]?)\s+Replies?", re.IGNORECASE),
    "reposts": re.compile(r"([0-9][0-9,\.]*[KMBkmb]?)\s+Reposts?", re.IGNORECASE),
    "likes": re.compile(r"([0-9][0-9,\.]*[KMBkmb]?)\s+Likes?", re.IGNORECASE),
    "views": re.compile(r"([0-9][0-9,\.]*[KMBkmb]?)\s+views?", re.IGNORECASE),
}

STOP_WORDS = {
    "about",
    "after",
    "agent",
    "all",
    "also",
    "and",
    "any",
    "are",
    "around",
    "been",
    "but",
    "can",
    "code",
    "design",
    "dont",
    "for",
    "from",
    "getting",
    "have",
    "here",
    "into",
    "just",
    "like",
    "more",
    "much",
    "not",
    "out",
    "prompt",
    "really",
    "that",
    "the",
    "this",
    "tool",
    "with",
    "work",
    "works",
    "your",
}


@dataclass
class Post:
    status_id: str
    account: str
    url: str
    datetime_raw: str
    datetime_local: Optional[dt.datetime]
    text: str
    social_context: str
    has_quote: bool
    has_media: bool
    replies: int
    reposts: int
    likes: int
    views: int
    type_label: str
    language: str


def chunked(values: List[Post], size: int) -> Iterable[List[Post]]:
    for i in range(0, len(values), size):
        yield values[i : i + size]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Render X timeline extraction JSON to archive markdown format."
    )
    parser.add_argument("--input-json", required=True, help="Path to extracted JSON file")
    parser.add_argument("--output", required=True, help="Output markdown path")
    parser.add_argument("--username", required=True, help="Target username")
    parser.add_argument("--source-url", required=True, help="Requested source URL")
    parser.add_argument("--resolved-url", required=True, help="Resolved URL")
    parser.add_argument("--title", required=True, help="Page title")
    parser.add_argument("--collected-at", required=True, help="Collection timestamp")
    parser.add_argument("--screenshot-file", required=True, help="Screenshot file name")
    return parser.parse_args()


def normalize_status_url(url: str) -> Optional[Tuple[str, str, str]]:
    if not url:
        return None
    parsed = urlparse(url)
    if not parsed.scheme or not parsed.netloc:
        return None
    parts = [p for p in parsed.path.split("/") if p]
    if len(parts) < 3:
        return None
    if parts[1] != "status":
        return None
    status_id = parts[2]
    if not status_id.isdigit():
        return None
    account = parts[0]
    normalized = f"{parsed.scheme}://{parsed.netloc}/{account}/status/{status_id}"
    return normalized, account, status_id


def parse_iso_datetime(value: str) -> Optional[dt.datetime]:
    if not value:
        return None
    try:
        return dt.datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone()
    except ValueError:
        return None


def parse_compact_number(text: str) -> int:
    value = text.strip().replace(",", "")
    multiplier = 1.0
    if value.lower().endswith("k"):
        multiplier = 1_000.0
        value = value[:-1]
    elif value.lower().endswith("m"):
        multiplier = 1_000_000.0
        value = value[:-1]
    elif value.lower().endswith("b"):
        multiplier = 1_000_000_000.0
        value = value[:-1]
    try:
        return int(float(value) * multiplier)
    except ValueError:
        return 0


def metric_from_labels(labels: Iterable[str], key: str) -> int:
    pattern = METRIC_PATTERNS[key]
    for label in labels:
        match = pattern.search(label)
        if match:
            return parse_compact_number(match.group(1))
    return 0


def detect_language(text: str) -> str:
    if not text.strip():
        return "その他"
    has_cjk = bool(re.search(r"[\u4e00-\u9fff]", text))
    has_kana = bool(re.search(r"[\u3040-\u30ff]", text))
    has_latin = bool(re.search(r"[A-Za-z]", text))
    if has_cjk and not has_kana:
        return "中国語"
    if has_latin:
        return "英語"
    if has_kana:
        return "日本語"
    return "その他"


def classify_type(text: str, social_context: str, has_quote: bool) -> str:
    if has_quote:
        return "引用RT"
    if text.lstrip().startswith("@"):
        return "リプライ"
    if re.search(r"Replying to|返信先", social_context, re.IGNORECASE):
        return "リプライ"
    return "投稿"


def parse_display_name_from_title(title: str, username: str) -> str:
    pattern = re.compile(r"^(.*?)\s+\(@?" + re.escape(username) + r"\)\s*/\s*X\s*$")
    match = pattern.match(title.strip())
    if match:
        candidate = match.group(1).strip()
        candidate = re.sub(r"^\(\d+\)\s*", "", candidate).strip()
        if candidate:
            return candidate
    return username


def load_posts(input_path: pathlib.Path, username: str) -> List[Post]:
    try:
        payload = json.loads(input_path.read_text(encoding="utf-8"))
    except Exception:
        return []
    if not isinstance(payload, list):
        return []

    target = username.lower()
    deduped: Dict[str, Post] = {}

    for row in payload:
        if not isinstance(row, dict):
            continue
        normalized = normalize_status_url(str(row.get("url", "")).strip())
        if not normalized:
            continue
        normalized_url, account, status_id = normalized
        if account.lower() != target:
            continue
        if status_id in deduped:
            continue

        text = str(row.get("text", "")).strip()
        social_context = str(row.get("social_context", "")).strip()
        metrics_labels_raw = row.get("metrics_labels", [])
        metrics_labels = []
        if isinstance(metrics_labels_raw, list):
            metrics_labels = [str(v) for v in metrics_labels_raw if str(v).strip()]

        has_quote = bool(row.get("has_quote", False))
        has_media = bool(row.get("has_media", False))
        dt_local = parse_iso_datetime(str(row.get("datetime", "")).strip())

        post = Post(
            status_id=status_id,
            account=account,
            url=normalized_url,
            datetime_raw=str(row.get("datetime", "")).strip(),
            datetime_local=dt_local,
            text=text,
            social_context=social_context,
            has_quote=has_quote,
            has_media=has_media,
            replies=metric_from_labels(metrics_labels, "replies"),
            reposts=metric_from_labels(metrics_labels, "reposts"),
            likes=metric_from_labels(metrics_labels, "likes"),
            views=metric_from_labels(metrics_labels, "views"),
            type_label=classify_type(text=text, social_context=social_context, has_quote=has_quote),
            language=detect_language(text),
        )
        deduped[status_id] = post

    posts = list(deduped.values())
    posts.sort(
        key=lambda p: (
            p.datetime_local is not None,
            p.datetime_local or dt.datetime.min.replace(tzinfo=dt.timezone.utc),
            p.status_id,
        ),
        reverse=True,
    )
    return posts


def format_int(value: int) -> str:
    return f"{value:,}"


def format_avg(value: float) -> str:
    rendered = f"{value:.1f}"
    if rendered.endswith(".0"):
        return rendered[:-2]
    return rendered


def percent(part: int, total: int) -> str:
    if total <= 0:
        return "0.0%"
    return f"{(part / total) * 100:.1f}%"


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


def date_label_ja(value: dt.date) -> str:
    return f"{value.year}年{value.month}月{value.day}日"


def highlight_datetime_label(post: Post) -> str:
    if post.datetime_local is None:
        return "不明"
    utc_value = post.datetime_local.astimezone(dt.timezone.utc)
    return utc_value.strftime("%a %b %d %H:%M:%S +0000 %Y")


def post_time_label(post: Post) -> str:
    if post.datetime_local is None:
        return "--:--"
    return post.datetime_local.strftime("%H:%M")


def collect_topics(posts: List[Post], limit: int = 5) -> List[Tuple[str, int]]:
    counts: Counter[str] = Counter()
    for post in posts:
        text = post.text
        for mention in re.findall(r"@[A-Za-z0-9_]{2,}", text):
            counts[mention] += 1
        for hashtag in re.findall(r"#[A-Za-z0-9_]{2,}", text):
            counts[hashtag] += 1
        for word in re.findall(r"[A-Za-z][A-Za-z0-9_-]{3,}", text.lower()):
            if word in STOP_WORDS:
                continue
            counts[word] += 1
    return counts.most_common(limit)


def flatten_text(text: str) -> str:
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    return " ".join(lines).strip()


def parse_model_json(content: str) -> Dict[str, Any]:
    if not content:
        return {}
    try:
        parsed = json.loads(content)
        if isinstance(parsed, dict):
            return parsed
    except json.JSONDecodeError:
        pass

    match = re.search(r"\{.*\}", content, re.DOTALL)
    if not match:
        return {}
    try:
        parsed = json.loads(match.group(0))
        if isinstance(parsed, dict):
            return parsed
    except json.JSONDecodeError:
        return {}
    return {}


def request_ja_translations(batch: List[Post]) -> Dict[str, str]:
    api_key = os.getenv("OPENAI_API_KEY", "").strip()
    if not api_key:
        return {}

    base_url = os.getenv("OPENAI_BASE_URL", "https://api.openai.com").strip().rstrip("/")
    if base_url.endswith("/v1"):
        endpoint = f"{base_url}/chat/completions"
    else:
        endpoint = f"{base_url}/v1/chat/completions"
    model = os.getenv("OPENAI_TRANSLATION_MODEL", "gpt-4.1-mini").strip() or "gpt-4.1-mini"

    payload_items = [
        {
            "status_id": post.status_id,
            "text": post.text,
        }
        for post in batch
    ]
    user_prompt = (
        "以下の投稿テキストを自然な日本語に翻訳してください。"
        "URL・@メンション・#タグは保持し、要約せず、意味を変えないでください。"
        "JSONのみで返し、形式は {\"translations\":[{\"status_id\":\"...\",\"ja\":\"...\"}]} としてください。\n"
        + json.dumps(payload_items, ensure_ascii=False)
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
        endpoint,
        "-H",
        f"Authorization: Bearer {api_key}",
        "-H",
        "Content-Type: application/json",
        "-d",
        json.dumps(body, ensure_ascii=False),
    ]
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
    except Exception:
        return {}
    if result.returncode != 0:
        return {}
    raw = result.stdout

    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        return {}

    if isinstance(parsed, dict) and "error" in parsed:
        return {}

    choices = parsed.get("choices", [])
    if not isinstance(choices, list) or not choices:
        return {}

    message = choices[0].get("message", {})
    content = message.get("content", "")
    content_dict = parse_model_json(str(content))
    items = content_dict.get("translations", [])
    if not isinstance(items, list):
        return {}

    out: Dict[str, str] = {}
    for row in items:
        if not isinstance(row, dict):
            continue
        sid = str(row.get("status_id", "")).strip()
        ja = flatten_text(str(row.get("ja", "")).strip())
        if sid and ja:
            out[sid] = ja
    return out


def translate_text_via_google(text: str) -> str:
    if not text.strip():
        return ""
    endpoint = (
        "https://translate.googleapis.com/translate_a/single"
        f"?client=gtx&sl=auto&tl=ja&dt=t&q={quote(text, safe='')}"
    )
    cmd = ["curl", "-sS", "--max-time", "8", endpoint]
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
    except Exception:
        return ""
    if result.returncode != 0:
        return ""
    try:
        payload = json.loads(result.stdout)
    except json.JSONDecodeError:
        return ""
    if not isinstance(payload, list) or not payload:
        return ""
    parts = payload[0]
    if not isinstance(parts, list):
        return ""
    translated_chunks: List[str] = []
    for chunk in parts:
        if not isinstance(chunk, list) or not chunk:
            continue
        piece = str(chunk[0]).strip()
        if piece:
            translated_chunks.append(piece)
    return flatten_text("".join(translated_chunks))


def build_ja_translations(posts: List[Post]) -> Dict[str, str]:
    translations: Dict[str, str] = {}
    pending: List[Post] = []

    for post in posts:
        text = flatten_text(post.text)
        if not text:
            translations[post.status_id] = "（本文なし）"
            continue
        if post.language == "日本語":
            translations[post.status_id] = text
            continue
        pending.append(post)

    for batch in chunked(pending, 8):
        got = request_ja_translations(batch)
        translations.update(got)

    unresolved = [post for post in pending if post.status_id not in translations]
    if unresolved:
        with concurrent.futures.ThreadPoolExecutor(max_workers=6) as executor:
            futures = {
                executor.submit(translate_text_via_google, post.text): post.status_id
                for post in unresolved
            }
            for future in concurrent.futures.as_completed(futures):
                sid = futures[future]
                try:
                    ja = future.result()
                except Exception:
                    ja = ""
                translations[sid] = ja if ja else "（和訳取得失敗）"

    return translations


def render_archive(
    posts: List[Post],
    output_path: pathlib.Path,
    username: str,
    display_name: str,
    source_url: str,
    resolved_url: str,
    collected_at: str,
    screenshot_file: str,
) -> Dict[str, int]:
    try:
        collected_dt = dt.datetime.strptime(collected_at, "%Y-%m-%dT%H:%M:%S%z")
    except ValueError:
        collected_dt = dt.datetime.now().astimezone()
    collected_local = collected_dt.astimezone()

    dated_posts = [p for p in posts if p.datetime_local is not None]
    if dated_posts:
        since_date = min(p.datetime_local.date() for p in dated_posts)
        until_date = max(p.datetime_local.date() for p in dated_posts)
    else:
        since_date = collected_local.date()
        until_date = collected_local.date()

    total = len(posts)
    reply_count = sum(1 for p in posts if p.type_label == "リプライ")
    quote_count = sum(1 for p in posts if p.type_label == "引用RT")
    normal_count = sum(1 for p in posts if p.type_label == "投稿")
    tweet_count = normal_count + quote_count

    likes_total = sum(p.likes for p in posts)
    reposts_total = sum(p.reposts for p in posts)
    replies_total = sum(p.replies for p in posts)
    likes_max = max((p.likes for p in posts), default=0)
    reposts_max = max((p.reposts for p in posts), default=0)
    replies_max = max((p.replies for p in posts), default=0)

    language_counts = Counter(p.language for p in posts)
    language_order = ["英語", "中国語", "日本語", "その他"]

    grouped: Dict[dt.date, List[Post]] = defaultdict(list)
    for post in posts:
        key = post.datetime_local.date() if post.datetime_local else collected_local.date()
        grouped[key].append(post)
    for items in grouped.values():
        items.sort(
            key=lambda p: (
                p.datetime_local is not None,
                p.datetime_local or dt.datetime.min.replace(tzinfo=dt.timezone.utc),
                p.status_id,
            ),
            reverse=True,
        )

    highlights = sorted(
        posts,
        key=lambda p: (
            p.likes,
            p.datetime_local or dt.datetime.min.replace(tzinfo=dt.timezone.utc),
            p.status_id,
        ),
        reverse=True,
    )[:10]

    topic_rows = collect_topics(posts)
    ja_translations = build_ja_translations(posts)
    collected_local_label = collected_local.strftime("%Y-%m-%d %H:%M")

    lines: List[str] = []
    lines.extend(
        [
            "---",
            "tags:",
            "  - x-account-archive",
            f"  - {username}",
            f"username: {username}",
            f"display_name: {display_name}",
            f"collected_at: {collected_at}",
            "period:",
            f"  since: {since_date}",
            f"  until: {until_date}",
            "stats:",
            f"  total: {total}",
            f"  tweets: {tweet_count}",
            f"  replies: {reply_count}",
            "source: agent-browser",
            "---",
            "",
            f"# @{username} 投稿アーカイブ",
            "",
            "## 概要",
            "",
            "| 項目 | 値 |",
            "|------|-----|",
            f"| アカウント | [@{username}]({source_url}) |",
            f"| 表示名 | {display_name} |",
            f"| 収集日時 | {collected_local_label} |",
            f"| 対象期間 | {since_date} 〜 {until_date} |",
            f"| 投稿数 | {total}件（通常: {tweet_count}, リプライ: {reply_count}） |",
            "",
            "---",
            "",
            "## ハイライト投稿（Top 10）",
            "",
            "エンゲージメント上位の投稿:",
            "",
        ]
    )

    if not highlights:
        lines.append("- 投稿データなし")
        lines.append("")
    else:
        for idx, post in enumerate(highlights, 1):
            lines.extend(
                [
                    f"### {idx}. {post.likes} いいね",
                    "",
                    f"**日時**: {highlight_datetime_label(post)}",
                    f"**URL**: {post.url}",
                    "",
                    *quote_lines(post.text),
                    "",
                    f"**日本語訳**: {ja_translations.get(post.status_id, '（和訳取得失敗）')}",
                    "",
                    "---",
                    "",
                ]
            )

    lines.extend(
        [
            "## 投稿一覧（日付別）",
            "",
        ]
    )

    for day in sorted(grouped.keys(), reverse=True):
        lines.extend([f"### {date_label_ja(day)}", ""])
        for post in grouped[day]:
            lines.extend(
                [
                    f"#### {post_time_label(post)} - {post.likes} いいね",
                    "",
                    f"**URL**: {post.url}",
                    f"**種別**: {post.type_label}",
                ]
            )
            if post.has_media:
                lines.append("**メディア**: あり")
            lines.extend(
                [
                    "",
                    *quote_lines(post.text),
                    "",
                    f"**日本語訳**: {ja_translations.get(post.status_id, '（和訳取得失敗）')}",
                    "",
                    "---",
                    "",
                ]
            )

    lines.extend(
        [
            "## 統計",
            "",
            "### 言語別分布",
            "",
            "| 言語 | 件数 | 割合 |",
            "|------|------|------|",
        ]
    )
    for language in language_order:
        count = language_counts.get(language, 0)
        if count == 0:
            continue
        lines.append(f"| {language} | {count} | {percent(count, total)} |")
    if total == 0:
        lines.append("| - | 0 | 0.0% |")

    lines.extend(
        [
            "",
            "### エンゲージメント統計",
            "",
            "| 指標 | 合計 | 平均 | 最大 |",
            "|------|------|------|------|",
            f"| いいね | {format_int(likes_total)} | {format_avg(likes_total / total) if total else '0'} | {format_int(likes_max)} |",
            f"| RT | {format_int(reposts_total)} | {format_avg(reposts_total / total) if total else '0'} | {format_int(reposts_max)} |",
            f"| リプライ | {format_int(replies_total)} | {format_avg(replies_total / total) if total else '0'} | {format_int(replies_max)} |",
            "",
            "### 投稿タイプ分布",
            "",
            "| タイプ | 件数 | 割合 |",
            "|------|------|------|",
            f"| 通常投稿 | {normal_count} | {percent(normal_count, total)} |",
            f"| リプライ | {reply_count} | {percent(reply_count, total)} |",
            f"| 引用RT | {quote_count} | {percent(quote_count, total)} |",
            "",
            "### 主要トピック",
            "",
        ]
    )

    if not topic_rows:
        lines.append("1. **データ不足** - トピック抽出対象の投稿が不足")
    else:
        for idx, (token, count) in enumerate(topic_rows, 1):
            lines.append(f"{idx}. **{token}** - {count}件で出現")

    lines.extend(
        [
            "",
            "---",
            "",
            "## 収集ログ",
            "",
            f"- 開始: {since_date}",
            f"- 終了: {collected_local_label}",
            "- 収集方法: agent-browser (X timeline extraction)",
            "- エラー: なし",
            f"- 中間ファイル: `{output_path.name}`",
            f"- スクリーンショット: `{screenshot_file}`",
            f"- 解決URL: {resolved_url}",
            "",
        ]
    )

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text("\n".join(lines), encoding="utf-8")

    return {
        "post_count": total,
        "tweet_count": tweet_count,
        "reply_count": reply_count,
        "normal_count": normal_count,
        "quote_count": quote_count,
    }


def main() -> None:
    args = parse_args()
    input_path = pathlib.Path(args.input_json)
    output_path = pathlib.Path(args.output)
    display_name = parse_display_name_from_title(args.title, args.username)
    posts = load_posts(input_path=input_path, username=args.username)
    summary = render_archive(
        posts=posts,
        output_path=output_path,
        username=args.username,
        display_name=display_name,
        source_url=args.source_url,
        resolved_url=args.resolved_url,
        collected_at=args.collected_at,
        screenshot_file=args.screenshot_file,
    )
    print(f"post_count={summary['post_count']}")
    print(f"tweet_count={summary['tweet_count']}")
    print(f"reply_count={summary['reply_count']}")


if __name__ == "__main__":
    main()
