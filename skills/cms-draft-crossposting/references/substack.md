# Substack

## Stable Rules

- 本文 editor の subtitle と、settings の description は別物。
- tags は settings 側で管理する。
- settings modal / sidebar が既に開いている場合がある。新しく開く前に DOM を確認する。

## 本文側

- title
- subtitle
- 本文

これらは editor surface で確認する。

## Settings 側

確認 / 編集対象:

- description
- tags
- social preview

よく使う field:

- description: `textarea.file-sidebar-item-text-input-editable-multiline`
- tags: `input.pencraft.input-X5E9i8`

## タグ

- input に文字を入れて Enter で chip 化する。
- 保存後は body text の `Add tags` セクションや settings 再オープンで tag 名を確認する。
- 今回の安定例:
  - `AI Agents`
  - `Open Source`
  - `Machine Learning`
  - `GitHub`

## Verification

1. settings で description / tags を入れる。
2. `Done` を押す。
3. settings を開き直す。
4. `Add tags` セクションと description field を再確認する。
