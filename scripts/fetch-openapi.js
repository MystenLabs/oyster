// Fetches the OpenAPI spec from the live Oyster testnet Scalar page
// and saves it as docs/static/openapi.json.
//
// The spec is embedded inline in the HTML page served at /api/docs.

const fs = require("fs");
const path = require("path");

const OUT_DIR = path.resolve(__dirname, "../docs/static");
const OPENAPI_DOCS_URL =
  "https://oyster.testnet.mystenlabs.com/api/docs";

async function main() {
  console.log(`📥 Fetching OpenAPI spec from ${OPENAPI_DOCS_URL}`);

  const resp = await fetch(OPENAPI_DOCS_URL);
  if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
  const html = await resp.text();

  // The spec is embedded as a JSON object in the Scalar HTML page
  let spec = null;

  const specMatch = html.match(
    /(\{"openapi"\s*:\s*"3[\s\S]*?\})\s*(?:<\/script>|,\s*")/,
  );
  if (specMatch) {
    const startIdx = html.indexOf('{"openapi"');
    if (startIdx !== -1) {
      spec = extractJsonObject(html, startIdx);
    }
  }

  if (!spec) {
    const dataMatch = html.match(/data-spec='([^']+)'/);
    if (dataMatch) spec = dataMatch[1];
  }

  if (!spec) {
    throw new Error(
      "Could not extract OpenAPI spec from Scalar HTML page",
    );
  }

  const parsed = JSON.parse(spec);
  if (!parsed.openapi || !parsed.info) {
    throw new Error("Extracted JSON is not a valid OpenAPI spec");
  }

  // Rewrite relative server URL to absolute testnet URL
  if (parsed.servers) {
    parsed.servers = parsed.servers.map((s) => {
      if (s.url && s.url.startsWith("/")) {
        return {
          ...s,
          url: `https://oyster.testnet.mystenlabs.com${s.url}`,
        };
      }
      return s;
    });
  }

  fs.mkdirSync(OUT_DIR, { recursive: true });
  fs.writeFileSync(
    path.join(OUT_DIR, "openapi.json"),
    JSON.stringify(parsed, null, 2),
  );

  const pathCount = parsed.paths
    ? Object.keys(parsed.paths).length
    : "?";
  console.log(`✅ OpenAPI spec saved (${pathCount} paths)`);
}

function extractJsonObject(str, start) {
  let depth = 0;
  let inString = false;
  let escape = false;

  for (let i = start; i < str.length; i++) {
    const ch = str[i];
    if (escape) { escape = false; continue; }
    if (ch === "\\") { escape = true; continue; }
    if (ch === '"') { inString = !inString; continue; }
    if (inString) continue;
    if (ch === "{") depth++;
    if (ch === "}") {
      depth--;
      if (depth === 0) return str.slice(start, i + 1);
    }
  }
  return null;
}

main().catch((err) => {
  console.error(`❌ ${err.message}`);
  process.exit(1);
});
