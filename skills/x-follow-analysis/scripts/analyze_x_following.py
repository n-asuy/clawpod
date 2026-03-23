#!/usr/bin/env python3
import argparse
import csv
import datetime as dt
import json
import pathlib
import re
from typing import Any, Dict, List, Optional, Sequence, Tuple


DEFAULT_CONFIG: Dict[str, Any] = {
    "always_keep_handles": [],
    "visibility_target_handles": [],
    "always_unfollow_handles": [],
    "keep_keywords": [],
    "visibility_keywords": [],
    "unfollow_keywords": [],
    "protect_follows_you": True,
    "keep_verified": False,
}

RAW_FILE_PATTERN = re.compile(
    r"^(?P<ts>\d{8}_\d{4})_x_(?P<username>[A-Za-z0-9_]{1,15})_following_raw(?:\.json)?$"
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Analyze X following snapshot and classify keep/review/unfollow candidates."
    )
    parser.add_argument("--input-json", required=True, help="Captured following raw JSON path")
    parser.add_argument(
        "--output-dir",
        default="",
        help="Output directory (default: sibling analysis/ directory)",
    )
    parser.add_argument(
        "--config",
        default="",
        help="Rule config JSON path (default: built-in defaults)",
    )
    parser.add_argument(
        "--min-keep-score",
        type=int,
        default=3,
        help="Minimum keep score for keep_follow_and_engage",
    )
    parser.add_argument(
        "--min-visibility-score",
        type=int,
        default=2,
        help="Minimum visibility score for engage_without_follow",
    )
    parser.add_argument(
        "--min-unfollow-score",
        type=int,
        default=4,
        help="Minimum unfollow score for engage_without_follow",
    )
    parser.add_argument(
        "--max-unfollow",
        type=int,
        default=200,
        help="Max handles written to *_unfollow_candidates.txt",
    )
    parser.add_argument(
        "--detail-limit",
        type=int,
        default=400,
        help="Max rows rendered in markdown detail table",
    )
    return parser.parse_args()


def compact_text(value: Any) -> str:
    text = str(value or "").replace("\u00a0", " ")
    return re.sub(r"\s+", " ", text).strip()


def md_cell(value: Any, max_len: int = 180) -> str:
    text = compact_text(value).replace("|", "\\|")
    if len(text) > max_len:
        return text[: max_len - 1].rstrip() + "…"
    return text


def normalized_handle(value: Any) -> str:
    handle = compact_text(value).lstrip("@")
    if not re.fullmatch(r"[A-Za-z0-9_]{1,15}", handle):
        return ""
    return handle


def parse_raw_name(path: pathlib.Path) -> Tuple[str, str]:
    match = RAW_FILE_PATTERN.match(path.name)
    if not match:
        ts = dt.datetime.now().strftime("%Y%m%d_%H%M")
        return ts, "unknown"
    return match.group("ts"), match.group("username")


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
        username = normalized_handle(raw.get("username", ""))
        if not username:
            continue
        key = username.lower()
        if key in deduped:
            continue

        followers_count = raw.get("followers_count")
        following_count = raw.get("following_count")

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
            "followers_count": int(followers_count) if followers_count is not None else None,
            "following_count": int(following_count) if following_count is not None else None,
            "collection_source": compact_text(raw.get("collection_source", "cdp")),
        }
        deduped[key] = row

    rows = list(deduped.values())
    rows.sort(key=lambda x: x["username"].lower())
    return rows


def load_config(path: Optional[pathlib.Path]) -> Dict[str, Any]:
    config = dict(DEFAULT_CONFIG)
    if not path:
        return config
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except Exception as exc:
        raise ValueError(f"Failed to read config JSON: {exc}") from exc

    if not isinstance(payload, dict):
        raise ValueError("Config file must be a JSON object.")

    for key in ("always_keep_handles", "visibility_target_handles", "always_unfollow_handles"):
        value = payload.get(key, [])
        if not isinstance(value, list):
            raise ValueError(f"Config field '{key}' must be a list.")
        config[key] = [normalized_handle(v) for v in value if normalized_handle(v)]

    for key in ("keep_keywords", "visibility_keywords", "unfollow_keywords"):
        value = payload.get(key, [])
        if not isinstance(value, list):
            raise ValueError(f"Config field '{key}' must be a list.")
        config[key] = [compact_text(v) for v in value if compact_text(v)]

    for key in ("protect_follows_you", "keep_verified"):
        value = payload.get(key, config[key])
        config[key] = bool(value)

    return config


def keyword_matches(text: str, keywords: Sequence[str]) -> List[str]:
    text_lower = text.lower()
    matched: List[str] = []
    seen = set()
    for keyword in keywords:
        normalized = compact_text(keyword)
        if not normalized:
            continue
        probe = normalized.lower()
        if probe in text_lower and probe not in seen:
            matched.append(normalized)
            seen.add(probe)
    return matched


def classify_row(
    row: Dict[str, Any],
    config: Dict[str, Any],
    min_keep_score: int,
    min_visibility_score: int,
    min_unfollow_score: int,
) -> Dict[str, Any]:
    handle = row["username"].lower()
    text_blob = " ".join(
        [row["username"], row["display_name"], row["bio"], row["card_text"]]
    ).lower()

    always_keep = {h.lower() for h in config["always_keep_handles"]}
    visibility_targets = {h.lower() for h in config["visibility_target_handles"]}
    always_unfollow = {h.lower() for h in config["always_unfollow_handles"]}

    keep_score = 0
    visibility_score = 0
    unfollow_score = 0

    keep_reasons: List[str] = []
    visibility_reasons: List[str] = []
    unfollow_reasons: List[str] = []

    if handle in always_keep:
        keep_score += 100
        keep_reasons.append("always_keep_handle")

    if config["protect_follows_you"] and row["follows_you"]:
        keep_score += 4
        keep_reasons.append("follows_you")

    if config["keep_verified"] and row["verified"]:
        keep_score += 2
        keep_reasons.append("verified")

    keep_hits = keyword_matches(text_blob, config["keep_keywords"])
    if keep_hits:
        keep_score += 2 * len(keep_hits)
        keep_reasons.append("keep_keywords:" + ",".join(keep_hits))

    if handle in visibility_targets:
        visibility_score += 100
        visibility_reasons.append("visibility_target_handle")

    visibility_hits = keyword_matches(text_blob, config["visibility_keywords"])
    if visibility_hits:
        visibility_score += 2 * len(visibility_hits)
        visibility_reasons.append("visibility_keywords:" + ",".join(visibility_hits))

    if handle in always_unfollow:
        unfollow_score += 100
        unfollow_reasons.append("always_unfollow_handle")

    unfollow_hits = keyword_matches(text_blob, config["unfollow_keywords"])
    if unfollow_hits:
        unfollow_score += 3 * len(unfollow_hits)
        unfollow_reasons.append("unfollow_keywords:" + ",".join(unfollow_hits))

    if visibility_score >= min_visibility_score and keep_score < min_keep_score:
        unfollow_score += 2
        unfollow_reasons.append("visibility_dominant")

    if handle in always_keep:
        bucket = "keep_follow_and_engage"
    elif handle in always_unfollow:
        bucket = "engage_without_follow"
    elif keep_score >= max(min_keep_score, visibility_score + 1):
        bucket = "keep_follow_and_engage"
    elif visibility_score >= min_visibility_score and keep_score < min_keep_score:
        bucket = "engage_without_follow"
    elif unfollow_score >= min_unfollow_score and keep_score < min_keep_score:
        bucket = "engage_without_follow"
    else:
        bucket = "manual_review"

    if bucket == "keep_follow_and_engage":
        reasons = keep_reasons
        delta = keep_score - max(visibility_score, unfollow_score)
    elif bucket == "engage_without_follow":
        reasons = visibility_reasons + unfollow_reasons
        delta = max(visibility_score, unfollow_score) - keep_score
    else:
        reasons = keep_reasons + visibility_reasons + unfollow_reasons
        delta = 0

    if not reasons:
        reasons = ["signal_weak"]

    if delta >= 4:
        confidence = "high"
    elif delta >= 2:
        confidence = "medium"
    else:
        confidence = "low"

    result = dict(row)
    result.update(
        {
            "bucket": bucket,
            "confidence": confidence,
            "keep_score": keep_score,
            "visibility_score": visibility_score,
            "unfollow_score": unfollow_score,
            "reasons": ";".join(reasons),
        }
    )
    return result


def sort_rows(rows: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
    bucket_order = {
        "keep_follow_and_engage": 0,
        "engage_without_follow": 1,
        "manual_review": 2,
    }
    return sorted(
        rows,
        key=lambda x: (
            bucket_order.get(x["bucket"], 9),
            -x["keep_score"],
            -x["visibility_score"],
            -x["unfollow_score"],
            x["username"].lower(),
        ),
    )


def unfollow_candidates(rows: List[Dict[str, Any]], max_unfollow: int) -> List[Dict[str, Any]]:
    candidates = [row for row in rows if row["bucket"] == "engage_without_follow"]
    candidates.sort(
        key=lambda x: (
            -max(x["visibility_score"], x["unfollow_score"]),
            x["keep_score"],
            x["username"].lower(),
        )
    )
    if max_unfollow < 0:
        return candidates
    return candidates[:max_unfollow]


def write_csv(path: pathlib.Path, rows: List[Dict[str, Any]]) -> None:
    fields = [
        "username",
        "display_name",
        "bucket",
        "confidence",
        "keep_score",
        "visibility_score",
        "unfollow_score",
        "follows_you",
        "verified",
        "protected",
        "reasons",
        "profile_url",
        "bio",
        "followers_count",
        "following_count",
        "collection_source",
    ]
    with path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fields)
        writer.writeheader()
        for row in rows:
            writer.writerow({key: row.get(key, "") for key in fields})


def write_json(path: pathlib.Path, payload: Dict[str, Any]) -> None:
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")


def write_candidates(path: pathlib.Path, rows: List[Dict[str, Any]]) -> None:
    lines = [row["username"] for row in rows]
    path.write_text("\n".join(lines) + ("\n" if lines else ""), encoding="utf-8")


def has_apify_source(rows: List[Dict[str, Any]]) -> bool:
    return any(row.get("collection_source") == "apify" for row in rows)


BUCKET_LABELS_JA = {
    "keep_follow_and_engage": "フォロー維持して絡む",
    "engage_without_follow": "フォロー解除候補（絡みは継続）",
    "manual_review": "要確認",
}

CONFIDENCE_LABELS_JA = {
    "high": "高",
    "medium": "中",
    "low": "低",
}

REASON_LABELS_JA = {
    "always_keep_handle": "常時維持ハンドル",
    "follows_you": "相互フォロー",
    "verified": "認証済み",
    "visibility_target_handle": "可視化対象ハンドル",
    "always_unfollow_handle": "常時解除ハンドル",
    "visibility_dominant": "可視化目的優勢",
    "signal_weak": "シグナル弱",
}

REASON_PREFIX_JA = {
    "keep_keywords": "維持キーワード一致",
    "visibility_keywords": "可視化キーワード一致",
    "unfollow_keywords": "解除キーワード一致",
}


def reason_token_to_ja(token: str) -> str:
    token = compact_text(token)
    if not token:
        return ""
    if ":" in token:
        prefix, value = token.split(":", 1)
        if prefix in REASON_PREFIX_JA:
            return f"{REASON_PREFIX_JA[prefix]}({value})"
    return REASON_LABELS_JA.get(token, token)


def reasons_to_ja(value: str) -> str:
    parts = [reason_token_to_ja(v) for v in value.split(";")]
    parts = [p for p in parts if p]
    if not parts:
        return "シグナル弱"
    return " / ".join(parts)


def write_markdown(
    path: pathlib.Path,
    input_json: pathlib.Path,
    config_path: Optional[pathlib.Path],
    rows: List[Dict[str, Any]],
    sorted_rows: List[Dict[str, Any]],
    candidates: List[Dict[str, Any]],
    detail_limit: int,
) -> None:
    total = len(rows)
    keep_count = sum(1 for row in rows if row["bucket"] == "keep_follow_and_engage")
    engage_count = sum(1 for row in rows if row["bucket"] == "engage_without_follow")
    review_count = sum(1 for row in rows if row["bucket"] == "manual_review")
    apify_sourced = has_apify_source(rows)

    lines: List[str] = []
    lines.append("---")
    lines.append(f"generated_at: {dt.datetime.now().isoformat()}")
    lines.append(f"input_json: {input_json}")
    if config_path:
        lines.append(f"config: {config_path}")
    lines.append(f"total_accounts: {total}")
    lines.append(f"keep_follow_and_engage: {keep_count}")
    lines.append(f"engage_without_follow: {engage_count}")
    lines.append(f"manual_review: {review_count}")
    lines.append("---")
    lines.append("")
    lines.append("# X フォロー分析")
    lines.append("")

    if apify_sourced:
        lines.append("> **注意（Apify収集）**: Apify経由のデータでは `follows_you` が取得できません。")
        lines.append("> 相互フォローの判定ができないため、解除候補に誤分類される可能性があります。")
        lines.append("> 可能ならCDP収集で再実行してください。")
        lines.append("")

    lines.append("## サマリー")
    lines.append("")
    lines.append(f"- 総アカウント数: {total}")
    lines.append(f"- フォロー維持して絡む: {keep_count}")
    lines.append(f"- フォロー解除候補（絡みは継続）: {engage_count}")
    lines.append(f"- 要確認: {review_count}")
    lines.append("")
    lines.append("## 解除候補ショートリスト")
    lines.append("")
    if candidates:
        lines.append("| 順位 | ユーザー名 | 表示名 | 相互フォロー | シグナル点 | 判定理由 |")
        lines.append("| --- | --- | --- | --- | --- | --- |")
        for idx, row in enumerate(candidates, start=1):
            score_signal = max(row["visibility_score"], row["unfollow_score"])
            lines.append(
                "| "
                + " | ".join(
                    [
                        str(idx),
                        f"@{md_cell(row['username'])}",
                        md_cell(row["display_name"]),
                        "はい" if row["follows_you"] else "いいえ",
                        str(score_signal),
                        md_cell(reasons_to_ja(row["reasons"])),
                    ]
                )
                + " |"
            )
    else:
        lines.append("- 該当なし")

    lines.append("")
    lines.append("## 判定一覧")
    lines.append("")
    lines.append("| ユーザー名 | 判定 | 確信度 | keep | visibility | unfollow | 相互フォロー | 判定理由 |")
    lines.append("| --- | --- | --- | --- | --- | --- | --- | --- |")

    detail_rows = sorted_rows if detail_limit <= 0 else sorted_rows[:detail_limit]
    for row in detail_rows:
        lines.append(
            "| "
            + " | ".join(
                [
                    f"@{md_cell(row['username'])}",
                    md_cell(BUCKET_LABELS_JA.get(row["bucket"], row["bucket"]), 40),
                    md_cell(
                        CONFIDENCE_LABELS_JA.get(row["confidence"], row["confidence"]), 10
                    ),
                    str(row["keep_score"]),
                    str(row["visibility_score"]),
                    str(row["unfollow_score"]),
                    "はい" if row["follows_you"] else "いいえ",
                    md_cell(reasons_to_ja(row["reasons"])),
                ]
            )
            + " |"
        )

    if detail_limit > 0 and len(sorted_rows) > detail_limit:
        lines.append("")
        lines.append(f"- Markdownには先頭{detail_limit}件のみ表示しています。詳細はCSV/JSONを参照してください。")

    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def default_output_dir(input_path: pathlib.Path) -> pathlib.Path:
    if input_path.parent.name == "raw":
        return input_path.parent.parent / "analysis"
    return input_path.parent / "analysis"


def main() -> None:
    args = parse_args()

    input_path = pathlib.Path(args.input_json).resolve()
    if not input_path.exists():
        raise SystemExit(f"Input file not found: {input_path}")

    output_dir = (
        pathlib.Path(args.output_dir).resolve()
        if args.output_dir
        else default_output_dir(input_path).resolve()
    )
    output_dir.mkdir(parents=True, exist_ok=True)

    config_path = pathlib.Path(args.config).resolve() if args.config else None
    config = load_config(config_path)

    rows = load_rows(input_path)
    if not rows:
        raise SystemExit("No valid accounts found in input JSON.")

    classified = [
        classify_row(
            row=row,
            config=config,
            min_keep_score=args.min_keep_score,
            min_visibility_score=args.min_visibility_score,
            min_unfollow_score=args.min_unfollow_score,
        )
        for row in rows
    ]
    sorted_rows = sort_rows(classified)
    candidates = unfollow_candidates(sorted_rows, args.max_unfollow)

    ts, username = parse_raw_name(input_path)
    prefix = f"{ts}_x_{username}_following_analysis"

    markdown_path = output_dir / f"{prefix}.md"
    csv_path = output_dir / f"{prefix}.csv"
    json_path = output_dir / f"{prefix}.json"
    candidates_path = output_dir / f"{prefix}_unfollow_candidates.txt"

    write_markdown(
        path=markdown_path,
        input_json=input_path,
        config_path=config_path,
        rows=classified,
        sorted_rows=sorted_rows,
        candidates=candidates,
        detail_limit=args.detail_limit,
    )

    write_csv(csv_path, sorted_rows)
    write_json(
        json_path,
        {
            "generated_at": dt.datetime.now().isoformat(),
            "input_json": str(input_path),
            "config": str(config_path) if config_path else "",
            "thresholds": {
                "min_keep_score": args.min_keep_score,
                "min_visibility_score": args.min_visibility_score,
                "min_unfollow_score": args.min_unfollow_score,
            },
            "total_accounts": len(classified),
            "counts": {
                "keep_follow_and_engage": sum(
                    1 for row in classified if row["bucket"] == "keep_follow_and_engage"
                ),
                "engage_without_follow": sum(
                    1 for row in classified if row["bucket"] == "engage_without_follow"
                ),
                "manual_review": sum(
                    1 for row in classified if row["bucket"] == "manual_review"
                ),
            },
            "rows": sorted_rows,
        },
    )
    write_candidates(candidates_path, candidates)

    print(f"analysis_md={markdown_path}")
    print(f"analysis_csv={csv_path}")
    print(f"analysis_json={json_path}")
    print(f"unfollow_candidates={candidates_path}")
    print(f"total_accounts={len(classified)}")
    print(f"unfollow_count={len(candidates)}")


if __name__ == "__main__":
    main()
