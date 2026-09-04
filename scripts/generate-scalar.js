// Generates a standalone Scalar HTML page that loads the OpenAPI spec
// from the separate openapi.json file (not inlined).
// Output: docs/static/scalar.html

const fs = require("fs");
const path = require("path");

const STATIC_DIR = path.resolve(__dirname, "../docs/static");

const html = `<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>Walrus Oyster API Reference</title>
  <style>
    body { margin: 0; }
    hr { display: none !important; }
    .darklight-reference-promo { display: none !important; }
    [class*="separator"] { display: none !important; }
  </style>
</head>
<body>
  <div id="api-reference"></div>
  <script>
    document.addEventListener('DOMContentLoaded', async () => {
      const prefersDark =
        window.matchMedia('(prefers-color-scheme: dark)').matches;
      const parentDark = window.parent !== window
        ? window.parent.document.documentElement
            .getAttribute('data-theme') === 'dark'
        : prefersDark;

      const el = document.getElementById('api-reference');
      el.dataset.url = './openapi.json';
      el.dataset.configuration = JSON.stringify({
        theme: 'kepler',
        darkMode: parentDark,
        showSidebar: true,
        hideDownloadButton: false,
        hideModels: false,
        defaultOpenAllTags: true,
        hideSearch: true,
      });
    });
  </script>
  <script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference"></script>
</body>
</html>`;

fs.mkdirSync(STATIC_DIR, { recursive: true });
fs.writeFileSync(path.join(STATIC_DIR, "scalar.html"), html);
console.log("✅ Scalar standalone page generated");
