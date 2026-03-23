---
name: cms-draft-crossposting
description: ローカル記事原稿と assets を使って、Medium / Dev.to / Substack の下書きを整えるスキル。`Mediumに下書きを入れる`、`Dev.to の画像とリンクを直す`、`Substack settings を整える`、`3媒体にクロスポストする` といった依頼で使用する。
---

# CMS Draft Crossposting

このスキルは、`24_SNS_X/...` 配下のローカル原稿を source of truth として、Medium / Dev.to / Substack の draft を整える。

## 入力

- source files は `24_SNS_X/` 配下から探索する。
- platform draft の候補:
  - `**/*_medium.md`
  - `**/*_devto.md`
  - `**/*_substack.md`
- article root の候補:
  - `<topic>/`
  - `_published/<topic>/`
  - `_archive/<topic>/`
- 画像は固定位置を仮定せず、選んだ article root の近傍にある `assets/` を使う。
- 3 platform の原稿が全部そろっているとは仮定しない。対象 platform の source file が 1 つでも見つかれば作業を始めてよい。
- `agent-browser` 管理の既存 Chrome / CDP セッション

## ワークフロー

1. まず `24_SNS_X/` 配下を見て、対象記事の platform source file と article root を特定する。
2. candidate が複数ある場合は、最も近い階層で `*_medium.md` / `*_devto.md` / `*_substack.md` と `assets/` がまとまっている root を優先する。
3. `assets/` が sibling に無い場合は、1 つ上の article root まで遡って探す。見つからなければ本文だけ先に扱い、画像不足を明示して止める。
4. 対象 platform に対応する source file を source of truth にする。別 platform の原稿は補助参照に留める。
5. 1 回の作業では 1 platform だけ触る。複数 CMS を同時に進めない。
6. 画像、リンク、タグ、preview metadata を編集したら、fresh load で保存反映を確認する。
7. brittle な箇所は `agent-browser` 単体に固執せず、Playwright-over-CDP に切り替える。
8. platform ごとの詳細は必要な reference だけ読む。

## Reference

- Medium: [references/medium.md](references/medium.md)
- Dev.to: [references/devto.md](references/devto.md)
- Substack: [references/substack.md](references/substack.md)

## 共通ルール

- DOM を直接書き換えて見た目だけ変わっても、fresh load で戻るなら失敗。
- raw source field がある CMS では、rendered preview ではなく raw source を確認する。
- autocomplete でタグを入れる CMS では、typed text ではなく selected chip を確認する。
- settings / preview / topics / tags が本文 editor とは別面にあることを前提にする。
- 他 skill への導線は関連リンクとしてのみ扱う。後続 skill を自動実行前提にしない。

## Related Skills

- [[sources-article-writer]]: Upstream article writing and revision before crossposting.
- [[x-engagement-drafts]]: Downstream X quote-post / reply drafts after publication.

## 完了条件

- 画像が保存済みで、fresh load 後も残る。
- リンクが raw source で正しく markdown / anchor 化されている。
- タグ / topics / description / preview title / subtitle が再読込後も残る。
- 最終確認で、対象 platform の draft URL と主要設定値を報告できる。
