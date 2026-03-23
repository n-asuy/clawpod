# Medium

## Stable Rules

- 画像は `png` / `jpg` を優先する。`webp` は不安定なので、必要なら `sips` で変換する。
- 本文 editor と preview metadata は別面として扱う。
  - 本文: `https://medium.com/p/<draft-id>/edit`
  - preview / topics: `https://medium.com/p/<draft-id>/submission?...`
- `innerHTML` / `insertHTML` / `<figure>` の直接注入は信用しない。
- 空段落や caption placeholder は DOM 削除ではなく、実際の selection + keyboard 操作で消す。

## 画像

1. 挿入位置の段落に caret を置く。
2. `Add an image, video, embed, or new part` を展開する。
3. `Add an image` の hidden input / file chooser にファイルを流す。
4. async upload 後、fresh edit page で image count を再確認する。

補足:

- `ab upload 'input[type="file"]' ...` が不安定なら Playwright-over-CDP に切り替える。
- `button[aria-label="Add an image"]` は menu 展開前だと `0x0` のことがある。先に `+` を開く。

## 本文整形

- 余分な空行は `p.graf--empty` を DOM 削除しない。
- 該当段落に selection を置いて `Backspace` を打つ。これで Medium の保存モデルに乗る。
- 見出しや本文修正も同様に、実 editor 操作を優先する。

## Preview Metadata

- preview title は本文タイトルと一致させる。
- preview subtitle は 140 文字制限に収める。
- topics は 5 個まで。入力欄 placeholder が `Add a topic...` から `Add more topics...` に変わることがある。

## Verification

- edit URL を fresh load して `img` と本文 block を確認する。
- submission URL を fresh load して preview title / subtitle / topics を確認する。
- `Saving...` や save error banner が出たら、そのタブは信用せず fresh load で再確認する。
