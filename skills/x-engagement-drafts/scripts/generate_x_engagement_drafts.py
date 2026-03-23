#!/usr/bin/env python3
import argparse
import datetime as dt
import json
import pathlib
import re
from typing import Dict, List, Optional, Tuple


ROW_RE = re.compile(r"^\|\s*@?([^|]+?)\s*\|\s*(\d+)\s*\|\s*`([^`]+)`\s*\|$")
URL_RE = re.compile(r"^\*\*URL\*\*:\s*(https://x\.com/[^\s]+)\s*$")
HANDLE_RE = re.compile(r"^[A-Za-z0-9_]{1,15}$")
PLACEHOLDER_EXCERPTS = {
    "(本文なし)",
    "(本文抽出なし)",
    "（本文なし）",
    "（本文抽出なし）",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Generate quote-RT and reply draft ideas from a daily post summary markdown."
        )
    )
    parser.add_argument(
        "--summary-file",
        required=True,
        help="Path to crm/daily_post_reports/YYYYMMDD_summary.md",
    )
    parser.add_argument(
        "--output",
        default="",
        help="Output markdown path (default: <owner-dir>/YYYYMMDD_reply_quote_drafts.md)",
    )
    parser.add_argument(
        "--analysis-json",
        default="",
        help=(
            "Optional follow analysis JSON for priority weighting "
            "(default: auto-detect latest for owner)"
        ),
    )
    parser.add_argument(
        "--max-items",
        type=int,
        default=10,
        help="Maximum number of handles to include (default: 10)",
    )
    parser.add_argument(
        "--include-handle",
        action="append",
        default=[],
        help="Include only these handles (repeatable, @optional).",
    )
    parser.add_argument(
        "--exclude-handle",
        action="append",
        default=[],
        help="Exclude these handles (repeatable, @optional).",
    )
    return parser.parse_args()


def normalize_handle(value: str) -> str:
    handle = value.strip()
    if handle.startswith("@"):
        handle = handle[1:]
    if not HANDLE_RE.fullmatch(handle):
        return ""
    return handle


def compact(text: str, max_len: int = 220) -> str:
    one = re.sub(r"\s+", " ", text.replace("\u00a0", " ")).strip()
    if len(one) <= max_len:
        return one
    return one[: max_len - 1].rstrip() + "…"


def contains_any(text: str, words: List[str]) -> bool:
    return any(w in text for w in words)


def contains_token(text: str, token: str) -> bool:
    return re.search(rf"\b{re.escape(token)}\b", text) is not None


def is_placeholder_excerpt(text: str) -> bool:
    cleaned = compact(text, max_len=400)
    if not cleaned:
        return True
    return cleaned in PLACEHOLDER_EXCERPTS


def parse_frontmatter(text: str) -> Dict[str, str]:
    lines = text.splitlines()
    if not lines or lines[0].strip() != "---":
        return {}
    out: Dict[str, str] = {}
    for line in lines[1:]:
        if line.strip() == "---":
            break
        m = re.match(r"^\s*([A-Za-z0-9_]+)\s*:\s*(.*?)\s*$", line)
        if m:
            out[m.group(1)] = m.group(2)
    return out


def read_summary_rows(summary_path: pathlib.Path) -> Tuple[Dict[str, str], List[Dict[str, str]]]:
    text = summary_path.read_text(encoding="utf-8")
    frontmatter = parse_frontmatter(text)

    rows: List[Dict[str, str]] = []
    for line in text.splitlines():
        m = ROW_RE.match(line)
        if not m:
            continue
        handle = normalize_handle(m.group(1))
        count = int(m.group(2))
        rel_file = m.group(3).strip()
        if not handle or count <= 0:
            continue
        rows.append({"handle": handle, "count": str(count), "rel_file": rel_file})
    return frontmatter, rows


def extract_posts(post_file: pathlib.Path) -> List[Dict[str, str]]:
    text = post_file.read_text(encoding="utf-8")
    lines = text.splitlines()
    posts: List[Dict[str, str]] = []
    for idx, line in enumerate(lines):
        m = URL_RE.match(line.strip())
        if not m:
            continue
        url = m.group(1)
        quote_lines: List[str] = []
        j = idx + 1
        while j < len(lines):
            raw = lines[j]
            stripped = raw.strip()
            if stripped.startswith("**和訳**"):
                break
            if stripped.startswith("### "):
                break
            if stripped.startswith("**URL**:"):
                break
            if raw.startswith(">"):
                quote_lines.append(raw.lstrip("> ").rstrip())
            j += 1
        excerpt = compact(" ".join(quote_lines), max_len=260)
        posts.append({"url": url, "excerpt": excerpt})
    return posts


def detect_topic(text: str) -> str:
    probe = text.lower()
    if contains_any(
        probe,
        [
            "security",
            "vulnerability",
            "auth",
            "cve",
            "exploit",
            "breach",
            "脆弱性",
            "セキュリティ",
        ],
    ):
        return "security"
    if contains_any(
        probe,
        ["hiring", "we're hiring", "full stack engineer", "infrastructure engineer", "募集"],
    ):
        return "hiring"
    if contains_any(
        probe,
        [
            "release",
            "released",
            "launch",
            "launched",
            "shipped",
            "now live",
            "coming soon",
            "rollout",
            "generally available",
            "public beta",
            "update",
            "公開",
            "アップデート",
        ],
    ):
        return "release"
    if contains_any(
        probe,
        [
            "model",
            "benchmark",
            "context window",
            "token",
            "claude",
            "gpt",
            "gemini",
            "opus",
            "llm",
        ],
    ):
        return "modeling"
    if contains_any(
        probe,
        ["agent", "workflow", "automation", "observability", "pipeline", "mcp"],
    ):
        return "agent"
    if contains_token(probe, "ops"):
        return "agent"
    if contains_any(
        probe,
        ["pricing", "revenue", "cost", "enterprise", "funding", "adoption", "business"],
    ):
        return "business"
    if contains_any(probe, ["design", "figma", "visual", "skin details"]):
        return "design"
    if contains_token(probe, "ui") or contains_token(probe, "ux"):
        return "design"
    if contains_any(probe, ["thought", "analysis", "predict", "opinion", "考え", "分析"]):
        return "analysis"
    if contains_any(probe, ["built", "open source", "demo", "project", "作った", "構築"]):
        return "build"
    return "general"


def draft_texts(topic: str) -> Dict[str, str]:
    if topic == "security":
        return {
            "reply_ja": "この観点重要です。実運用では検出だけでなく、修正提案の再現性とレビュー導線まで含めて評価したいです。",
            "reply_en": "Great point. In production, I’d evaluate not only detection but also fix reliability and review workflow integration.",
            "quote_ja": "セキュリティ系は「見つける」から「安全に直す」への競争に移っている印象。CIとレビュー接続まで見たい。",
            "quote_en": "Security tooling is shifting from “finding issues” to “fixing safely.” CI/review integration is the key differentiator now.",
        }
    if topic == "hiring":
        return {
            "reply_ja": "採用文面が明確で良いですね。期待役割と最初の90日ミッションまであると、さらに候補者の解像度が上がりそうです。",
            "reply_en": "Clear hiring message. Adding role expectations and a first-90-days mission would likely attract even better-fit candidates.",
            "quote_ja": "この採用投稿、スコープとオーナーシップが伝わっていて強い。初期フェーズほど『何を任せるか』の明瞭さが効く。",
            "quote_en": "Strong hiring post with clear scope and ownership. In early teams, clarity on what the role truly owns makes a major difference.",
        }
    if topic == "release":
        return {
            "reply_ja": "いいリリースですね。導入の最小ステップ（テンプレや初期設定例）があると試す人が一気に増えそうです。",
            "reply_en": "Great release. A minimal onboarding template would probably accelerate adoption a lot.",
            "quote_ja": "この更新は機能追加そのものより、現場ワークフローを変えるタイプ。導入コストの低さが効く。",
            "quote_en": "This update matters less as a feature list and more as a workflow shift. Low integration friction is the real win.",
        }
    if topic == "modeling":
        return {
            "reply_ja": "比較軸が明確で助かります。速度・品質・長文耐性を分けて示すと、実務での使い分け判断がさらにしやすくなりますね。",
            "reply_en": "Helpful framing. Splitting speed, quality, and long-context reliability makes model selection much easier in real work.",
            "quote_ja": "モデル比較は『最強探し』より、用途ごとの最適配分まで落とし込むと意思決定に効く知見になる。",
            "quote_en": "Model comparisons become decision-grade when they map to use-case allocation, not only leaderboard ranking.",
        }
    if topic == "agent":
        return {
            "reply_ja": "運用視点まで触れているのが良いですね。観測性とフォールバック設計が揃うと、実戦投入の再現性が一気に上がります。",
            "reply_en": "Great that you include operations. Observability plus fallbacks is what makes agent workflows reliably production-ready.",
            "quote_ja": "エージェント活用はデモ精度だけでなく、監視と復旧まで含めた運用耐性で差がつくフェーズに入っている。",
            "quote_en": "Agent workflows are now differentiated by operational resilience, not demo quality alone.",
        }
    if topic == "business":
        return {
            "reply_ja": "この視点は実務的で重要です。価格や見栄えだけでなく、運用時の継続コストまで含めて見ると判断がぶれにくいですね。",
            "reply_en": "This is a practical lens. Decisions become far more robust when ongoing operating cost is considered, not just headline pricing.",
            "quote_ja": "プロダクト評価は機能だけでなく、単価・運用効率・定着率の3点で見ると意思決定しやすい。",
            "quote_en": "Product evaluation gets clearer when framed across capability, unit economics, and operational adoption.",
        }
    if topic == "design":
        return {
            "reply_ja": "この方向性いいですね。見た目の改善だけでなく、制作判断の往復回数を減らせる点が実務インパクトとして大きいです。",
            "reply_en": "Great direction. The real impact is reducing decision loops in production, not just improving visuals.",
            "quote_ja": "デザイン系の進化は表現力だけでなく、チームの意思決定速度を上げられるかで価値が決まる。",
            "quote_en": "The value of design tooling is defined by decision speed gains, not aesthetics alone.",
        }
    if topic == "analysis":
        return {
            "reply_ja": "視点いいですね。結論だけでなく前提条件も揃えると、実務で再利用しやすい議論になりそうです。",
            "reply_en": "Nice angle. Aligning assumptions alongside conclusions would make this much more reusable in practice.",
            "quote_ja": "この論点、速度比較だけでなく前提と評価軸を揃えると、意思決定に使える知見になる。",
            "quote_en": "This becomes decision-grade insight when assumptions and evaluation criteria are explicit, not only speed comparisons.",
        }
    if topic == "build":
        return {
            "reply_ja": "いいビルドですね。こういう可視化は運用理解が一気に進むので助かります。",
            "reply_en": "Great build. This kind of visualization really accelerates operational understanding.",
            "quote_ja": "ビルド共有は「何を作ったか」に加えて「運用でどう効いたか」まであると価値がさらに上がる。",
            "quote_en": "Build showcases become even more valuable when they include operational impact, not just implementation details.",
        }
    return {
        "reply_ja": "いい投稿ですね。実務で使うなら、この視点をどう運用に落とすかまで考えるとさらに面白くなりそうです。",
        "reply_en": "Great post. It gets even more useful when mapped directly to operational workflow decisions.",
        "quote_ja": "この話、現場文脈に置き換えると示唆が増える。運用設計とセットで見ると効くテーマ。",
        "quote_en": "This becomes more actionable in a concrete ops context. It’s a strong theme when paired with workflow design.",
    }


def reason_text(topic: str, post_count: int, bucket: str) -> str:
    topic_reason = {
        "security": "安全性の実務論点に自然につながりやすく、会話の価値が高い。",
        "hiring": "採用投稿は役割定義への補足が刺さりやすく、返信が続きやすい。",
        "release": "リリース投稿は具体提案を添えると再反応が発生しやすい。",
        "modeling": "比較系投稿は評価軸を補足すると保存・再共有されやすい。",
        "agent": "エージェント運用は監視/復旧の視点を足すと専門層の反応が増える。",
        "business": "ビジネス視点を補足すると実務層との対話が生まれやすい。",
        "design": "デザイン文脈は実務効果を明示すると共感が得られやすい。",
        "analysis": "意見投稿は前提条件を揃える補足が会話に発展しやすい。",
        "build": "デモ系は運用インパクトを足すと差別化しやすい。",
        "general": "過度な断定を避けつつ対話を開始しやすい文面。",
    }
    parts = [topic_reason.get(topic, topic_reason["general"])]
    if post_count >= 3:
        parts.append(f"当日投稿が{post_count}件あり、継続対話に発展させやすい。")
    if bucket == "keep_follow_and_engage":
        parts.append("関係維持の優先度が高い相手として先に絡む価値がある。")
    elif bucket == "manual_review":
        parts.append("温度感を測る初回接点としても使いやすい。")
    return " ".join(parts)


def bucket_bonus(bucket: str) -> int:
    if bucket == "keep_follow_and_engage":
        return 3
    if bucket == "manual_review":
        return 1
    return 0


def load_bucket_map(path: Optional[pathlib.Path]) -> Dict[str, str]:
    if not path or not path.exists():
        return {}
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except Exception:
        return {}
    rows = data.get("rows", [])
    if not isinstance(rows, list):
        return {}
    out: Dict[str, str] = {}
    for row in rows:
        if not isinstance(row, dict):
            continue
        handle = normalize_handle(str(row.get("username", "")))
        bucket = str(row.get("bucket", "")).strip()
        if handle and bucket:
            out[handle.lower()] = bucket
    return out


def infer_analysis_json(summary_path: pathlib.Path, owner: str) -> Optional[pathlib.Path]:
    crm_dir = summary_path.parent.parent
    owner_dir = crm_dir.parent
    analysis_dir = owner_dir / "follow_analysis" / "analysis"
    if not analysis_dir.exists():
        return None
    cands = sorted(analysis_dir.glob(f"*_x_{owner}_following_analysis.json"))
    if not cands:
        return None
    return cands[-1]


def score_item(
    post_count: int, excerpt: str, bucket: str, handle: str, include_handles: set
) -> Tuple[int, str]:
    topic = detect_topic(excerpt)
    score = min(post_count, 3) + bucket_bonus(bucket)
    if topic in ("security", "release", "build", "hiring", "modeling", "agent"):
        score += 2
    if topic in ("analysis", "business", "design"):
        score += 1
    if is_placeholder_excerpt(excerpt):
        score -= 5
    if include_handles and handle.lower() in include_handles:
        score += 100
    return score, topic


def main() -> None:
    args = parse_args()
    summary_path = pathlib.Path(args.summary_file).resolve()
    if not summary_path.exists():
        raise SystemExit(f"summary file not found: {summary_path}")

    frontmatter, rows = read_summary_rows(summary_path)
    if not rows:
        raise SystemExit("no handles with non-zero posts found in summary")

    owner = frontmatter.get("owner", "").strip()
    if not owner:
        owner = "unknown_owner"
    target_date = frontmatter.get("target_date", "").strip()
    if not target_date:
        target_date = dt.date.today().isoformat()
    target_tag = target_date.replace("-", "")

    include_handles = {normalize_handle(h).lower() for h in args.include_handle if normalize_handle(h)}
    exclude_handles = {normalize_handle(h).lower() for h in args.exclude_handle if normalize_handle(h)}

    analysis_path: Optional[pathlib.Path]
    if args.analysis_json.strip():
        analysis_path = pathlib.Path(args.analysis_json).resolve()
    else:
        analysis_path = infer_analysis_json(summary_path, owner)
    bucket_map = load_bucket_map(analysis_path)

    crm_dir = summary_path.parent.parent
    items: List[Dict[str, str]] = []
    for row in rows:
        handle = row["handle"]
        if include_handles and handle.lower() not in include_handles:
            continue
        if handle.lower() in exclude_handles:
            continue

        post_count = int(row["count"])
        post_file = (crm_dir / row["rel_file"]).resolve()
        if not post_file.exists():
            continue
        posts = extract_posts(post_file)
        if not posts:
            continue
        usable = [p for p in posts if not is_placeholder_excerpt(p["excerpt"])]
        first = max(usable, key=lambda p: len(p["excerpt"])) if usable else posts[0]
        excerpt = first["excerpt"] or "(本文抽出なし)"
        if is_placeholder_excerpt(excerpt):
            continue
        bucket = bucket_map.get(handle.lower(), "manual_review")
        score, topic = score_item(post_count, excerpt, bucket, handle, include_handles)
        drafts = draft_texts(topic)
        items.append(
            {
                "handle": handle,
                "post_count": str(post_count),
                "url": first["url"],
                "excerpt": excerpt,
                "score": str(score),
                "topic": topic,
                "bucket": bucket,
                "reason": reason_text(topic, post_count, bucket),
                **drafts,
            }
        )

    if not items:
        raise SystemExit("no draftable items found after filters")

    items.sort(key=lambda x: (int(x["score"]), int(x["post_count"])), reverse=True)
    items = items[: max(1, args.max_items)]

    high = items[:3]
    mid = items[3:7]
    low = items[7:]

    mmdd = dt.date.fromisoformat(target_date).strftime("%-m/%-d")

    owner_dir = crm_dir.parent
    default_out = owner_dir / f"{target_tag}_reply_quote_drafts.md"
    output_path = pathlib.Path(args.output).resolve() if args.output.strip() else default_out
    output_path.parent.mkdir(parents=True, exist_ok=True)

    lines: List[str] = []
    lines.extend(
        [
            f"# @{owner} 引リツ・リプライ案（{target_date}）",
            "",
            f"{target_date} の当日投稿から、会話価値が高い候補を優先度付きで作成。",
            "",
            "---",
            "",
        ]
    )

    index = 1

    def append_section(title: str, section_items: List[Dict[str, str]]) -> None:
        nonlocal index
        if not section_items:
            return
        lines.append(title)
        lines.append("")
        for item in section_items:
            lines.append(
                f"### {index}. @{item['handle']} [{mmdd}収集 / 当日投稿{item['post_count']}件]"
            )
            lines.append("")
            lines.append(f"**狙う投稿**: {compact(item['excerpt'], max_len=80)}")
            lines.append(f"- URL: {item['url']}")
            lines.append("")
            lines.append(f"**内容**: {compact(item['excerpt'], max_len=180)}")
            lines.append("")
            lines.append("**リプライ案（日本語）**:")
            lines.append(f"> {item['reply_ja']}")
            lines.append("")
            lines.append("**Reply Draft (English)**:")
            lines.append(f"> {item['reply_en']}")
            lines.append("")
            lines.append("**引用RT案（日本語）**:")
            lines.append(f"> {item['quote_ja']}")
            lines.append("")
            lines.append("**Quote RT Draft (English)**:")
            lines.append(f"> {item['quote_en']}")
            lines.append("")
            lines.append(f"**理由**: {item['reason']}")
            lines.append("")
            lines.append("---")
            lines.append("")
            index += 1

    append_section("## 優先度 高（今日中にやる / 3件）", high)
    append_section("## 優先度 中（今週中 / 4件）", mid)
    append_section("## 優先度 低（余裕があれば）", low)

    top_handles = [f"@{x['handle']}" for x in items[:3]]
    lines.extend(
        [
            "## 今日のアクション",
            "",
            *(f"{i + 1}. **{h}** に返信 or 引用RT" for i, h in enumerate(top_handles)),
            "",
        ]
    )

    output_path.write_text("\n".join(lines), encoding="utf-8")

    print(f"output_file={output_path}")
    print(f"items_total={len(items)}")
    print(f"high_count={len(high)}")
    print(f"medium_count={len(mid)}")
    print(f"low_count={len(low)}")
    if analysis_path:
        print(f"analysis_json={analysis_path}")


if __name__ == "__main__":
    main()
