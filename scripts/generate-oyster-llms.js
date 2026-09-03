// Generates llms.txt and llms-full.txt from the Oyster docs markdown.
// Output: docs/static/llms.txt and docs/static/llms-full.txt

const fs = require("fs");
const path = require("path");

const DOCS_DIR = path.resolve(__dirname, "../docs/src");
const OUT_DIR = path.resolve(__dirname, "../docs/static");
const BASE_URL = "https://docs.wal.app";

function walk(dir, results = []) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      walk(full, results);
    } else if (entry.name.endsWith(".md") && entry.name !== "SUMMARY.md") {
      results.push(full);
    }
  }
  return results;
}

function extractTitle(content) {
  const h1 = content.match(/^#\s+(.+)$/m);
  return h1 ? h1[1].replace(/\[([^\]]+)\]\([^)]+\)/g, "$1").trim() : null;
}

function extractDescription(content) {
  // Skip frontmatter and heading, get first paragraph
  let body = content.replace(/^#\s+.+$/m, "");
  body = body.replace(/```[\s\S]*?```/g, "");
  body = body.replace(/\[([^\]]+)\]\([^)]+\)/g, "$1");
  body = body.replace(/<[^>]+>/g, "");
  body = body.replace(/\n+/g, " ").trim();
  // Take first ~200 chars ending at a sentence
  const chunk = body.slice(0, 300);
  const lastDot = chunk.lastIndexOf(".");
  if (lastDot > 50) return chunk.slice(0, lastDot + 1).trim();
  return chunk.slice(0, 200).trim();
}

function fileToSlug(filePath) {
  let rel = path.relative(DOCS_DIR, filePath).replace(/\\/g, "/");
  rel = rel.replace(/\.md$/, "");
  rel = rel.replace(/\/README$/, "");
  if (rel === "README") rel = "";
  return rel;
}

function toSection(slug) {
  const parts = slug.split("/");
  if (parts.length >= 2) {
    return (
      "Walrus Oyster API: " +
      parts[0]
        .replace(/[-_]/g, " ")
        .replace(/\b\w/g, (c) => c.toUpperCase())
        .replace(/\bApi\b/g, "API")
        .replace(/\bS3\b/gi, "S3")
        .replace(/\bJson\b/g, "JSON")
    );
  }
  return "Walrus Oyster API";
}

const files = walk(DOCS_DIR).sort();
const pages = [];

for (const file of files) {
  const content = fs.readFileSync(file, "utf8");
  const title = extractTitle(content) || path.basename(file, ".md");
  const description = extractDescription(content);
  const slug = fileToSlug(file);
  const mdUrl = `${BASE_URL}/oyster/${slug || "index"}.md`;
  const section = toSection(slug);
  pages.push({ title, description, mdUrl, section });
}

// Group by section
const sections = {};
const sectionOrder = [];
for (const page of pages) {
  if (!sections[page.section]) {
    sections[page.section] = [];
    sectionOrder.push(page.section);
  }
  sections[page.section].push(page);
}

const intro =
  "Walrus Oyster API is a Web2-friendly object storage service " +
  "backed by Walrus (decentralized blob storage) and Sui (onchain " +
  "state). It provides familiar HTTP and S3-compatible APIs while " +
  "data is stored on a decentralized network.";

// llms.txt (with descriptions)
const lines = [
  "# Walrus Oyster API",
  "",
  `> ${intro}`,
  "",
  `> For the complete page index see`,
  `[llms-full.txt](${BASE_URL}/oyster/llms-full.txt).`,
  "",
  `- [OpenAPI Specification (JSON)](${BASE_URL}/oyster/openapi.json):`,
  "    Machine-readable OpenAPI 3.1 spec for the Walrus Oyster API.",
  "",
];

for (const section of sectionOrder) {
  lines.push(`## ${section}`, "");
  for (const page of sections[section]) {
    if (page.description) {
      lines.push(`- [${page.title}](${page.mdUrl}):`);
      // Wrap description at ~96 chars with 4-space indent
      const words = page.description.split(" ");
      let line = "    ";
      for (const word of words) {
        if (line.length + word.length > 96 && line.trim()) {
          lines.push(line.trimEnd());
          line = "    ";
        }
        line += word + " ";
      }
      if (line.trim()) lines.push(line.trimEnd());
    } else {
      lines.push(`- [${page.title}](${page.mdUrl})`);
    }
  }
  lines.push("");
}

// llms-full.txt (compact, no descriptions)
const fullLines = [
  "# Walrus Oyster API",
  "",
  `> ${intro}`,
  "",
  `- [OpenAPI Specification (JSON)](${BASE_URL}/oyster/openapi.json)`,
  "",
];

for (const section of sectionOrder) {
  fullLines.push(`## ${section}`, "");
  for (const page of sections[section]) {
    fullLines.push(`- [${page.title}](${page.mdUrl})`);
  }
  fullLines.push("");
}

fs.mkdirSync(OUT_DIR, { recursive: true });
fs.writeFileSync(path.join(OUT_DIR, "llms.txt"), lines.join("\n"));
fs.writeFileSync(path.join(OUT_DIR, "llms-full.txt"), fullLines.join("\n"));

console.log(
  `✅ Generated llms.txt (${pages.length} pages, ` +
    `${sectionOrder.length} sections)`,
);
