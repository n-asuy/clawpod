# CMS Editors

Patterns for brittle editors that mix contenteditable blocks, hidden file inputs, overlays, async uploads, and modal metadata editors.

## Hard Rules

1. Work sequentially.
   Never manipulate multiple CMS tabs in parallel on the same CDP session.
2. Use the editor's own persistence model.
   Do not rely on `innerHTML`, `insertHTML`, or direct DOM node insertion for saved content.
3. Verify server-side persistence.
   Reopen the draft in a fresh tab or page after image uploads, link edits, tag changes, and metadata edits.
4. Distinguish body fields from settings fields.
   Many CMSes store subtitle / description / tags / preview data outside the main editor surface.

## When `agent-browser` Alone Is Not Enough

Use Playwright-over-CDP when any of these happen:

- `ab click` is intercepted by overlays or zero-sized buttons
- `ab upload 'input[type="file"]' ...` times out after an editor action
- the editor exposes hidden inputs or file choosers only after JavaScript-side menu expansion
- a modal or submission page becomes a separate tab / page outside the main edit surface

```bash
PW_CORE="$(npm root -g)/agent-browser/node_modules/playwright-core"
PW_CORE="$PW_CORE" node - <<'EOF'
const { chromium } = require(process.env.PW_CORE);
(async () => {
  const browser = await chromium.connectOverCDP('http://127.0.0.1:9400');
  const context = browser.contexts()[0];
  const pages = context.pages().map((p, i) => ({ i, url: p.url() }));
  console.log(JSON.stringify(pages, null, 2));
  await browser.close();
})();
EOF
```

## Image Upload Checklist

1. Put the caret or selection at the real insertion point.
2. Expand the editor's image menu if needed.
3. Wait for hidden file inputs or a file chooser to appear.
4. Upload through the editor-owned control.
5. Wait for async upload and save to settle.
6. Reopen the same draft and verify image count or uploaded asset URLs.

## Link / Tag / Metadata Checklist

- Links: inspect the raw field value where possible.
- Tags: verify selected chips after autocomplete, not only the typed text in the input.
- Preview title / subtitle / description: verify on the page where the CMS stores them, which may be a modal, sidebar, or separate submission URL.
- If a page shows `Saving...` or a save error banner, stop and re-check state before continuing.

## Platform-Specific Note

If the task is end-to-end article draft work for Medium, Dev.to, or Substack, use the dedicated `cms-draft-crossposting` skill if it is available.
