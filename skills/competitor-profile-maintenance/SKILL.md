---
name: competitor-profile-maintenance
description: `agent-browser` を使って `02_全社_競合・ベンチマーク/プロファイル` を反復運用するためのスキル。競合/アカウントプロファイル更新、XやWebページのraw再取得、プロファイル `index.md` メタデータ（`last_collected`、`collection_count`、`updated`）再生成、`raw/`・`log/`・`index.md` のフォルダ構成標準化が必要なときに使用する。
---

# 競合プロファイル保守

## ワークフロー

1. 対象プロファイルディレクトリと取得元URLを確定する。
2. `scripts/collect_profile_raw.sh` で最新rawテキストを取得する。
3. `scripts/refresh_profile_index.py` で `index.md` の要約メタデータ/frontmatterを更新する。
4. 変更点（新規rawファイルパス、スクリーンショットパス、更新メタデータ値）を報告する。

ユーザーから指定がない限り、リポジトリルートで実行する。

## 収集ステップ

決定的に再実行できる raw 収集には `scripts/collect_profile_raw.sh` を使用する。

```bash
.agents/skills/competitor-profile-maintenance/scripts/collect_profile_raw.sh \
  --target-dir "02_全社_競合・ベンチマーク/プロファイル/jasonzhou1993" \
  --platform x \
  --id jasonzhou1993 \
  --url "https://x.com/jasonzhou1993" \
  --cdp-url "http://localhost:9222" \
  --wait-ms 3500 \
  --scroll-steps 24 \
  --max-duration-sec 480
```

挙動:
- CDP経由で既存Chromeに接続する。
- URLを開いて固定ミリ秒待機する（Xでは `networkidle` に依存しない）。
- `x` の場合はタイムラインをスクロールし、投稿レコードのみを抽出する（CSS/ページ装飾は除外）。
- より深い履歴が必要な場合は `--scroll-steps` を増やす。
- `--max-duration-sec` で実行時間を強制上限にする（対スクレイピング/レート制限対策、`0` で無効）。
- 実行ごとに分離された `agent-browser` セッションを使い、終了時に自動クローズする。
- `raw/<timestamp>_<platform>_<id>_raw.md` にアーカイブ形式で保存する（内容）:
  - frontmatter: `tags`, `username`, `display_name`, `collected_at`, `period`, `stats`, `source`
  - sections: `概要`, `ハイライト投稿（Top 10）`, `投稿一覧（日付別）`, `統計`, `収集ログ`
  - each post includes `日本語訳`
- `web` の場合はページ全文テキストを保持する。
- スクリーンショットを `log/<timestamp>_<platform>_<id>.png` に保存する。
- 収集メタデータを `log/manifest.tsv` に追記する。

認証/2FA が必要な場合は、ユーザーにブラウザで完了してもらってから同じコマンドを再実行する。

## インデックス更新ステップ

各収集後に `scripts/refresh_profile_index.py` を実行する:

```bash
python3 .agents/skills/competitor-profile-maintenance/scripts/refresh_profile_index.py \
  --profile-dir "02_全社_競合・ベンチマーク/プロファイル/jasonzhou1993" \
  --username jasonzhou1993 \
  --display-name "Jason Zhou" \
  --platform x \
  --url "https://x.com/jasonzhou1993" \
  --watch-priority medium
```

挙動:
- `raw/` の最新ファイルを検出し、`post_count` または `stats.total` を `collection_count` として使う（フォールバック: rawファイル数）。
- ファイル名から最新収集タイムスタンプを検出する。
- `index.md` の frontmatter キーをアップサートする:
  - `username`
  - `display_name`
  - `platform`
  - `url`
  - `watch_priority`
  - `last_collected`
  - `collection_count`
  - `created` (if missing)
  - `updated`

`index.md` が存在しない場合は、frontmatter と初期セクション付きの最小ファイルを作成する。

## ディレクトリ規則

`references/profile-layout.md` のレイアウトに従う。

命名規則は次で統一する:
- raw収集: `<YYYYMMDD_HHMM>_<platform>_<id>_raw.md`
- スクリーンショット: `<YYYYMMDD_HHMM>_<platform>_<id>.png`
- プロファイル要約: `index.md`

## 検証チェックリスト

- 今回の実行で `raw/` に新規ファイルがちょうど1件追加されたことを確認する。
- X収集ファイルがアーカイブ用frontmatter（`period`, `stats`）で始まり、CSSボイラープレートを含まないことを確認する。
- `log/manifest.tsv` に対応タイムスタンプの新規行があることを確認する。
- `index.md` の frontmatter の日付/件数が更新されていることを確認する。
