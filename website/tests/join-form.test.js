// Static checks on the join form: one success message, one failure address.
// Run: node --test website/tests/join-form.test.js
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join as joinPath } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const SITE = joinPath(HERE, "..");
const APP = readFileSync(joinPath(SITE, "assets", "app.js"), "utf8");
const INDEX = readFileSync(joinPath(SITE, "index.html"), "utf8");

test("the success copy is exactly what the applicant sees today", () => {
  const copy = "'<span class=\"ok\">Application received \\u2014 we\\'ll be in touch.</span>'";
  assert.ok(APP.includes(copy), "success copy must read “Application received — we'll be in touch.”");
});

test("the fallback address is crewlyllc@gmail.com and nothing else", () => {
  assert.match(APP, /var FALLBACK_EMAIL = "crewlyllc@gmail\.com";/);
  assert.equal(APP.match(/mailto:/g).length, 1, "exactly one mailto, in the fallback path");
  assert.match(APP, /var mail = "mailto:" \+ FALLBACK_EMAIL/);
  assert.ok(!APP.includes("hello@gpncoin.ai"), "the old mailbox must be gone");
});

test("the mailto draft is only rendered by the failure path", () => {
  const fallback = APP.indexOf("function renderFallback(");
  assert.ok(fallback > 0, "renderFallback must exist");
  const successIdx = APP.indexOf("Application received");
  assert.ok(successIdx > 0 && successIdx < fallback, "success renders before the fallback helper");
  const successBlock = APP.slice(successIdx, fallback);
  assert.ok(!successBlock.includes("FALLBACK_EMAIL"), "success must not reference the email");
  assert.ok(!successBlock.includes("mailto:"), "success must not render a mailto link");
});

test("the unavailable message matches the Worker's copy", () => {
  assert.ok(APP.includes("Applications are temporarily unavailable \\u2014 try again in a minute."));
});

test("the page has no stale mailbox and its static contact link is crewlyllc", () => {
  assert.ok(!INDEX.includes("hello@gpncoin.ai"));
  assert.match(INDEX, /href="mailto:crewlyllc@gmail\.com\?subject=GPN%20application"/);
  assert.match(INDEX, /id="join-form"/);
  assert.match(INDEX, /name="website"/, "honeypot must stay on the form");
});
