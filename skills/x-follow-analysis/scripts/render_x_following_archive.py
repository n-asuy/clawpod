#!/usr/bin/env python3
import argparse
import json
import pathlib
import re
from typing import Any, Dict, List


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Render X following JSON to markdown archive format."
    )
    parser.add_argument("--input-json", required=True, help="Input raw JSON path")
    parser.add_argument("--output", required=True, help="Output markdown path")
    parser.add_argument("--username", required=True, help="Target username")
    parser.add_argument("--source-url", required=True, help="Requested URL")
    parser.add_argument("--resolved-url", required=True, help="Resolved page URL")
    parser.add_argument("--title", required=True, help="Page title")
    parser.add_argument("--collected-at", required=True, help="Collection timestamp")
    parser.add_argument("--screenshot-file", required=True, help="Screenshot file name")
    return parser.parse_args()


def compact_text(value: Any) -> str:
    text = str(value or "").replace("\u00a0", " ")
    text = re.sub(r"\s+", " ", text).strip()
    return text


def md_cell(value: Any, max_len: int = 180) -> str:
    text = compact_text(value).replace("|", "\\|")
    if len(text) > max_len:
        return text[: max_len - 1].rstrip() + "…"
    return text


def load_rows(path: pathlib.Path) -> List[Dict[str, Any]]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return []
    if not isinstance(payload, list):
        return []

    deduped: Dict[str, Dict[str, Any]] = {}
    for raw in payload:
        if not isinstance(raw, dict):
            continue
        username = compact_text(raw.get("username", "")).lstrip("@")
        if not re.fullmatch(r"[A-Za-z0-9_]{1,15}", username):
            continue
        key = username.lower()
        if key in deduped:
            continue

        row = {
            "username": username,
            "display_name": compact_text(raw.get("display_name", username)) or username,
            "bio": compact_text(raw.get("bio", "")),
            "profile_url": compact_text(raw.get("profile_url", f"https://x.com/{username}")),
            "follows_you": bool(raw.get("follows_you", False)),
            "verified": bool(raw.get("verified", False)),
            "protected": bool(raw.get("protected", False)),
            "button_text": compact_text(raw.get("button_text", "")),
            "card_text": compact_text(raw.get("card_text", "")),
        }
        deduped[key] = row

    rows = list(deduped.values())
    rows.sort(key=lambda x: (not x["follows_you"], x["username"].lower()))
    return rows


def render_markdown(
    rows: List[Dict[str, Any]],
    args: argparse.Namespace,
    output_path: pathlib.Path,
) -> None:
    total_accounts = len(rows)
    follows_you_count = sum(1 for row in rows if row["follows_you"])
    verified_count = sum(1 for row in rows if row["verified"])
    protected_count = sum(1 for row in rows if row["protected"])
    with_bio_count = sum(1 for row in rows if row["bio"])

    follows_you_handles = [f"@{row['username']}" for row in rows if row["follows_you"]][:30]

    lines: List[str] = []
    lines.append("---")
    lines.append(f"collected_at: {args.collected_at}")
    lines.append("platform: x")
    lines.append(f"username: {args.username}")
    lines.append(f"source_url: {args.source_url}")
    lines.append(f"resolved_url: {args.resolved_url}")
    lines.append(f"title: {args.title}")
    lines.append(f"screenshot_file: {args.screenshot_file}")
    lines.append(f"total_accounts: {total_accounts}")
    lines.append(f"follows_you_count: {follows_you_count}")
    lines.append(f"verified_count: {verified_count}")
    lines.append(f"protected_count: {protected_count}")
    lines.append(f"with_bio_count: {with_bio_count}")
    lines.append("---")
    lines.append("")
    lines.append(f"# X Following Archive (@{args.username})")
    lines.append("")
    lines.append("## Summary")
    lines.append("")
    lines.append(f"- total_accounts: {total_accounts}")
    lines.append(f"- follows_you_count: {follows_you_count}")
    lines.append(f"- verified_count: {verified_count}")
    lines.append(f"- protected_count: {protected_count}")
    lines.append(f"- with_bio_count: {with_bio_count}")
    lines.append("")
    lines.append("## Follows You (Top 30)")
    lines.append("")
    if follows_you_handles:
        lines.append("- " + ", ".join(follows_you_handles))
    else:
        lines.append("- (none)")
    lines.append("")
    lines.append("## Accounts")
    lines.append("")
    lines.append("| username | display_name | follows_you | verified | protected | bio |")
    lines.append("| --- | --- | --- | --- | --- | --- |")

    for row in rows:
        lines.append(
            "| "
            + " | ".join(
                [
                    f"@{md_cell(row['username'])}",
                    md_cell(row["display_name"]),
                    "yes" if row["follows_you"] else "no",
                    "yes" if row["verified"] else "no",
                    "yes" if row["protected"] else "no",
                    md_cell(row["bio"]),
                ]
            )
            + " |"
        )

    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> None:
    args = parse_args()
    input_path = pathlib.Path(args.input_json)
    output_path = pathlib.Path(args.output)

    rows = load_rows(input_path)
    render_markdown(rows, args, output_path)

    follows_you_count = sum(1 for row in rows if row["follows_you"])
    verified_count = sum(1 for row in rows if row["verified"])
    protected_count = sum(1 for row in rows if row["protected"])
    with_bio_count = sum(1 for row in rows if row["bio"])

    print(f"total_accounts={len(rows)}")
    print(f"follows_you_count={follows_you_count}")
    print(f"verified_count={verified_count}")
    print(f"protected_count={protected_count}")
    print(f"with_bio_count={with_bio_count}")


if __name__ == "__main__":
    main()
