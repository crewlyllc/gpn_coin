// Static checks on the /nodes live directory page.
// Run: node --test website/tests/nodes-directory.test.js
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const SITE = join(HERE, "..");
const PAGE = readFileSync(join(SITE, "nodes.html"), "utf8");
const NETWORK = JSON.parse(readFileSync(join(SITE, "..", "network.json"), "utf8"));

test("the headline counts container exists with the three numbers", () => {
  assert.match(PAGE, /id="summary"/);
  for (const id of ["c-live", "c-serving", "c-offline"]) {
    assert.match(PAGE, new RegExp(`id="${id}"`));
  }
  assert.match(PAGE, /Peers online/);
  assert.match(PAGE, /Serving AI/);
  assert.match(PAGE, /Offline/);
});

test("the table renders peer id, status, VRAM, models, address and last seen", () => {
  for (const col of ["Peer ID", "Status", "VRAM", "Models", "Address", "Last seen"]) {
    assert.match(PAGE, new RegExp(`<th scope="col">${col}</th>`));
  }
  assert.match(PAGE, /id="nodes-body"/);
});

test("live data comes from /api/network, not from the seed file", () => {
  assert.match(PAGE, /fetch\("\/api\/network"/);
  // The only /seeds.json references are documentation/links, never the fetch.
  assert.ok(!/fetch\(["']\/seeds\.json/.test(PAGE), "the page must not render from the seed list");
});

test("the cached fallback is clearly labelled and never presented as live", () => {
  assert.match(PAGE, /cached — live data unavailable/);
  assert.match(PAGE, /function showCached\(/);
  // The fallback rows are badge-cached, and the counts are withheld.
  assert.match(PAGE, /statusBadge\("offline", true\)/);
  assert.match(PAGE, /setText\(\$\("c-live"\), "\?"\)/);
  assert.ok(
    !/CACHED_BOOTNODES[\s\S]{0,400}statusBadge\("serving"/.test(PAGE),
    "cached rows must never be labelled serving",
  );
});

test("offline rows are visually distinct", () => {
  assert.match(PAGE, /tr\.className = "offline-row"/);
  assert.match(PAGE, /\.dir-table tr\.offline-row td \{ opacity: \.6; \}/);
});

test("the baked cached list matches network.json's bootnodes", () => {
  const block = PAGE.slice(PAGE.indexOf("var CACHED_BOOTNODES"), PAGE.indexOf("];", PAGE.indexOf("var CACHED_BOOTNODES")));
  for (const ma of NETWORK.bootnodes) {
    assert.ok(block.includes(ma), `cached list must contain ${ma}`);
  }
});

test("the page dedupes rows by peer id and knows the three statuses", () => {
  assert.match(PAGE, /byPeer\.get/);
  assert.match(PAGE, /"peer-only"/);
  assert.match(PAGE, /"serving"/);
  assert.match(PAGE, /"offline"/);
});
