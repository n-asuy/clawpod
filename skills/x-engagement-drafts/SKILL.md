---
name: x-engagement-drafts
description: Xの当日投稿アーカイブから、引リツ（引用RT）とリプライの文案を和英で作成するスキル。`引リツ案を作る`、`リプライ案をまとめる`、`当日投稿ベースでエンゲージメント文面を作る` といった依頼で使用する。
---

# X Engagement Drafts

## ワークフロー

1. 当日投稿サマリー（`crm/daily_post_reports/YYYYMMDD_summary.md`）を入力する。
2. `scripts/generate_x_engagement_drafts.py` を実行する。
3. 出力されたドラフト（優先度 高/中/低）を確認し、必要なら語調を微調整する。

## 実行コマンド

```bash
python3 .agents/skills/x-engagement-drafts/scripts/generate_x_engagement_drafts.py \
  --summary-file "24_SNS_X/n_asuy/crm/daily_post_reports/20260307_summary.md"
```

### 日次SNSトレンド入力（100件規模の候補を作る場合）

```bash
python3 .agents/skills/x-engagement-drafts/scripts/generate_x_engagement_drafts_from_trends.py \
  --trend-dir "02_全社_競合・ベンチマーク/日次_SNSトレンド" \
  --pattern "20260308_*_x_*.md" \
  --owner-dir "24_SNS_X/n_asuy" \
  --owner "n_asuy" \
  --target-date "2026-03-08" \
  --analysis-json "24_SNS_X/n_asuy/follow_analysis/analysis/20260307_1506_x_n_asuy_following_analysis.json" \
  --max-items 100
```

## 出力先（デフォルト）

- `24_SNS_X/<owner>/YYYYMMDD_reply_quote_drafts.md`

## 任意オプション

- `--output`: 出力ファイルを明示指定
- `--analysis-json`: `follow_analysis` の分類結果JSONを指定（優先度付けに使用）
- `--max-items`: 生成件数上限（デフォルト10）
- `--include-handle`: 特定ハンドルのみ対象
- `--exclude-handle`: 特定ハンドルを除外
- `--trend-dir` / `--pattern`: 日次SNSトレンドからの抽出時に使用

## 生成フォーマット

既存運用に合わせ、各項目を次の形式で出力する:

- `狙う投稿`
- `内容`
- `リプライ案（日本語）`
- `Reply Draft (English)`
- `引用RT案（日本語）`
- `Quote RT Draft (English)`
- `理由`
