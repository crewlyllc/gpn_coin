/* Tests for row 225: the referral share link is `/join?ref=<code>`
 * (short alias `/r/<code>`), never a `/download?ref=` URL.
 *
 * Run with:  node website/tests/join.test.js
 * (no dependencies, no build — plain node:assert)
 *
 * The rule under test is the promise the link makes. A download URL says
 * "this will install something" before the visitor has decided to trust the
 * referrer; `/join` is a landing page that explains the network, shows the
 * code as plain text (so a screenshot keeps the attribution) and puts the
 * install commands — with the code already in them — behind a deliberate
 * choice. A malformed code must be named as invalid, not silently dropped.
 */
"use strict";

const assert = require("assert");
const fs = require("fs");
const path = require("path");

const R = require("../assets/referral.js");
const SITE = path.join(__dirname, "..");

let passed = 0;
function test(name, fn) {
  fn();
  passed++;
  console.log("ok - " + name);
}

function read(p) {
  return fs.readFileSync(path.join(SITE, p), "utf8");
}

/* ---------- the page exists and carries the four commands ---------- */

test("join.html exists and bakes the code into four install commands", () => {
  const page = read("join.html");
  const placeholders = page.match(/<span data-ref-arg><\/span>/g) || [];
  assert.strictEqual(placeholders.length, 4, "join.html must carry exactly four ref placeholders");
  // The four shapes the rest of the site advertises.
  assert.ok(page.includes("-File C:\\gpn\\bin.pending\\install.ps1<span data-ref-arg></span>"),
    "Windows quickstart (install.ps1) must take the ref");
  assert.ok(page.includes("./install.sh<span data-ref-arg></span>"),
    "Linux quickstart (./install.sh) must take the ref");
  assert.ok(page.includes(".\\bin\\gpn_node.exe --setup<span data-ref-arg></span>"),
    "Windows manual snippet must take the ref");
  assert.ok(/<pre><code[^>]*>\.\/install\.sh<span data-ref-arg><\/span><\/code><\/pre>/.test(page),
    "from-clone install.sh must take the ref");
});

test("the page shows the code as plain text, not only inside a command", () => {
  const page = read("join.html");
  assert.ok(/data-ref-code/.test(page), "a [data-ref-code] element must show the code itself");
  assert.ok(page.includes("Referral code"), "the code must be labelled for a screenshot");
});

test("with no code the page still explains the network and offers a download", () => {
  const page = read("join.html");
  assert.ok(/href="download"/.test(page), "download must be an opt-in link on the page");
  assert.ok(/GPN/i.test(page), "the page must say what GPN is");
});

/* ---------- malformed codes are named, not silently dropped ---------- */

test("a malformed ?ref= is announced as invalid", () => {
  const page = read("join.html");
  assert.ok(/id="ref-invalid"/.test(page), "join.html must carry an invalid-code notice");
  assert.ok(/not a valid referral code/i.test(page), "the notice must say the code is not valid");
  assert.ok(/data-ref-arg/.test(page), "commands must remain usable when the code is bad");
});

test("rawFromSearch returns the raw value so the page can distinguish absent from invalid", () => {
  assert.strictEqual(R.rawFromSearch("?ref=abc123"), "abc123");
  assert.strictEqual(R.rawFromSearch("?r=abc123"), "abc123");
  assert.strictEqual(R.rawFromSearch("?ref="), "");
  assert.strictEqual(R.rawFromSearch(""), null);
  // And a raw value still has to pass the one validation rule.
  assert.strictEqual(R.validate(R.rawFromSearch('?ref="><script>')), null);
});

test("the page script writes a good code and names a bad one (no markup)", () => {
  const page = read("join.html");
  const m = page.match(/<script>\s*\/\* The landing page's own two jobs[\s\S]*?<\/script>/);
  assert.ok(m, "join.html must carry its referral wiring script");
  const body = m[0].replace(/^<script>/, "").replace(/<\/script>$/, "");

  const run = (search) => {
    const shown = { textContent: null };
    const note = { hidden: true };
    const win = { GPN: { referral: R }, location: { search } };
    const doc = {
      readyState: "complete",
      querySelector: (sel) => (sel === "[data-ref-code]" ? shown : null),
      getElementById: () => note,
      addEventListener: () => {},
    };
    new Function("window", "document", "navigator", body)(win, doc, { clipboard: null });
    return { shown: shown.textContent, invalid: note.hidden === false };
  };

  assert.deepStrictEqual(run("?ref=GPN-ZYCU-4W49-6R"), { shown: "GPN-ZYCU-4W49-6R", invalid: false });
  assert.deepStrictEqual(run("?ref=%3Cscript%3E"), { shown: "invalid code", invalid: true });
  assert.deepStrictEqual(run(""), { shown: "no code in this link", invalid: false });
});

/* ---------- the alias and the canonical path ---------- */

test("/r/CODE 301s to /join?ref=CODE, preserving the code", () => {
  /* The site runs in Cloudflare advanced mode (worker/worker.js in front of the
     asset binding), where `_redirects` is not applied — so the alias has to
     live in the Worker, AND the path has to be in `assets.run_worker_first` or
     the asset layer answers 404 before the Worker is consulted. */
  const src = fs.readFileSync(path.join(SITE, "..", "worker", "worker.js"), "utf8");
  assert.ok(src.includes("/^\\/r\\/([^/]+)$/"), "worker.js must match the /r/<code> route");
  assert.ok(src.includes("/join?ref="), "worker.js must redirect to /join?ref=");
  assert.ok(!fs.existsSync(path.join(SITE, "_redirects")),
    "_redirects is dead config in advanced mode — the Worker owns the route");

  const cfg = fs.readFileSync(path.join(SITE, "..", "wrangler.jsonc"), "utf8");
  const first = cfg.slice(cfg.indexOf("run_worker_first"), cfg.indexOf("]", cfg.indexOf("run_worker_first")));
  assert.ok(first.includes('"/r/*"'), "wrangler run_worker_first must list /r/* or the asset layer 404s");
});

test("sitemap lists /join and /referrals, not a download URL", () => {
  const map = read("sitemap.xml");
  assert.ok(map.includes("https://gpncoin.ai/join"), "sitemap must list /join");
  assert.ok(!/sitemap[^]*download\?ref=/.test(map), "the sitemap must never carry a download?ref URL");
});

/* ---------- no surface asks anyone to share a download URL ---------- */

test("no page or doc tells anyone to share /download?ref=", () => {
  const bad = [];
  const walk = (rel) => {
    const abs = path.join(SITE, rel);
    for (const e of fs.readdirSync(abs, { withFileTypes: true })) {
      const r = path.join(rel, e.name);
      if (e.isDirectory()) {
        if (r === "tests" || r === "assets") continue;
        walk(r);
      } else if (/\.(html|md)$/.test(e.name)) {
        const text = fs.readFileSync(path.join(SITE, r), "utf8");
        if (/download(\.html)?\?ref=/.test(text)) bad.push(r);
      }
    }
  };
  walk(".");
  const rootDoc = fs.readFileSync(path.join(SITE, "..", "docs", "run-a-node.md"), "utf8");
  if (/download(\.html)?\?ref=/.test(rootDoc)) bad.push("docs/run-a-node.md");
  assert.deepStrictEqual(bad, [], "these surfaces still advertise /download?ref= : " + bad.join(", "));
});

test("the canonical share link is the one the Rust binary builds", () => {
  const rs = fs.readFileSync(path.join(SITE, "..", "gpn-agent", "src", "referral.rs"), "utf8");
  assert.ok(rs.includes('SHARE_BASE: &str = "https://gpncoin.ai/join?ref="'),
    "referral.rs SHARE_BASE must be the /join form");
});

console.log("\n" + passed + " tests passed");
