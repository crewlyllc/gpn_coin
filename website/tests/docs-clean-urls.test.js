// Clean URLs in the generated docs (queue item #255).
// Run: node --test website/tests/docs-clean-urls.test.js
//
// Cloudflare serves clean URLs: /docs/<slug>, /download, /ai, … . Commit 6980e71
// ("seo: 301 the .html URLs, clean the sitemap and internal links") rewrote the
// committed HTML but not scripts/build-site-docs.py, so regenerating the docs
// silently put every .html URL back across 14 files and the next stamp shipped
// the reversal. These checks make that a test failure:
//   * no generated page may link, canonicalise or og:url a .html URL;
//   * the generator's own templates must not contain one either — that is the
//     half a regeneration-check cannot see until it has already shipped.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const SITE = join(HERE, "..");
const ROOT = join(SITE, "..");
const DOCS = join(SITE, "docs");

const URL_ATTRS = [
  /href="([^"]*)"/g,
  /<link rel="canonical" href="([^"]*)"/g,
  /<meta property="og:url" content="([^"]*)"/g,
];

function htmlUrls(text) {
  const out = [];
  for (const re of URL_ATTRS) {
    for (const m of text.matchAll(re)) out.push(m[1]);
  }
  return out;
}

test("no committed docs page emits a .html URL", () => {
  const pages = readdirSync(DOCS).filter((f) => f.endsWith(".html"));
  assert.ok(pages.length >= 10, `expected the generated docs set, found ${pages.length}`);
  const bad = [];
  for (const page of pages) {
    for (const url of htmlUrls(readFileSync(join(DOCS, page), "utf8"))) {
      // normalise: "index.html" is also the old, forbidden form
      if (/\.html(?:[#?].*)?$/.test(url)) bad.push(`${page}: ${url}`);
    }
  }
  assert.deepEqual(bad, [], `clean URLs only — regenerate with scripts/build-site-docs.py`);
});

test("the docs generator's templates emit clean URLs", () => {
  const src = readFileSync(join(ROOT, "scripts", "build-site-docs.py"), "utf8");
  // Only the template half of the file matters: markup the generator writes.
  const bad = [];
  for (const m of src.matchAll(/href=\\?"([^"\\]*)\\?"/g)) {
    if (/\.html$/.test(m[1])) bad.push(m[0]);
  }
  for (const m of src.matchAll(/content="[^"]*\.html"/g)) bad.push(m[0]);
  assert.deepEqual(bad, [], "build-site-docs.py must not template a .html URL");
});
