#!/usr/bin/env python3
import argparse
import datetime as dt
import pathlib
import re
from typing import Dict, List


TZ_TOKYO = dt.timezone(dt.timedelta(hours=9))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Rebuild daily post summary from existing crm/posts/<handle>/YYYYMMDD.md files."
        )
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
        "--date",
        required=True,
        help="Target date in YYYY-MM-DD.",
    )
    parser.add_argument(
        "--output",
        default="",
        help="Optional summary markdown output path.",
    )
    parser.add_argument(
        "--require-all",
        action="store_true",
        help="Exit with code 2 if any handle is missing YYYYMMDD.md.",
    )
    return parser.parse_args()


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


def resolve_posts_dir(accounts_dir: pathlib.Path, handle: str) -> pathlib.Path:
    dir_style = accounts_dir / handle
    file_style = accounts_dir / f"{handle}.md"
    if dir_style.is_dir():
        return dir_style / "posts"
    if file_style.is_file():
        return accounts_dir.parent / "posts" / handle
    return dir_style / "posts"


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


def main() -> None:
    args = parse_args()
    accounts_dir = pathlib.Path(args.accounts_dir).resolve()
    if not accounts_dir.exists():
        raise SystemExit(f"accounts dir not found: {accounts_dir}")

    target_date = dt.date.fromisoformat(args.date)
    target_tag = target_date.strftime("%Y%m%d")
    crm_dir = accounts_dir.parent
    summary_dir = crm_dir / "daily_post_reports"
    summary_dir.mkdir(parents=True, exist_ok=True)
    summary_path = (
        pathlib.Path(args.output).resolve()
        if args.output.strip()
        else summary_dir / f"{target_tag}_summary.md"
    )

    handles = list_handles(accounts_dir)
    rows: List[Dict[str, object]] = []
    for handle in handles:
        out_md = resolve_posts_dir(accounts_dir, handle) / f"{target_tag}.md"
        exists = out_md.exists()
        rows.append(
            {
                "handle": handle,
                "count": read_existing_total(out_md) if exists else 0,
                "file": out_md,
                "exists": exists,
            }
        )

    success = sum(1 for row in rows if bool(row["exists"]))
    failure = len(rows) - success

    lines: List[str] = []
    lines.extend(
        [
            "---",
            f"owner: {args.owner}",
            f"target_date: {target_date.isoformat()}",
            f"generated_at: {dt.datetime.now(TZ_TOKYO).isoformat()}",
            f"accounts_total: {len(rows)}",
            f"success_count: {success}",
            f"failure_count: {failure}",
            "---",
            "",
            f"# @{args.owner} フォロー先 本日投稿収集レポート",
            "",
            f"- 対象日: {target_date.isoformat()}",
            f"- 成功: {success}",
            f"- 失敗: {failure}",
            "",
            "## 投稿件数",
            "",
            "| Handle | 本日投稿数 | ファイル |",
            "|---|---:|---|",
        ]
    )

    # Sort table rows by today's post count (desc), then handle (asc) for stable output.
    display_rows = sorted(
        rows,
        key=lambda row: (-int(row["count"]), str(row["handle"]).lower()),
    )

    for row in display_rows:
        rel = pathlib.Path(str(row["file"])).relative_to(crm_dir)
        lines.append(f"| @{row['handle']} | {row['count']} | `{rel}` |")

    missing_handles = [str(row["handle"]) for row in rows if not bool(row["exists"])]
    if missing_handles:
        lines.extend(["", "## 失敗一覧", ""])
        for handle in missing_handles:
            lines.append(f"- {handle}: missing {target_tag}.md")

    lines.append("")
    summary_path.write_text("\n".join(lines), encoding="utf-8")

    print(f"summary_file={summary_path}")
    print(f"accounts_total={len(rows)}")
    print(f"success_count={success}")
    print(f"failure_count={failure}")
    if missing_handles and args.require_all:
        raise SystemExit(2)


if __name__ == "__main__":
    main()
