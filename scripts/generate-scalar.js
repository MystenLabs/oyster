// Generates a standalone Scalar HTML page from the OpenAPI spec.
// Reads: docs/static/openapi.json
// Output: docs/static/scalar.html

const fs = require("fs");
const path = require("path");

const STATIC_DIR = path.resolve(__dirname, "../docs/static");
const specPath = path.join(STATIC_DIR, "openapi.json");

if (!fs.existsSync(specPath)) {
  console.error("❌ openapi.json not found. Run fetch-openapi.js first.");
  process.exit(1);
}

const specJson = fs.readFileSync(specPath, "utf8");

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
  <script id="api-reference" type="application/json">${specJson}</script>
  <script>
    document.addEventListener('DOMContentLoaded', () => {
      const prefersDark =
        window.matchMedia('(prefers-color-scheme: dark)').matches;
      const parentDark = window.parent !== window
        ? window.parent.document.documentElement
            .getAttribute('data-theme') === 'dark'
        : prefersDark;

      const config = {
        theme: 'kepler',
        darkMode: parentDark,
        showSidebar: true,
        hideDownloadButton: false,
        hideModels: false,
        defaultOpenAllTags: true,
        hideSearch: true,
      };

      const el = document.getElementById('api-reference');
      el.dataset.configuration = JSON.stringify(config);
    });
  </script>
  <script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference"></script>
</body>
</html>`;

fs.writeFileSync(path.join(STATIC_DIR, "scalar.html"), html);
console.log("✅ Scalar standalone page generated");
