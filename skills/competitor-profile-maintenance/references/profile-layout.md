# Profile Layout

Use this layout under `02_全社_競合・ベンチマーク/プロファイル/<profile-id>/`.

## Required Files

- `index.md`
- `raw/`
- `log/manifest.tsv`

## Naming Rules

- Raw capture file:
  - `<YYYYMMDD_HHMM>_<platform>_<id>_raw.md`
  - Example: `20260226_0830_x_jasonzhou1993_raw.md`
  - For `x`, file body is archive-style markdown (`概要`/`ハイライト投稿`/`投稿一覧`/`統計`) rather than full page text dump.
- Screenshot file:
  - `<YYYYMMDD_HHMM>_<platform>_<id>.png`
  - Example: `20260226_0830_x_jasonzhou1993.png`

## `index.md` Frontmatter Keys

- `username`
- `display_name`
- `platform`
- `url`
- `watch_priority`
- `last_collected`
- `collection_count`
- `created`
- `updated`

## `manifest.tsv` Columns

1. `timestamp`
2. `platform`
3. `id`
4. `source_url`
5. `resolved_url`
6. `title`
7. `raw_file`
8. `screenshot_file`
