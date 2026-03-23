#!/usr/bin/env python3
import argparse
import pathlib
import re
import sys
from typing import Dict, List


TRANSLATION_BLOCK_MARKERS = ("**和訳（手動）**", "**和訳**")
DEFAULT_PLACEHOLDER_TEXT = "（ここに和訳を記入）"
DEFAULT_FAILURE_PLACEHOLDER_TEXT = "（和訳取得失敗）"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Validate that Japanese translations are filled for all collected daily post files."
        )
    )
    parser.add_argument(
        "--summary-file",
        required=True,
        help="Path to crm/daily_post_reports/YYYYMMDD_summary.md",
    )
    parser.add_argument(
        "--placeholder-text",
        default=DEFAULT_PLACEHOLDER_TEXT,
        help="Placeholder text that indicates untranslated content.",
    )
    parser.add_argument(
        "--failure-placeholder-text",
        default=DEFAULT_FAILURE_PLACEHOLDER_TEXT,
        help="Failure placeholder text that indicates unresolved translation.",
    )
    return parser.parse_args()


def parse_post_files_from_summary(summary_text: str) -> List[pathlib.Path]:
    rel_paths = re.findall(r"`([^`]+\.md)`", summary_text)
    out: List[pathlib.Path] = []
    seen = set()
    for rel in rel_paths:
        rel_path = pathlib.Path(rel)
        if rel_path in seen:
            continue
        seen.add(rel_path)
        out.append(rel_path)
    return out


def analyze_translation_blocks(
    text: str, placeholder_texts: List[str]
) -> Dict[str, int]:
    lines = text.splitlines()
    post_count = sum(1 for line in lines if line.startswith("### "))
    block_positions = [
        idx
        for idx, line in enumerate(lines)
        if line.strip() in TRANSLATION_BLOCK_MARKERS
    ]

    placeholder_hits = 0
    for marker in sorted(set(p for p in placeholder_texts if p.strip())):
        placeholder_hits += text.count(marker)
    empty_blocks = 0
    for pos in block_positions:
        cursor = pos + 1
        while cursor < len(lines) and not lines[cursor].strip():
            cursor += 1
        if cursor >= len(lines):
            empty_blocks += 1
            continue
        candidate = lines[cursor].strip()
        if candidate.startswith(">"):
            candidate = candidate[1:].strip()
        if not candidate:
            empty_blocks += 1

    missing_blocks = max(0, post_count - len(block_positions))
    return {
        "post_count": post_count,
        "translation_blocks": len(block_positions),
        "missing_blocks": missing_blocks,
        "placeholder_hits": placeholder_hits,
        "empty_blocks": empty_blocks,
    }


def main() -> None:
    args = parse_args()
    summary_file = pathlib.Path(args.summary_file).resolve()
    if not summary_file.exists():
        raise SystemExit(f"summary file not found: {summary_file}")

    crm_dir = summary_file.parent.parent
    summary_text = summary_file.read_text(encoding="utf-8")
    rel_post_files = parse_post_files_from_summary(summary_text)
    if not rel_post_files:
        raise SystemExit(f"no post markdown paths found in summary: {summary_file}")

    missing_files: List[pathlib.Path] = []
    problems: List[str] = []
    checked = 0
    total_posts = 0
    total_placeholders = 0
    total_missing_blocks = 0
    total_empty_blocks = 0
    placeholder_texts = [args.placeholder_text, args.failure_placeholder_text]

    for rel_path in rel_post_files:
        post_file = (crm_dir / rel_path).resolve()
        if not post_file.exists():
            missing_files.append(post_file)
            continue

        text = post_file.read_text(encoding="utf-8")
        stats = analyze_translation_blocks(text, placeholder_texts)
        checked += 1
        total_posts += stats["post_count"]
        total_placeholders += stats["placeholder_hits"]
        total_missing_blocks += stats["missing_blocks"]
        total_empty_blocks += stats["empty_blocks"]

        if (
            stats["missing_blocks"] > 0
            or stats["placeholder_hits"] > 0
            or stats["empty_blocks"] > 0
        ):
            problems.append(
                (
                    f"{post_file}: posts={stats['post_count']}, "
                    f"translation_blocks={stats['translation_blocks']}, "
                    f"missing_blocks={stats['missing_blocks']}, "
                    f"placeholder_hits={stats['placeholder_hits']}, "
                    f"empty_blocks={stats['empty_blocks']}"
                )
            )

    print(f"summary_file={summary_file}")
    print(f"checked_files={checked}")
    print(f"total_posts={total_posts}")
    print(f"total_placeholder_hits={total_placeholders}")
    print(f"total_missing_blocks={total_missing_blocks}")
    print(f"total_empty_blocks={total_empty_blocks}")
    print(f"missing_file_count={len(missing_files)}")
    print(f"problem_file_count={len(problems)}")

    if missing_files:
        print("missing_files:", file=sys.stderr)
        for path in missing_files:
            print(f"- {path}", file=sys.stderr)
    if problems:
        print("translation_issues:", file=sys.stderr)
        for line in problems:
            print(f"- {line}", file=sys.stderr)

    if missing_files or problems:
        raise SystemExit(2)

    print("translation_check=passed")


if __name__ == "__main__":
    main()
