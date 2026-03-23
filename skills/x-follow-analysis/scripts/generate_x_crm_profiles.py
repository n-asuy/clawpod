#!/usr/bin/env python3
import argparse
import csv
import datetime as dt
import json
import pathlib
import re
from typing import Any, Dict, List, Sequence, Tuple


ANALYSIS_FILE_PATTERN = re.compile(
    r"^(?P<ts>\d{8}_\d{4})_x_(?P<owner>[A-Za-z0-9_]{1,15})_following_analysis(?:\.json)?$"
)


VALUE_TAG_RULES: Sequence[Tuple[str, Sequence[str]]] = (
    ("AI・LLM", ("ai", "llm", "gpt", "claude", "openai", "anthropic", "agent", "model", "prompt", "vibe")),
    ("開発・OSS", ("developer", "dev", "software", "engineer", "code", "coding", "build", "open source", "oss", "github")),
    ("デザイン・UX", ("design", "designer", "ux", "ui", "product design", "visual")),
    ("スタートアップ・事業", ("startup", "founder", "cofounder", "ceo", "business", "growth", "saas", "product")),
    ("投資・VC", ("investor", "venture", "vc", "angel", "fund", "capital")),
    ("クリプト・Web3", ("crypto", "blockchain", "web3", "defi", "ethereum", "bitcoin", "solana")),
    ("教育・発信", ("writer", "newsletter", "podcast", "educator", "teacher", "youtube", "media")),
)


def compact(value: Any) -> str:
    text = str(value or "").replace("\u00a0", " ")
    return re.sub(r"\s+", " ", text).strip()


def md_escape(value: Any) -> str:
    return compact(value).replace("|", "\\|")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate CRM profile directory from x-follow-analysis JSON output."
    )
    parser.add_argument(
        "--analysis-json",
        required=True,
        help="Path to *_following_analysis.json",
    )
    parser.add_argument(
        "--output-dir",
        default="",
        help="CRM output directory (default: sibling crm/ directory)",
    )
    parser.add_argument(
        "--owner",
        default="",
        help="Owner X handle (default: parsed from filename)",
    )
    parser.add_argument(
        "--top-n",
        type=int,
        default=80,
        help="Number of top-priority contacts shown in index markdown table",
    )
    return parser.parse_args()


def parse_analysis_name(path: pathlib.Path) -> Tuple[str, str]:
    m = ANALYSIS_FILE_PATTERN.match(path.name)
    if m:
        return m.group("ts"), m.group("owner")
    return dt.datetime.now().strftime("%Y%m%d_%H%M"), "unknown"


def load_analysis(path: pathlib.Path) -> Dict[str, Any]:
    obj = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(obj, dict):
        raise ValueError("analysis json must be object")
    rows = obj.get("rows")
    if not isinstance(rows, list) or not rows:
        raise ValueError("analysis json has no rows")
    obj["rows"] = [r for r in rows if isinstance(r, dict)]
    return obj


def infer_value_tags(text: str) -> List[str]:
    lower = text.lower()
    tags: List[str] = []
    for label, keys in VALUE_TAG_RULES:
        if any(k in lower for k in keys):
            tags.append(label)
    return tags or ["その他"]


def bucket_to_action(bucket: str) -> str:
    if bucket == "keep_follow_and_engage":
        return "重点関係維持"
    if bucket == "engage_without_follow":
        return "フォロー解除候補（接点維持）"
    return "要観察"


def bucket_to_engagement(bucket: str) -> str:
    if bucket == "keep_follow_and_engage":
        return "返信・引用・DMの優先対象。週1回以上の能動接点を作る。"
    if bucket == "engage_without_follow":
        return "フォロー依存ではなく、検索・リスト経由で投稿反応。フォロー解除を検討。"
    return "投稿テーマの推移を観察し、関係維持か解除かを再判定。"


def priority_score(row: Dict[str, Any]) -> int:
    keep = int(row.get("keep_score", 0) or 0)
    vis = int(row.get("visibility_score", 0) or 0)
    unf = int(row.get("unfollow_score", 0) or 0)
    follows_you = 1 if row.get("follows_you") else 0
    verified = 1 if row.get("verified") else 0

    score = 45 + (keep * 6) + (vis * 2) - (unf * 4) + (follows_you * 25) + (verified * 5)
    score = max(0, min(100, score))
    return score


def normalize_row(row: Dict[str, Any]) -> Dict[str, Any]:
    username = compact(row.get("username")).lstrip("@")
    display_name = compact(row.get("display_name")) or username
    bio = compact(row.get("bio"))
    card_text = compact(row.get("card_text"))
    text_blob = f"{display_name} {bio} {card_text}"
    tags = infer_value_tags(text_blob)
    bucket = compact(row.get("bucket"))
    action = bucket_to_action(bucket)
    return {
        "username": username,
        "display_name": display_name,
        "profile_url": compact(row.get("profile_url")) or f"https://x.com/{username}",
        "bio": bio,
        "card_text": card_text,
        "bucket": bucket,
        "action": action,
        "engagement_policy": bucket_to_engagement(bucket),
        "reasons": compact(row.get("reasons")),
        "keep_score": int(row.get("keep_score", 0) or 0),
        "visibility_score": int(row.get("visibility_score", 0) or 0),
        "unfollow_score": int(row.get("unfollow_score", 0) or 0),
        "follows_you": bool(row.get("follows_you", False)),
        "verified": bool(row.get("verified", False)),
        "protected": bool(row.get("protected", False)),
        "value_tags": tags,
        "priority_score": priority_score(row),
    }


def write_contact_markdown(
    path: pathlib.Path,
    owner: str,
    collected_at: str,
    contact: Dict[str, Any],
) -> None:
    tags = " / ".join(contact["value_tags"])
    follows_you = "はい" if contact["follows_you"] else "いいえ"
    verified = "はい" if contact["verified"] else "いいえ"
    protected = "はい" if contact["protected"] else "いいえ"

    lines: List[str] = []
    lines.append("---")
    lines.append(f"owner: {owner}")
    lines.append(f"collected_at: {collected_at}")
    lines.append(f"username: {contact['username']}")
    lines.append(f"display_name: {contact['display_name']}")
    lines.append(f"profile_url: {contact['profile_url']}")
    lines.append(f"action: {contact['action']}")
    lines.append(f"priority_score: {contact['priority_score']}")
    lines.append(f"value_tags: [{', '.join(contact['value_tags'])}]")
    lines.append("---")
    lines.append("")
    lines.append(f"# @{contact['username']} CRMプロファイル")
    lines.append("")
    lines.append("## 基本情報")
    lines.append("")
    lines.append(f"- 表示名: {contact['display_name']}")
    lines.append(f"- プロフィール: {contact['profile_url']}")
    lines.append(f"- 相互フォロー: {follows_you}")
    lines.append(f"- 認証済み: {verified}")
    lines.append(f"- 鍵アカウント: {protected}")
    lines.append("")
    lines.append("## 価値観・関心テーマ（推定）")
    lines.append("")
    lines.append(f"- タグ: {tags}")
    lines.append(f"- 自己紹介: {contact['bio'] or '（自己紹介未取得）'}")
    lines.append("")
    lines.append("## 投稿・発信の観測")
    lines.append("")
    lines.append("- 取得ソース: `following` 一覧のユーザーカード")
    lines.append("- 投稿本文: 今回は未収集（必要なら次回バッチで追加取得）")
    lines.append("")
    lines.append("## CRM方針")
    lines.append("")
    lines.append(f"- 推奨アクション: {contact['action']}")
    lines.append(f"- 優先度スコア: {contact['priority_score']}/100")
    lines.append(f"- 実行方針: {contact['engagement_policy']}")
    lines.append(f"- 判定根拠: {contact['reasons'] or 'signal_weak'}")
    lines.append("")
    lines.append("## オペレーションメモ")
    lines.append("")
    lines.append("- 最終接点日: 未記録")
    lines.append("- 次回アクション: ")
    lines.append("- メモ: ")
    lines.append("")

    path.write_text("\n".join(lines), encoding="utf-8")


def write_index_markdown(
    path: pathlib.Path,
    owner: str,
    collected_at: str,
    contacts: List[Dict[str, Any]],
    top_n: int,
) -> None:
    total = len(contacts)
    action_counts = {
        "重点関係維持": sum(1 for c in contacts if c["action"] == "重点関係維持"),
        "フォロー解除候補（接点維持）": sum(
            1 for c in contacts if c["action"] == "フォロー解除候補（接点維持）"
        ),
        "要観察": sum(1 for c in contacts if c["action"] == "要観察"),
    }
    top_rows = contacts[: max(0, top_n)]

    lines: List[str] = []
    lines.append("---")
    lines.append(f"owner: {owner}")
    lines.append(f"collected_at: {collected_at}")
    lines.append(f"total_contacts: {total}")
    lines.append("---")
    lines.append("")
    lines.append(f"# @{owner} フォローCRM一覧")
    lines.append("")
    lines.append("## サマリー")
    lines.append("")
    lines.append(f"- 総件数: {total}")
    lines.append(f"- 重点関係維持: {action_counts['重点関係維持']}")
    lines.append(
        f"- フォロー解除候補（接点維持）: {action_counts['フォロー解除候補（接点維持）']}"
    )
    lines.append(f"- 要観察: {action_counts['要観察']}")
    lines.append("")
    lines.append("## 優先度順リスト")
    lines.append("")
    lines.append("| # | ユーザー | 表示名 | 優先度 | 推奨アクション | 価値観タグ | 相互 |")
    lines.append("| --- | --- | --- | --- | --- | --- | --- |")
    for idx, c in enumerate(top_rows, start=1):
        lines.append(
            "| "
            + " | ".join(
                [
                    str(idx),
                    f"[@{md_escape(c['username'])}](accounts/{c['username']}.md)",
                    md_escape(c["display_name"]),
                    str(c["priority_score"]),
                    md_escape(c["action"]),
                    md_escape(" / ".join(c["value_tags"])),
                    "はい" if c["follows_you"] else "いいえ",
                ]
            )
            + " |"
        )
    if top_n > 0 and total > top_n:
        lines.append("")
        lines.append(f"- 表示は上位{top_n}件。全件は `crm_contacts.csv` を参照。")

    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def write_csv(path: pathlib.Path, contacts: List[Dict[str, Any]]) -> None:
    fieldnames = [
        "username",
        "display_name",
        "profile_url",
        "priority_score",
        "action",
        "bucket",
        "follows_you",
        "verified",
        "protected",
        "keep_score",
        "visibility_score",
        "unfollow_score",
        "value_tags",
        "bio",
        "reasons",
    ]
    with path.open("w", encoding="utf-8", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=fieldnames)
        writer.writeheader()
        for c in contacts:
            writer.writerow(
                {
                    **{k: c.get(k, "") for k in fieldnames},
                    "value_tags": "|".join(c.get("value_tags", [])),
                }
            )


def write_manifest(path: pathlib.Path, payload: Dict[str, Any]) -> None:
    path.write_text(json.dumps(payload, ensure_ascii=False, indent=2), encoding="utf-8")


def default_output_dir(analysis_json: pathlib.Path) -> pathlib.Path:
    if analysis_json.parent.name == "analysis":
        return analysis_json.parent.parent / "crm"
    return analysis_json.parent / "crm"


def main() -> None:
    args = parse_args()
    analysis_path = pathlib.Path(args.analysis_json).resolve()
    if not analysis_path.exists():
        raise SystemExit(f"analysis json not found: {analysis_path}")

    data = load_analysis(analysis_path)
    ts, owner_from_name = parse_analysis_name(analysis_path)
    owner = compact(args.owner).lstrip("@") or owner_from_name
    collected_at = compact(data.get("generated_at")) or dt.datetime.now().isoformat()

    output_dir = (
        pathlib.Path(args.output_dir).resolve()
        if args.output_dir
        else default_output_dir(analysis_path).resolve()
    )
    accounts_dir = output_dir / "accounts"
    output_dir.mkdir(parents=True, exist_ok=True)
    accounts_dir.mkdir(parents=True, exist_ok=True)

    contacts = [normalize_row(r) for r in data["rows"]]
    contacts.sort(
        key=lambda x: (
            -x["priority_score"],
            -x["keep_score"],
            x["username"].lower(),
        )
    )

    for c in contacts:
        write_contact_markdown(
            path=accounts_dir / f"{c['username']}.md",
            owner=owner,
            collected_at=collected_at,
            contact=c,
        )

    index_md = output_dir / "index.md"
    contacts_csv = output_dir / "crm_contacts.csv"
    contacts_json = output_dir / "crm_contacts.json"
    manifest_json = output_dir / "manifest.json"

    write_index_markdown(index_md, owner, collected_at, contacts, args.top_n)
    write_csv(contacts_csv, contacts)
    contacts_json.write_text(
        json.dumps(
            {
                "owner": owner,
                "collected_at": collected_at,
                "source_analysis_json": str(analysis_path),
                "total_contacts": len(contacts),
                "contacts": contacts,
            },
            ensure_ascii=False,
            indent=2,
        ),
        encoding="utf-8",
    )
    write_manifest(
        manifest_json,
        {
            "generated_at": dt.datetime.now().isoformat(),
            "owner": owner,
            "source_analysis_json": str(analysis_path),
            "output_dir": str(output_dir),
            "total_contacts": len(contacts),
            "accounts_dir": str(accounts_dir),
            "index_md": str(index_md),
            "contacts_csv": str(contacts_csv),
            "contacts_json": str(contacts_json),
            "timestamp_tag": ts,
        },
    )

    print(f"crm_dir={output_dir}")
    print(f"index_md={index_md}")
    print(f"contacts_csv={contacts_csv}")
    print(f"contacts_json={contacts_json}")
    print(f"total_contacts={len(contacts)}")


if __name__ == "__main__":
    main()
