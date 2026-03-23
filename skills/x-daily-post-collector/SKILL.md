---
name: x-daily-post-collector
description: XのCRMアカウント全件から当日投稿を収集し、日次サマリー再構築と和訳完了チェックまで一括実行するスキル。`投稿を全部集める`、`本日分の投稿収集`、`daily_post_reportsを更新`、`翻訳チェックまで回す` といった依頼で使用する。
---

# X Daily Post Collector

## ワークフロー

1. `scripts/run_x_daily_post_collection.sh` を実行して全件収集する。
2. 同スクリプト内でサマリー再構築（`--require-all`）を実行する。
3. 同スクリプト内で翻訳完了チェックを実行する。
4. `success_count` / `failure_count` / `translation_check=passed` を確認する。

## 実行コマンド

```bash
.agents/skills/x-daily-post-collector/scripts/run_x_daily_post_collection.sh \
  --accounts-dir "24_SNS_X/n_asuy/crm/accounts" \
  --owner "n_asuy" \
  --date "2026-03-07" \
  --no-cdp-connect
```

## CDPを使う場合

```bash
.agents/skills/x-daily-post-collector/scripts/run_x_daily_post_collection.sh \
  --accounts-dir "24_SNS_X/n_asuy/crm/accounts" \
  --owner "n_asuy" \
  --date "2026-03-07" \
  --cdp-url "http://localhost:9300"
```

## 挙動

- 投稿収集: `.agents/skills/x-follow-analysis/scripts/collect_x_today_posts.py`
- サマリー: `.agents/skills/x-follow-analysis/scripts/build_x_daily_summary.py --require-all`
- 翻訳チェック: `.agents/skills/x-follow-analysis/scripts/check_x_translation_completion.py`

## 主要オプション

- `--no-cdp-connect`: CDP接続を省略し、agent-browser管理セッションで収集する。
- `--cdp-url`: CDP接続先を指定する（`--no-cdp-connect` 未指定時）。
- `--skip-existing`: 既存 `posts/YYYYMMDD.md` をスキップ（デフォルト有効）。
- `--max-duration-sec`: 全体タイムアウト秒（デフォルト 5400）。
- `--session-name`: agent-browser セッション名を固定する。

## 出力

- `crm/posts/<handle>/YYYYMMDD.md`
- `crm/daily_post_reports/YYYYMMDD_summary.md`
- 翻訳完了チェック結果（`translation_check=passed`）
