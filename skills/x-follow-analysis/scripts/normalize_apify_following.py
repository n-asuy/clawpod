#!/usr/bin/env python3
"""Normalize Apify actor output to x-follow-analysis canonical schema.

Supports three known actor output formats:
  - apidojo/twitter-following-scraper
  - microworlds/twitter-followers-following-scraper
  - web_scraping_pro/twitter-following-list-scraper

Input: JSONL (one JSON object per line) or JSON array file.
Output: JSON array in canonical snake_case schema.
"""
import argparse
import datetime as dt
import json
import pathlib
import re
from typing import Any, Dict, List, Optional


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Normalize Apify following output to canonical schema."
    )
    parser.add_argument(
        "--input-jsonl", required=True, help="Input JSONL or JSON array file from Apify"
    )
    parser.add_argument(
        "--output", required=True, help="Output JSON array path (canonical schema)"
    )
    return parser.parse_args()


def compact_text(value: Any) -> str:
    text = str(value or "").replace("\u00a0", " ")
    return re.sub(r"\s+", " ", text).strip()


def normalized_handle(value: Any) -> str:
    handle = compact_text(value).lstrip("@")
    if not re.fullmatch(r"[A-Za-z0-9_]{1,15}", handle):
        return ""
    return handle


def safe_int(value: Any) -> Optional[int]:
    if value is None:
        return None
    try:
        return int(value)
    except (ValueError, TypeError):
        return None


def extract_username(raw: Dict[str, Any]) -> str:
    """Try each actor's username field in priority order."""
    for key in ("screen_name", "userName", "username"):
        val = normalized_handle(raw.get(key, ""))
        if val:
            return val
    return ""


def extract_display_name(raw: Dict[str, Any], fallback: str) -> str:
    for key in ("name", "displayName", "display_name"):
        val = compact_text(raw.get(key, ""))
        if val:
            return val
    return fallback


def extract_bio(raw: Dict[str, Any]) -> str:
    for key in ("description", "bio"):
        val = compact_text(raw.get(key, ""))
        if val:
            return val
    return ""


def extract_followers_count(raw: Dict[str, Any]) -> Optional[int]:
    for key in ("followers_count", "followersCount", "followers"):
        val = safe_int(raw.get(key))
        if val is not None:
            return val
    return None


def extract_following_count(raw: Dict[str, Any]) -> Optional[int]:
    for key in ("friends_count", "followingCount", "following"):
        val = safe_int(raw.get(key))
        if val is not None:
            return val
    return None


def extract_verified(raw: Dict[str, Any]) -> bool:
    for key in ("verified", "isVerified"):
        val = raw.get(key)
        if val is not None:
            return bool(val)
    return False


def extract_avatar_url(raw: Dict[str, Any]) -> str:
    for key in ("profile_image_url", "profileImageUrl", "avatar"):
        val = compact_text(raw.get(key, ""))
        if val:
            return val
    return ""


def extract_profile_url(raw: Dict[str, Any], username: str) -> str:
    for key in ("profileUrl", "url", "profile_url"):
        val = compact_text(raw.get(key, ""))
        if val:
            return val
    return f"https://x.com/{username}"


def normalize_row(raw: Dict[str, Any], now_iso: str) -> Optional[Dict[str, Any]]:
    username = extract_username(raw)
    if not username:
        return None

    return {
        "username": username,
        "display_name": extract_display_name(raw, username),
        "bio": extract_bio(raw),
        "profile_url": extract_profile_url(raw, username),
        "follows_you": False,
        "verified": extract_verified(raw),
        "protected": False,
        "button_text": "",
        "avatar_url": extract_avatar_url(raw),
        "card_text": "",
        "captured_at": now_iso,
        "followers_count": extract_followers_count(raw),
        "following_count": extract_following_count(raw),
        "collection_source": "apify",
    }


def load_input(path: pathlib.Path) -> List[Dict[str, Any]]:
    """Load JSONL or JSON array file."""
    text = path.read_text(encoding="utf-8").strip()
    if not text:
        return []

    # Try JSON array first
    if text.startswith("["):
        try:
            payload = json.loads(text)
            if isinstance(payload, list):
                return [item for item in payload if isinstance(item, dict)]
        except json.JSONDecodeError:
            pass

    # Fall back to JSONL
    rows: List[Dict[str, Any]] = []
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
            if isinstance(obj, dict):
                rows.append(obj)
        except json.JSONDecodeError:
            continue
    return rows


def main() -> None:
    args = parse_args()
    input_path = pathlib.Path(args.input_jsonl)
    output_path = pathlib.Path(args.output)

    if not input_path.exists():
        raise SystemExit(f"Input file not found: {input_path}")

    raw_rows = load_input(input_path)
    now_iso = dt.datetime.now().isoformat()

    deduped: Dict[str, Dict[str, Any]] = {}
    for raw in raw_rows:
        normalized = normalize_row(raw, now_iso)
        if normalized is None:
            continue
        key = normalized["username"].lower()
        if key not in deduped:
            deduped[key] = normalized

    rows = sorted(deduped.values(), key=lambda x: x["username"].lower())

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(
        json.dumps(rows, ensure_ascii=False, indent=2), encoding="utf-8"
    )

    print(f"total_accounts={len(rows)}")
    print(f"output={output_path}")


if __name__ == "__main__":
    main()
