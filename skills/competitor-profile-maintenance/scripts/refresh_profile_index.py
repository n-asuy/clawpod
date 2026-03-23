#!/usr/bin/env python3
import argparse
import datetime as dt
import pathlib
import re
from typing import Dict, List, Optional, Tuple


RAW_PATTERN = re.compile(r"^(\d{8})_(\d{4})_.*_raw\.md$")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Refresh profile index.md frontmatter using files in raw/."
    )
    parser.add_argument("--profile-dir", required=True, help="Profile directory path")
    parser.add_argument("--username", required=True, help="Account username/ID")
    parser.add_argument("--display-name", required=True, help="Display name")
    parser.add_argument("--platform", default="x", help="Platform label (default: x)")
    parser.add_argument("--url", required=True, help="Profile URL")
    parser.add_argument("--watch-priority", default="medium", help="Watch priority label")
    return parser.parse_args()


def get_raw_files(raw_dir: pathlib.Path) -> List[pathlib.Path]:
    return sorted([p for p in raw_dir.glob("*_raw.md") if p.is_file()])


def parse_raw_timestamp(path: pathlib.Path) -> Optional[dt.datetime]:
    m = RAW_PATTERN.match(path.name)
    if not m:
        return None
    ymd, hm = m.group(1), m.group(2)
    try:
        return dt.datetime.strptime(f"{ymd}{hm}", "%Y%m%d%H%M")
    except ValueError:
        return None


def extract_latest_date(raw_files: List[pathlib.Path]) -> Optional[str]:
    latest: Optional[dt.datetime] = None
    for path in raw_files:
        current = parse_raw_timestamp(path)
        if current is None:
            continue
        if latest is None or current > latest:
            latest = current
    return latest.strftime("%Y-%m-%d") if latest else None


def get_latest_raw_file(raw_files: List[pathlib.Path]) -> Optional[pathlib.Path]:
    latest: Optional[Tuple[dt.datetime, pathlib.Path]] = None
    for path in raw_files:
        ts = parse_raw_timestamp(path)
        if ts is None:
            continue
        if latest is None or ts > latest[0]:
            latest = (ts, path)
    return latest[1] if latest else None


def parse_collection_count_from_raw(raw_path: Optional[pathlib.Path]) -> Optional[int]:
    if raw_path is None or not raw_path.exists():
        return None
    text = raw_path.read_text(encoding="utf-8")

    post_count_match = re.search(r"^post_count:\s*(\d+)\s*$", text, flags=re.MULTILINE)
    if post_count_match:
        return int(post_count_match.group(1))

    in_stats = False
    for line in text.splitlines():
        if line.strip() == "stats:":
            in_stats = True
            continue
        if in_stats and not line.startswith("  "):
            break
        if in_stats:
            stripped = line.strip()
            if stripped.startswith("total:"):
                value = stripped.split(":", 1)[1].strip()
                if value.isdigit():
                    return int(value)
    return None


def split_frontmatter(content: str) -> Tuple[Optional[List[str]], str]:
    if not content.startswith("---\n"):
        return None, content
    end = content.find("\n---\n", 4)
    if end == -1:
        return None, content
    frontmatter = content[4:end].splitlines()
    body = content[end + 5 :]
    return frontmatter, body


def upsert_frontmatter(
    existing: Optional[List[str]], updates: Dict[str, str]
) -> List[str]:
    if existing is None:
        lines: List[str] = []
    else:
        lines = list(existing)

    positions: Dict[str, int] = {}
    for i, line in enumerate(lines):
        if not line.strip() or line.startswith(" ") or ":" not in line:
            continue
        key = line.split(":", 1)[0].strip()
        positions[key] = i

    for key, value in updates.items():
        new_line = f"{key}: {value}"
        if key in positions:
            lines[positions[key]] = new_line
        else:
            lines.append(new_line)

    return lines


def default_body(display_name: str, username: str) -> str:
    return (
        f"# {display_name} (@{username})\n\n"
        "## 概要\n\n"
        "- TODO: プロファイル概要を記入\n\n"
        "## 最新収集\n\n"
        "- `raw/`: 一次取得データ\n"
        "- `log/`: 取得ログとスクリーンショット\n"
    )


def main() -> None:
    args = parse_args()
    profile_dir = pathlib.Path(args.profile_dir)
    raw_dir = profile_dir / "raw"
    index_path = profile_dir / "index.md"

    raw_dir.mkdir(parents=True, exist_ok=True)
    profile_dir.mkdir(parents=True, exist_ok=True)

    raw_files = get_raw_files(raw_dir)
    latest = extract_latest_date(raw_files)
    latest_raw_file = get_latest_raw_file(raw_files)
    collection_count_from_raw = parse_collection_count_from_raw(latest_raw_file)
    collection_count_value = (
        collection_count_from_raw
        if collection_count_from_raw is not None
        else len(raw_files)
    )
    today = dt.date.today().strftime("%Y-%m-%d")

    if index_path.exists():
        original = index_path.read_text(encoding="utf-8")
        fm, body = split_frontmatter(original)
    else:
        original = ""
        fm, body = None, ""

    created_value = today
    if fm:
        for line in fm:
            if line.startswith("created:"):
                created_value = line.split(":", 1)[1].strip() or today
                break

    updates = {
        "username": args.username,
        "display_name": args.display_name,
        "platform": args.platform,
        "url": args.url,
        "watch_priority": args.watch_priority,
        "last_collected": latest or "n/a",
        "collection_count": str(collection_count_value),
        "created": created_value,
        "updated": today,
    }

    new_fm = upsert_frontmatter(fm, updates)
    final_body = body.strip("\n")
    if not final_body:
        final_body = default_body(args.display_name, args.username)

    rendered = "---\n" + "\n".join(new_fm) + "\n---\n\n" + final_body + "\n"
    if rendered != original:
        index_path.write_text(rendered, encoding="utf-8")

    print(f"index_file={index_path}")
    print(f"collection_count={collection_count_value}")
    print(f"last_collected={latest or 'n/a'}")


if __name__ == "__main__":
    main()
