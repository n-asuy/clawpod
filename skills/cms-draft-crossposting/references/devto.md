# Dev.to

## Stable Rules

- preview ではなく `/edit` の raw markdown を source of truth として扱う。
- `#article_body_markdown` を直接確認する。
- 画像は先に upload して URL を確定し、その後 markdown 本文に差し込む。

## 画像

- hidden upload field があれば `#image-upload-field` を使う。
- upload 後は `dev-to-uploads.s3.amazonaws.com` の URL が本文に入っていることを確認する。
- preview だけ見て完了扱いにしない。fresh edit load で raw markdown を再確認する。

## リンク

- rendered preview の anchor ではなく、`#article_body_markdown` の markdown link syntax を確認する。
- 典型例:
  - `[arXiv:2510.01174](https://arxiv.org/abs/2510.01174)`
  - `[arXiv:2602.12430](https://arxiv.org/abs/2602.12430)`

## タグ

- tag input に文字を入れて Enter だけでは未確定のことがある。
- autocomplete popover の `role="option"` を選んで chip 化する。
- 保存後は `.c-autocomplete--multi__tag-selection` の selected tags を fresh load で確認する。

## Verification

- title
- `#article_body_markdown`
- selected tags
- save draft 後の fresh `/edit` load
