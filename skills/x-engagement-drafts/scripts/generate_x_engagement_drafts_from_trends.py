#!/usr/bin/env python3
import argparse
import datetime as dt
import json
import pathlib
import re
from collections import defaultdict
from dataclasses import dataclass
from typing import Dict, List, Optional, Set


HEADER_RE = re.compile(r"^###\s+\d+\.\s+@([A-Za-z0-9_]{1,15})")
LIKES_RE = re.compile(r"^\*\*いいね\*\*:\s*([0-9,]+)")
URL_RE = re.compile(r"^\*\*URL\*\*:\s*(https://x\.com/[^\s]+)")
HANDLE_RE = re.compile(r"^[A-Za-z0-9_]{1,15}$")


@dataclass
class Post:
    handle: str
    url: str
    excerpt: str
    likes: int
    file: str


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="Generate engagement drafts from trend markdown reports")
    p.add_argument("--trend-dir", required=True, help="Directory containing trend markdown files")
    p.add_argument("--pattern", required=True, help="Glob pattern for trend files")
    p.add_argument("--owner-dir", required=True, help="Owner root dir e.g. 24_SNS_X/n_asuy")
    p.add_argument("--owner", default="n_asuy")
    p.add_argument("--target-date", default="")
    p.add_argument("--analysis-json", default="")
    p.add_argument("--output", default="")
    p.add_argument("--max-items", type=int, default=100)
    return p.parse_args()


def compact(text: str, max_len: int = 220) -> str:
    one = re.sub(r"\s+", " ", text.replace("\u00a0", " ")).strip()
    if len(one) <= max_len:
        return one
    return one[: max_len - 1].rstrip() + "…"


def normalize_handle(value: str) -> str:
    handle = value.strip()
    if handle.startswith("@"):
        handle = handle[1:]
    if not HANDLE_RE.fullmatch(handle):
        return ""
    return handle


def parse_posts(path: pathlib.Path) -> List[Post]:
    lines = path.read_text(encoding="utf-8").splitlines()
    posts: List[Post] = []

    i = 0
    while i < len(lines):
        line = lines[i]
        m = HEADER_RE.match(line.strip())
        if not m:
            i += 1
            continue

        handle = normalize_handle(m.group(1))
        likes = 0
        url = ""
        excerpt_lines: List[str] = []

        j = i + 1
        while j < len(lines):
            cur = lines[j].rstrip()
            if HEADER_RE.match(cur.strip()):
                break

            lm = LIKES_RE.match(cur.strip())
            if lm:
                likes = int(lm.group(1).replace(",", ""))

            um = URL_RE.match(cur.strip())
            if um:
                url = um.group(1)

            if cur.strip().startswith("**和訳**"):
                # Excerpt from original text only
                pass
            elif cur.startswith(">"):
                excerpt_lines.append(cur.lstrip("> ").strip())

            j += 1

        excerpt = compact(" ".join(excerpt_lines), max_len=280)
        if handle and url and excerpt:
            posts.append(Post(handle=handle, url=url, excerpt=excerpt, likes=likes, file=path.name))

        i = j

    return posts


def load_crm_handles(owner_dir: pathlib.Path) -> Set[str]:
    accounts_dir = owner_dir / "crm" / "accounts"
    if not accounts_dir.exists():
        return set()
    return {p.stem.lower() for p in accounts_dir.glob("*.md") if p.is_file()}


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
        h = normalize_handle(str(row.get("username", "")))
        b = str(row.get("bucket", "")).strip()
        if h and b:
            out[h.lower()] = b
    return out


def detect_topic(text: str) -> str:
    t = text.lower()
    if any(k in t for k in ["security", "auth", "vulnerability", "cve", "脆弱性", "セキュリティ"]):
        return "security"
    if any(k in t for k in ["launch", "released", "introducing", "now live", "public beta", "発表", "公開"]):
        return "release"
    if any(k in t for k in ["cursor", "claude", "codex", "model", "benchmark", "llm"]):
        return "modeling"
    if any(k in t for k in ["vc", "revenue", "cost", "subsid", "business", "pricing", "funding"]):
        return "business"
    if any(k in t for k in ["design", "ui", "ux", "figma", "component"]):
        return "design"
    if any(k in t for k in ["agent", "workflow", "automation", "pipeline", "mcp"]):
        return "agent"
    if any(k in t for k in ["opinion", "journey", "future", "replace", "farmer", "psychopath"]):
        return "opinion"
    return "general"


def bucket_bonus(bucket: str) -> int:
    if bucket == "keep_follow_and_engage":
        return 15
    if bucket == "manual_review":
        return 5
    if bucket == "unfollow_candidate":
        return -10
    return 0


def drafts_for(topic: str) -> Dict[str, str]:
    if topic == "security":
        return {
            "reply_ja": "この整理すごく参考になります。実運用では、検出精度に加えて修正提案の再現性とレビュー統合まで見られるとさらに強いですね。",
            "reply_en": "Super useful breakdown. In production, it gets even stronger when fix reliability and review integration are covered alongside detection quality.",
            "quote_ja": "こういう安全性前提の議論は本当に価値が高い。セキュリティは『見つける』だけでなく『安全に直す』まで設計してこそ強い。",
            "quote_en": "This security-first framing is genuinely valuable. The real edge is not only finding issues but fixing them safely with workflow integration.",
            "reason": "感謝と称賛を先に置くと、専門的な追加コメントにも前向きに返答されやすい。",
        }
    if topic == "release":
        return {
            "reply_ja": "良いリリースですね、かなり刺さります。導入の最小テンプレートまであると、試す人が一気に増えそうです。",
            "reply_en": "Excellent release, this one really lands. A minimal onboarding template would likely boost adoption even faster.",
            "quote_ja": "このアップデートは機能追加以上に、現場の手数を減らしてくれるタイプ。こういう改善が積み上がると強い。",
            "quote_en": "This update matters beyond feature count; it reduces real workflow friction. Improvements like this compound over time.",
            "reason": "リリース投稿はまずポジティブに評価し、次に小さな提案を添えると会話が伸びやすい。",
        }
    if topic == "modeling":
        return {
            "reply_ja": "比較をここまで言語化してくれるの助かります。速度/品質/長文耐性で分けると、実務でもすぐ使える知見になりますね。",
            "reply_en": "Really appreciate how clearly you framed this comparison. Breaking it down into speed/quality/long-context reliability makes it immediately useful in practice.",
            "quote_ja": "こういう比較投稿は価値が高い。『どれが最強か』だけでなく、用途ごとの最適配分まで落とし込むと意思決定が早くなる。",
            "quote_en": "Comparisons like this are high-value. Not just \"which model wins,\" but how to allocate models by use case for faster decisions.",
            "reason": "検証を称える文面は好感を得やすく、引用RTでの拡散にも向く。",
        }
    if topic == "business":
        return {
            "reply_ja": "この視点すごく重要です。価格の見え方だけでなく、補助が切れた後の運用コストまで先に見ているのがさすがです。",
            "reply_en": "This is an excellent perspective. Looking beyond headline pricing to post-subsidy operating cost is exactly the right lens.",
            "quote_ja": "この投稿の価値は大きい。AIツールは機能競争だけでなく、単価と運用効率の設計力で差がつくフェーズに入っている。",
            "quote_en": "This is a high-value take. AI tooling is now a competition of pricing resilience and workflow efficiency, not features alone.",
            "reason": "ビジネス視点を肯定しつつ補足すると、実務層からの反応が増えやすい。",
        }
    if topic == "design":
        return {
            "reply_ja": "この方向性すごく良いですね。実装前の意思決定が速くなるので、チーム全体のスループット改善に直結しそうです。",
            "reply_en": "This direction is excellent. Faster pre-implementation decisions should directly improve overall team throughput.",
            "quote_ja": "このアプローチ好きです。デザイン系AIの本質は見た目だけでなく、意思決定の往復回数を減らして前進速度を上げること。",
            "quote_en": "Love this approach. The real value of design AI is not visuals alone, but reducing decision loops and increasing shipping speed.",
            "reason": "デザイン文脈ではポジティブな実務効果を明示すると共感を得やすい。",
        }
    if topic == "agent":
        return {
            "reply_ja": "運用設計まで踏み込んでいるのが素晴らしいです。観測・再実行・フォールバックが揃っていて、実戦投入しやすい構成ですね。",
            "reply_en": "Great depth on operational design. With observability, retries, and fallbacks in place, this looks highly production-ready.",
            "quote_ja": "こういう投稿は勉強になる。エージェント活用はデモ精度以上に、監視と復旧を含む運用耐性で差がつく。",
            "quote_en": "Posts like this are highly educational. Agent workflows are differentiated not by demos alone, but by operational resilience with monitoring and recovery.",
            "reason": "実務の完成度を先に褒めると、専門的な補足コメントも受け入れられやすい。",
        }
    if topic == "opinion":
        return {
            "reply_ja": "この感覚に共感します。率直に言語化してくれる投稿はありがたいですし、次の行動に繋がりやすいですね。",
            "reply_en": "I really resonate with this. Honest framing like this is valuable and makes the next practical steps clearer.",
            "quote_ja": "こういう本音の投稿は価値がある。悲観か楽観かより、どの工程を再設計してレバレッジを上げるかが鍵だと思う。",
            "quote_en": "These honest posts are valuable. Beyond optimism vs doom, the key is redesigning workflows to increase leverage.",
            "reason": "共感を前面に出すと、会話が前向きに始まりやすい。",
        }
    return {
        "reply_ja": "面白くて学びのある視点です。実運用の観点まで繋がると、さらに多くの人に再利用される知見になりそうです。",
        "reply_en": "This is insightful and genuinely useful. Connecting it to concrete operational context would make it even more reusable.",
        "quote_ja": "この投稿、示唆が多い。現場文脈と運用設計まで接続すると、さらに価値が伸びるタイプ。",
        "quote_en": "This post carries strong signal. Its value grows even more when connected to real workflow context and operational design.",
        "reason": "まず価値を明確に肯定することで、自然に前向きな対話に入りやすい。",
    }


def main() -> None:
    args = parse_args()
    trend_dir = pathlib.Path(args.trend_dir).resolve()
    owner_dir = pathlib.Path(args.owner_dir).resolve()

    files = sorted(trend_dir.glob(args.pattern))
    if not files:
        raise SystemExit("no trend files found")

    target_date = args.target_date.strip()
    if not target_date:
        m = re.search(r"(\d{8})", args.pattern)
        if m:
            d = dt.datetime.strptime(m.group(1), "%Y%m%d").date()
            target_date = d.isoformat()
        else:
            target_date = dt.date.today().isoformat()

    analysis_path = pathlib.Path(args.analysis_json).resolve() if args.analysis_json.strip() else None
    bucket_map = load_bucket_map(analysis_path)
    crm_handles = load_crm_handles(owner_dir)

    posts: List[Post] = []
    for f in files:
        posts.extend(parse_posts(f))

    if not posts:
        raise SystemExit("no posts parsed from trend files")

    mention_count: Dict[str, int] = defaultdict(int)
    file_sets: Dict[str, Set[str]] = defaultdict(set)
    best_post: Dict[str, Post] = {}

    for p in posts:
        h = p.handle.lower()
        mention_count[h] += 1
        file_sets[h].add(p.file)
        prev = best_post.get(h)
        if prev is None or p.likes > prev.likes:
            best_post[h] = p

    scored = []
    for h, post in best_post.items():
        mentions = mention_count[h]
        file_count = len(file_sets[h])
        likes = post.likes
        crm_bonus = 8 if h in crm_handles else 0
        bucket = bucket_map.get(h, "n/a")
        score = mentions * 6 + file_count * 4 + min(likes // 100, 30) + crm_bonus + bucket_bonus(bucket)
        topic = detect_topic(post.excerpt)
        scored.append(
            {
                "handle": post.handle,
                "url": post.url,
                "excerpt": post.excerpt,
                "likes": likes,
                "mentions": mentions,
                "files": file_count,
                "score": score,
                "crm": h in crm_handles,
                "bucket": bucket,
                "topic": topic,
            }
        )

    scored.sort(key=lambda x: (x["score"], x["mentions"], x["likes"]), reverse=True)
    items = scored[: max(1, args.max_items)]

    high_n = min(20, len(items))
    mid_n = min(30, max(0, len(items) - high_n))
    high = items[:high_n]
    mid = items[high_n : high_n + mid_n]
    low = items[high_n + mid_n :]

    target_tag = target_date.replace("-", "")
    default_out = owner_dir / f"{target_tag}_trend_reply_quote_drafts_{len(items)}.md"
    output_path = pathlib.Path(args.output).resolve() if args.output.strip() else default_out
    output_path.parent.mkdir(parents=True, exist_ok=True)

    lines: List[str] = []
    lines.extend(
        [
            f"# @{args.owner} トレンド起点 引リツ・リプライ案（{target_date}）",
            "",
            f"{target_date} の `02_全社_競合・ベンチマーク/日次_SNSトレンド`（{len(files)}ファイル）から、絡みに行く候補を {len(items)} 件作成。",
            "",
            "---",
            "",
        ]
    )

    idx = 1

    def append_section(title: str, section_items: List[Dict[str, object]]) -> None:
        nonlocal idx
        if not section_items:
            return
        lines.append(title)
        lines.append("")
        for it in section_items:
            d = drafts_for(str(it["topic"]))
            rel = "CRM" if it["crm"] else "non-CRM"
            bucket = str(it["bucket"])
            lines.append(
                f"### {idx}. @{it['handle']} [trend {it['mentions']}/{it['files']} files, likes {it['likes']}, {rel}, bucket:{bucket}]"
            )
            lines.append("")
            lines.append(f"**狙う投稿**: {compact(str(it['excerpt']), max_len=80)}")
            lines.append(f"- URL: {it['url']}")
            lines.append("")
            lines.append(f"**内容**: {compact(str(it['excerpt']), max_len=180)}")
            lines.append("")
            lines.append("**リプライ案（日本語）**:")
            lines.append(f"> {d['reply_ja']}")
            lines.append("")
            lines.append("**Reply Draft (English)**:")
            lines.append(f"> {d['reply_en']}")
            lines.append("")
            lines.append("**引用RT案（日本語）**:")
            lines.append(f"> {d['quote_ja']}")
            lines.append("")
            lines.append("**Quote RT Draft (English)**:")
            lines.append(f"> {d['quote_en']}")
            lines.append("")
            lines.append(
                f"**理由**: trend出現{it['mentions']}回 / {it['files']}ファイル、最大いいね{it['likes']}、関係性:{rel}（{bucket}）。{d['reason']}"
            )
            lines.append("")
            lines.append("---")
            lines.append("")
            idx += 1

    append_section("## 優先度 高（即対応 / 20件）", high)
    append_section("## 優先度 中（今日〜明日 / 30件）", mid)
    append_section("## 優先度 低（監視しつつ対応 / 残り）", low)

    top = [f"@{x['handle']}" for x in items[:10]]
    lines.extend(
        [
            "## 今日のアクション",
            "",
            *(f"{i+1}. **{h}** にリプライ or 引用RT" for i, h in enumerate(top)),
            "",
        ]
    )

    output_path.write_text("\n".join(lines), encoding="utf-8")
    print(f"output_file={output_path}")
    print(f"source_files={len(files)}")
    print(f"source_posts={len(posts)}")
    print(f"unique_handles={len(best_post)}")
    print(f"items_total={len(items)}")


if __name__ == "__main__":
    main()
