# X Follow Analysis Layout

Use this layout for each target account:

`<base-dir>/<x-username>/`

## Required Files and Directories

- `raw/`
- `analysis/`
- `log/manifest.tsv`

## Naming Rules

- Raw JSON:
  - `<YYYYMMDD_HHMM>_x_<username>_following_raw.json`
- Raw markdown archive:
  - `<YYYYMMDD_HHMM>_x_<username>_following_raw.md`
- Screenshot:
  - `<YYYYMMDD_HHMM>_x_<username>_following.png`
- Analysis outputs:
  - `<YYYYMMDD_HHMM>_x_<username>_following_analysis.{md,csv,json}`
  - `<YYYYMMDD_HHMM>_x_<username>_following_analysis_unfollow_candidates.txt`

## `manifest.tsv` Columns

1. `timestamp`
2. `platform`
3. `id`
4. `source_url`
5. `resolved_url`
6. `title`
7. `total_accounts`
8. `raw_json`
9. `raw_md`
10. `screenshot_file`
11. `collection_source` (`cdp` or `apify`)
