/* Tests for the `?ref=CODE` download-link behaviour.
 *
 * Run with:  node website/tests/referral.test.js
 * (no dependencies, no build — plain node:assert)
 *
 * The rule under test is the XSS boundary: a code comes from the URL, gets
 * interpolated into a copy-paste command, so it must be validated against
 * ^[A-Za-z0-9_-]{4,32}$ and written as TEXT only.
 */
"use strict";

const assert = require("assert");
const fs = require("fs");
const path = require("path");

const R = require("../assets/referral.js");
const HERE = __dirname;
const SITE = path.join(HERE, "..");

let passed = 0;
function test(name, fn) {
  fn();
  passed++;
  console.log("ok - " + name);
}

/* ---------- validation rule ---------- */

test("accepts well-formed codes, including the 4/32 boundaries", () => {
  assert.strictEqual(R.validate("abc123"), "abc123");
  assert.strictEqual(R.validate("abcd"), "abcd");
  assert.strictEqual(R.validate("A-Z_9"), "A-Z_9");
  assert.strictEqual(R.validate("x".repeat(32)), "x".repeat(32));
  assert.strictEqual(R.validate("team-alpha"), "team-alpha");
});

test("rejects wrong lengths", () => {
  assert.strictEqual(R.validate("abc"), null);
  assert.strictEqual(R.validate("x".repeat(33)), null);
  assert.strictEqual(R.validate(""), null);
});

test("rejects anything outside [A-Za-z0-9_-]", () => {
  for (const bad of ["abc def", "abc!", "ab/d", "ab.d", "ab\u00e9d", "abc%20d"]) {
    assert.strictEqual(R.validate(bad), null, bad);
  }
});

test("rejects injection strings outright", () => {
  const attacks = [
    '"><script>alert(1)</script>',
    "<script>alert(1)</script>",
    "abc' onmouseover='x",
    "abc&amp;d",
    "abc`id`",
    "javascript:alert(1)",
    "abc123\nabc",
    "abc123\t",
    " abc123",
    "abc123 ",
  ];
  for (const a of attacks) assert.strictEqual(R.validate(a), null, JSON.stringify(a));
});

/* ---------- URL parsing ---------- */

test("reads ?ref= and ?r=, ignores everything else", () => {
  assert.strictEqual(R.fromSearch("?ref=abc123"), "abc123");
  assert.strictEqual(R.fromSearch("?r=abc123"), "abc123");
  assert.strictEqual(R.fromSearch("?utm=x&ref=team-a&y=1"), "team-a");
  assert.strictEqual(R.fromSearch(""), null);
  assert.strictEqual(R.fromSearch("?ref="), null);
  assert.strictEqual(R.fromSearch("?ref=abc"), null);
  assert.strictEqual(R.fromSearch("?other=abc123"), null);
});

test("a percent-encoded injection never survives parsing", () => {
  assert.strictEqual(R.fromSearch("?ref=%22%3E%3Cscript%3E"), null);
  assert.strictEqual(R.fromSearch("?ref=abc%20def"), null);
  assert.strictEqual(R.fromSearch("?ref=abc%3Cscript%3E"), null);
});

/* ---------- rendered command text ---------- */

test("argFor is exactly what the binary accepts", () => {
  assert.strictEqual(R.argFor("abc123"), " --ref abc123");
  // No code (or a bad one) appends nothing: the command is unchanged.
  assert.strictEqual(R.argFor(""), "");
  assert.strictEqual(R.argFor(null), "");
  assert.strictEqual(R.argFor('"><script>'), "");
});

test("renderInto writes textContent only — never innerHTML", () => {
  let text = null;
  const el = {
    set textContent(v) { text = v; },
    get textContent() { return text; },
  };
  Object.defineProperty(el, "innerHTML", {
    set() { throw new Error("innerHTML must never be used for a referral code"); },
    get() { throw new Error("innerHTML must never be used for a referral code"); },
  });
  assert.strictEqual(R.renderInto(el, "abc123"), " --ref abc123");
  assert.strictEqual(text, " --ref abc123");
  // An injection string is dropped before it can reach the element at all.
  assert.strictEqual(R.renderInto(el, '"><script>'), "");
  assert.strictEqual(text, "");
});

/* ---------- the page glue, statically ---------- */

test("app.js assigns the code with textContent, not innerHTML", () => {
  const src = fs.readFileSync(path.join(SITE, "assets", "app.js"), "utf8");
  const start = src.indexOf("function wireReferral()");
  assert.ok(start > 0, "wireReferral() must exist");
  const end = src.indexOf("function init()", start);
  const body = src.slice(start, end);
  assert.ok(body.includes("textContent"), "must write with textContent");
  assert.ok(!body.includes("innerHTML"), "must not use innerHTML for referral data");
  assert.ok(body.includes("sessionStorage"), "must persist across navigation");
  assert.ok(body.includes('"[data-ref-arg]"'), "must target the placeholder spans");
});

test("the published snippets carry the exact flag order the binary parses", () => {
  // Windows: the referral is captured by the installer, which then starts the
  // node -- so `--ref CODE` has to follow install.ps1, not a gpn_node call.
  const index = fs.readFileSync(path.join(SITE, "index.html"), "utf8");
  assert.ok(
    index.includes("-File C:\\gpn\\bin\\install.ps1<span data-ref-arg></span>"),
    "Windows quickstart must append the ref to install.ps1"
  );
  // Linux: `./install.sh --ref CODE`
  const dl = fs.readFileSync(path.join(SITE, "download.html"), "utf8");
  assert.ok(
    dl.includes("./install.sh<span data-ref-arg></span>"),
    "download page must append the ref to install.sh"
  );
  assert.ok(
    dl.includes(".\\bin\\gpn_node.exe --setup<span data-ref-arg></span>"),
    "download page must carry the Windows node snippet"
  );
  // The placeholder must render empty when there is no code.
  assert.strictEqual(R.argFor(R.validate("")), "");
});

/* ---------- the two installers actually accept what the site advertises ----
 *
 * The site appends " --ref CODE" to a command. If the program that command
 * names rejects that flag, the copy button hands the user a failing command --
 * which is exactly how this check came to exist (index.html moved the Windows
 * quickstart to install.ps1 while the test still asserted the old gpn_node
 * snippet, and install.ps1 had no --ref at all). Assert both installers parse
 * the flag AND record it write-once, so neither can silently drop it again.
 */
test("both installers parse --ref and record the code write-once", () => {
  const ps1 = fs.readFileSync(path.join(SITE, "..", "install.ps1"), "utf8");
  // parses --ref CODE / --ref=CODE (and GPN_REF)
  assert.ok(/\^\-\{1,2\}ref\[=:\]\(\.\+\)\$/.test(ps1), "install.ps1 must parse --ref=CODE");
  assert.ok(/\^\-\{1,2\}ref\$/.test(ps1), "install.ps1 must parse `--ref CODE`");
  assert.ok(ps1.includes("GPN_REF"), "install.ps1 must honour GPN_REF");
  // identical validation rule to referral.js / referral.rs
  assert.ok(ps1.includes("^[A-Za-z0-9_-]+$"), "install.ps1 must use the same code alphabet");
  assert.ok(ps1.includes("too short (need 4-32 chars)") && ps1.includes("too long (need 4-32 chars)"),
    "install.ps1 must enforce the 4-32 length rule");
  // write-once + never fatal: an existing file is kept, a bad code only warns
  assert.ok(/Test-Path \$file/.test(ps1), "install.ps1 must not overwrite an existing code");
  assert.ok(ps1.includes("'referral'"), "install.ps1 must write the node's referral file name");
  assert.ok(ps1.includes("referral code ignored"), "a malformed code must be a warning, not a failure");

  const sh = fs.readFileSync(path.join(SITE, "..", "install.sh"), "utf8");
  assert.ok(sh.includes("--ref)"), "install.sh must parse --ref CODE");
  assert.ok(sh.includes("--ref=*)"), "install.sh must parse --ref=CODE");
});

test("both pages load the referral helper before app.js", () => {
  for (const page of ["index.html", "download.html"]) {
    const html = fs.readFileSync(path.join(SITE, page), "utf8");
    const helper = html.indexOf('src="assets/referral.js"');
    const app = html.indexOf('src="assets/app.js"');
    assert.ok(helper > 0 && app > helper, page + " must load referral.js before app.js");
  }
});

console.log("\n" + passed + " tests passed");
