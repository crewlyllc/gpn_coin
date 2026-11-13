// /access-points bootnode table (queue item #114).
// Run: node --test website/tests/access-points.test.js
//
// The page published hand-copied multiaddrs and they went stale: 30334 +
// WRWv4… for a node that had moved to 30333 + WAFt…, and MXU21… for one that
// now answers as FqZ2…. A reader who copied a row could not join the network.
// The table is now generated from /seeds.json at page load, and these checks
// make drift a build failure:
//   * every multiaddr published on the page must be in the live seed list
//     (network.json `bootnodes` — the same list the Worker and the node ship),
//     with the right port and the right /p2p/ id;
//   * every live seed must appear, so the page cannot omit a dialable node;
//   * the static fallback rows and /nodes' baked list must match that list;
//   * the renderer must keep the honest statuses and never treat fetched text
//     as markup.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import vm from "node:vm";

const HERE = dirname(fileURLToPath(import.meta.url));
const SITE = join(HERE, "..");
const PAGE = readFileSync(join(SITE, "access-points.html"), "utf8");
const NODES = readFileSync(join(SITE, "nodes.html"), "utf8");
const NETWORK = JSON.parse(readFileSync(join(SITE, "..", "network.json"), "utf8"));

const CANONICAL = NETWORK.bootnodes;

/** Every libp2p multiaddr that appears anywhere in a page. */
function multiaddrsIn(html) {
  const re = /\/(?:dns4|dns6|ip4|ip6)\/[^/\s"'<>]+(?:\/udp\/\d+\/quic-v1|\/tcp\/\d+(?:\/ws|\/wss)?)\/p2p\/[A-Za-z0-9]+/g;
  return [...new Set(html.match(re) || [])];
}

function peerAndPort(ma) {
  const m = /^\/[^/]+\/[^/]+\/(?:tcp|udp)\/(\d+)(?:\/(?:ws|wss))?\/p2p\/([A-Za-z0-9]+)$/.exec(ma);
  return m ? { port: m[1], peer: m[2] } : null;
}

test("every multiaddr published on /access-points is in the live seed list", () => {
  const published = multiaddrsIn(PAGE);
  // The guard used to be `>= 3`, a magic number that encoded "three endpoints
  // on two machines". It is now tied to the registry: the seed list is the
  // single source of truth, and a page that publishes a different number of
  // endpoints than the registry holds has either lost the table or invented a
  // row. Tying it to CANONICAL keeps this check honest when the registry is
  // legitimately trimmed (seed-2 was dropped 2026-11-13) instead of turning a
  // correct registry into a permanent build failure.
  assert.equal(
    published.length,
    CANONICAL.length,
    `expected ${CANONICAL.length} bootnode endpoints (network.json), found ${published.length}`,
  );
  for (const ma of published) {
    assert.ok(
      CANONICAL.includes(ma),
      `stale multiaddr on /access-points: ${ma}\n  live seed list has: ${CANONICAL.join(", ")}`,
    );
  }
});

test("every live seed is published, so no dialable node is missing from the page", () => {
  for (const ma of CANONICAL) {
    assert.ok(PAGE.includes(ma), `/access-points does not publish ${ma}`);
  }
});

test("published ports and peer ids match the live list exactly", () => {
  for (const ma of multiaddrsIn(PAGE)) {
    const mine = peerAndPort(ma);
    const ref = CANONICAL.map(peerAndPort).find((x) => x && x.peer === mine.peer);
    assert.ok(ref, `peer ${mine.peer} is not in network.json`);
    assert.equal(mine.port, ref.port, `port ${mine.port} for ${mine.peer} disagrees with the seed list`);
  }
});

test("no retired identity is published as a dial target", () => {
  // The two ids that made the old table unusable. They may only appear in prose
  // (the "retired … identity is gone" note), never inside a multiaddr.
  for (const dead of ["12D3KooWRWv4", "12D3KooWMXU21"]) {
    assert.ok(!multiaddrsIn(PAGE).some((ma) => ma.includes(dead)), `retired identity ${dead} is published`);
  }
});

test("/nodes' baked fallback list also matches network.json", () => {
  const start = NODES.indexOf("var CACHED_BOOTNODES");
  assert.ok(start > -1, "CACHED_BOOTNODES must exist");
  const block = NODES.slice(start, NODES.indexOf("];", start));
  for (const ma of multiaddrsIn(block)) {
    assert.ok(CANONICAL.includes(ma), `stale multiaddr in /nodes' cached list: ${ma}`);
  }
  for (const ma of CANONICAL) {
    assert.ok(block.includes(ma), `/nodes' cached list is missing ${ma}`);
  }
});

test("the table is generated from /seeds.json, not from a copied snapshot", () => {
  assert.match(PAGE, /fetch\("\/seeds\.json"/);
  assert.match(PAGE, /id="bootnode-rows"/);
  assert.match(PAGE, /id="bootnode-source"/);
  // Fetched values must be set as text, never parsed as markup.
  assert.ok(!/innerHTML\s*=\s*(?!["']{2})/.test(PAGE.slice(PAGE.indexOf("id=\"bootnode-rows\""))),
    "the renderer must not assign fetched data to innerHTML");
});

test("a failed fetch is labelled as the last-known list, not as live", () => {
  assert.match(PAGE, /Live seed list unavailable/);
  assert.match(PAGE, /last-known list/);
});

test("liveness is three-valued: Live / Offline / Unreported", () => {
  assert.match(PAGE, /live === true \? " Live" : live === false \? " Offline" : " Unreported"/);
});

/* ── the multiaddr describer, run for real out of the page ──────────────── */
function loadDescribe() {
  const script = PAGE.slice(PAGE.indexOf("(function () {\n  \"use strict\";"));
  const start = script.indexOf("function describe(ma) {");
  const end = script.indexOf("\n  }", start) + 4;
  const src = script.slice(start, end);
  return vm.runInNewContext(`${src}; describe;`, {});
}

test("describe() parses the shapes the seed list actually uses", () => {
  const describe = loadDescribe();
  assert.deepEqual(
    { ...describe("/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo") },
    { host: "gpn-testnet.duckdns.org:30333", port: "30333", peer: "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo", kind: "DNS name" },
  );
  assert.deepEqual(
    { ...describe("/ip4/142.129.54.88/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo") },
    { host: "142.129.54.88:30333", port: "30333", peer: "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo", kind: "IP literal" },
  );
  for (const ma of CANONICAL) assert.ok(describe(ma), `cannot describe live multiaddr ${ma}`);
  // 2026-11-13: full nodes listen with websocket framing, so the seed list
  // carries /ws. The describer must render it, not reject it as non-libp2p.
  assert.deepEqual(
    { ...describe("/ip4/142.129.54.88/tcp/30333/ws/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo") },
    { host: "142.129.54.88:30333", port: "30333", peer: "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo", kind: "IP literal" },
  );
  assert.equal(describe("/ip4/1.2.3.4/tcp/30333"), null, "a non-libp2p address must not be published as a node");
});
