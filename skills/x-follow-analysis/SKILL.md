---
name: x-follow-analysis
description: Xのフォロー一覧を分析・保守するスキル。CDP（agent-browser）での収集を優先し、必要時はApify MCPをフォールバックとして利用する。アカウントを keep/review/unfollow バケットに分類し、フォロー監査、フォロー整理、定期スナップショット取得、フォロー一覧収集、アンフォロー候補レポート生成の依頼に対応する。
---

# Xフォロー分析

## ワークフロー

1. （CDP収集時は必須）Chromeをremote debugging付きで起動し、`9300` 番台から連番でCDPポートを確保する。
2. 対象ディレクトリとXユーザー名を確定する。
3. 現在のフォロー一覧スナップショットを収集する（CDP優先、Apifyはフォールバック）。
4. `references/config.template.json` からルール設定JSONを準備する。
5. `scripts/analyze_x_following.py` を実行してアカウントを分類する。
6. 出力パスと件数（`keep`、`engage_without_follow`、`manual_review`）を報告する。
7. （任意）運用向けにCRMプロファイルディレクトリを生成する。
8. （任意）CRMアカウント配下に当日投稿を収集して日次アーカイブを更新する。
9. （8を実行した場合は必須）投稿本文の和訳を必ず埋める（プレースホルダ禁止）。
10. （8を実行した場合は必須）和訳完了チェックを実行し、未翻訳0件を確認する。

ユーザーから指定がない限り、リポジトリルートでコマンドを実行する。

## 事前準備（CDP / 必須）

`agent-browser` で収集する前に、ヘルパースクリプトでCDPポート（`9300-9399`）を先頭から順に確保する:

```bash
.agents/skills/x-follow-analysis/scripts/start_chrome_cdp.sh
```

出力例:

```text
cdp_port=9300
cdp_url=http://localhost:9300
profile_dir=/tmp/nasuy-debug-profile-9300
chrome_pid=12345
log_file=/tmp/nasuy-debug-profile-9300/logs/chrome_cdp_9300.log
cdp_ready=true
```

起動確認（必要に応じて）:

```bash
curl -s http://localhost:9300/json/version
```

補足:
- 並列実行では、各ワーカーごとに別CDP URL（例: `9300`, `9301`, `9302`）を使う。
- `start_chrome_cdp.sh` は `--user-data-dir=/tmp/nasuy-debug-profile-<port>` を使うため、プロファイル競合を避けられる。
- Xログイン/2FAが未完了の場合は、先に `https://x.com/home` でログインを完了してから収集を実行する。
- `No page found` が出る場合は、CDP接続先Chromeにタブがない可能性がある。`https://x.com/home` を1タブ開いて再実行する。
- `x.com/<handle>/following` がXロゴ表示で止まる場合は、`https://x.com/home` またはプロフィールページを先に開いてから再試行する。

## 収集ステップ（CDP: 優先）

決定的に再実行できる収集スクリプトを使用する:

```bash
  .agents/skills/x-follow-analysis/scripts/collect_x_following_raw.sh \
  --target-dir "23_SNS_X_セキュリティ系/follow_analysis/my_account" \
  --username "my_account" \
  --cdp-url "http://localhost:9300" \
  --wait-ms 3000 \
  --scroll-steps 45 \
  --max-duration-sec 480
```

挙動:
- CDP経由でChromeに接続し、`https://x.com/<username>/following` を開く。
- 固定ステップでスクロールし、`UserCell` カードを収集する。
- `--max-duration-sec` で実行時間を強制上限にする（対スクレイピング/レート制限対策、`0` で無効）。
- 実行ごとに分離された `agent-browser` セッションを使い、終了時に自動クローズする。
- raw JSONとMarkdownアーカイブを `raw/` に保存する。
- スクリーンショットを `log/` に保存し、`log/manifest.tsv` に追記する。
- 主要な出力パスと件数を返す。

Xのログイン/2FAが必要な場合は、ユーザーにブラウザで完了してもらってから再実行する。

## 収集ステップ（Apify: フォールバック）

CDP/agent-browser が使えない場合は、Apify MCPツールを使用する:

1. Apify Actorを呼び出す（Actor一覧と入力スキーマは `references/apify-actors.md` を参照）。
2. Actor出力をJSONLとして保存する。
3. 次で正規化する:

```bash
python3 .agents/skills/x-follow-analysis/scripts/normalize_apify_following.py \
  --input-jsonl "<raw_apify_output>.jsonl" \
  --output "<normalized>.json"
```

4. 収集パイプラインに投入する:

```bash
.agents/skills/x-follow-analysis/scripts/collect_x_following_raw.sh \
  --target-dir "23_SNS_X_セキュリティ系/follow_analysis/my_account" \
  --username "my_account" \
  --input-json "<normalized>.json" \
  --source apify
```

**制約**: Apifyでは `follows_you` を検出できない。すべてのアカウントが `follows_you: false` になるため、keep_score の精度が下がる。可能ならCDPを優先する。

## 分析ステップ

`references/config.template.json` をコピーして、ルール一覧を編集した設定ファイルを準備する。

次を実行する:

```bash
python3 .agents/skills/x-follow-analysis/scripts/analyze_x_following.py \
  --input-json "23_SNS_X_セキュリティ系/follow_analysis/my_account/raw/20260303_0900_x_my_account_following_raw.json" \
  --output-dir "23_SNS_X_セキュリティ系/follow_analysis/my_account/analysis" \
  --config ".agents/skills/x-follow-analysis/references/config.template.json" \
  --max-unfollow 200
```

挙動:
- 収集JSONからフォロー中アカウントを読み込む。
- ハンドルリストとキーワードルールで各アカウントをスコアリングする。
- 次の3バケットに分類する:
  - `keep_follow_and_engage`
  - `engage_without_follow`（アンフォロー候補だが接触は継続）
  - `manual_review`
- Markdownサマリー、CSV、JSON、`*_unfollow_candidates.txt` を出力する。

## CRMプロファイル生成（任意）

バケット中心の分析結果ではなく運用向けのプロファイルが必要な場合は、分析JSONからCRMファイルを生成する:

```bash
python3 .agents/skills/x-follow-analysis/scripts/generate_x_crm_profiles.py \
  --analysis-json "23_SNS_X_セキュリティ系/follow_analysis/my_account/analysis/<timestamp>_x_my_account_following_analysis.json" \
  --output-dir "23_SNS_X_セキュリティ系/follow_analysis/my_account/crm" \
  --owner "my_account" \
  --top-n 200
```

出力:
- `crm/index.md`（優先度順サマリー）
- `crm/crm_contacts.csv`
- `crm/crm_contacts.json`
- `crm/accounts/<handle>.md`（フォロー先1アカウントにつき1ファイル）

## 当日投稿収集（任意）

`crm/accounts` 配下の各ハンドルを巡回して当日投稿を収集し、日次アーカイブを更新する。

```bash
python3 .agents/skills/x-follow-analysis/scripts/collect_x_today_posts.py \
  --accounts-dir "24_SNS_X/n_asuy/crm/accounts" \
  --owner "n_asuy" \
  --cdp-url "http://localhost:9300" \
  --date "2026-03-04" \
  --translation-mode auto \
  --max-duration-sec 1800 \
  --skip-existing
```

挙動:
- `crm/accounts` 内の各ハンドルを巡回し、当日投稿のみ抽出する。
- `accounts/<handle>/` 形式では、投稿を `crm/accounts/<handle>/posts/YYYYMMDD.md` に保存する。
- `accounts/<handle>.md` 形式では、投稿を `crm/posts/<handle>/YYYYMMDD.md` に保存する。
- いずれの形式でも `posts/index.md` を更新する。
- 全体サマリーを `crm/daily_post_reports/YYYYMMDD_summary.md` に出力する。
- 実行ごとに専用 `agent-browser` セッションを使い、終了時に自動クローズする。
- `--command-timeout-sec` で各 `agent-browser` コマンドの待機時間を制限できる。
- `--max-duration-sec` で全体実行時間を制限できる（`0` で無制限）。
- デフォルトの `--translation-mode auto` では、投稿本文の和訳を自動生成し `**和訳**` ブロックに出力する。
- 翻訳は `OPENAI_API_KEY` があれば OpenAI を優先し、未解決分のみHTTP翻訳フォールバックを使う。
- 自動翻訳で未解決が残ると実行は失敗する（`--allow-translation-fallback-placeholder` を指定した場合のみ例外的にプレースホルダ許容）。

## 当日投稿収集（監視重視 / 推奨）

長時間の1プロセスループではなく、**エージェントが1ハンドルずつ実行して進捗を可視化**する。

1. 収集対象ハンドルを列挙する。
2. 未収集ハンドルごとに、次の単体実行を1回ずつ呼ぶ。

```bash
python3 .agents/skills/x-follow-analysis/scripts/collect_x_today_posts.py \
  --accounts-dir "24_SNS_X/n_asuy/crm/accounts" \
  --owner "n_asuy" \
  --cdp-url "9222" \
  --date "2026-03-05" \
  --translation-mode auto \
  --handle "sab8a"
```

補足:
- `--handle` は複数回指定できる（例: `--handle sab8a --handle saltyAom`）。
- `--skip-existing` と併用すると再実行時に安全。
- CDP接続が不安定な場合は `--no-cdp-connect` を使い、agent-browser管理セッションで実行できる。

3. 任意のタイミングでサマリーを再構築し、進捗を監視する。

```bash
python3 .agents/skills/x-follow-analysis/scripts/build_x_daily_summary.py \
  --accounts-dir "24_SNS_X/n_asuy/crm/accounts" \
  --owner "n_asuy" \
  --date "2026-03-05"
```

4. 全件完了後、`--require-all` 付きでサマリーを再構築し、欠損0件を確認する。

```bash
python3 .agents/skills/x-follow-analysis/scripts/build_x_daily_summary.py \
  --accounts-dir "24_SNS_X/n_asuy/crm/accounts" \
  --owner "n_asuy" \
  --date "2026-03-05" \
  --require-all
```

## 和訳フロー（必須 / プレースホルダ禁止）

和訳は `collect_x_today_posts.py` 実行時に同時生成し、プレースホルダを残さない。

1. 当日投稿収集を `--translation-mode auto` で実行する（デフォルト）。
2. 実行後に `crm/daily_post_reports/YYYYMMDD_summary.md` を確認する。
3. 次のチェックコマンドを実行し、`translation_check=passed` を確認する。

```bash
python3 .agents/skills/x-follow-analysis/scripts/check_x_translation_completion.py \
  --summary-file "24_SNS_X/n_asuy/crm/daily_post_reports/YYYYMMDD_summary.md"
```

4. チェック失敗時は、失敗ファイル/失敗IDを修正して再実行する。
5. 最後に更新したファイル一覧と和訳件数を報告する。

## 判定モデル

- `keep_follow_and_engage`:
  - 関係性シグナルが可視性シグナルより強い。
  - 主な理由: keep対象ハンドル明示、`follows_you`、keepキーワード。
- `engage_without_follow`:
  - 可視性ターゲットシグナルが関係性シグナルより強い。
  - 主な理由: 可視性/アンフォロー対象ハンドル明示、可視性キーワード、アンフォローキーワード。
- `manual_review`:
  - シグナルが弱い、または競合している。

しきい値は分析スクリプトのフラグ（`--min-keep-score`, `--min-visibility-score`, `--min-unfollow-score`）で調整する。

## ディレクトリ規則

`references/layout.md` に従う。

## 安全ルール

- このワークフローで一括アンフォローを自動実行しない。
- 破壊的操作の前に必ず候補を提示し、ユーザー確認を取る。
- ユーザーが設定で明示的に上書きしない限り、`follows_you` アカウントはデフォルトで維持する。

## 検証チェックリスト

- raw JSONとraw Markdownがそれぞれ1件ずつ新規作成されたことを確認する。
- `log/manifest.tsv` に同一タイムスタンプの新規行が追加されたことを確認する。
- 分析出力（md/csv/json/txt）が存在することを確認する。
- ルール上妥当な場合にのみアンフォロー候補件数が非ゼロになることを確認する。
- 当日投稿収集を実施した場合、`check_x_translation_completion.py` が `translation_check=passed` を返すことを確認する。
