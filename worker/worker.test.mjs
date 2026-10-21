// Unit tests for the gpncoin.ai peer-directory Worker.
// Run: node --test worker/worker.test.mjs
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import crypto from "node:crypto";

import worker, { __test } from "./worker.js";

const here = dirname(fileURLToPath(import.meta.url));
const network = JSON.parse(readFileSync(join(here, "..", "network.json"), "utf8"));

test("compiled-in seed list matches network.json", () => {
  assert.deepEqual(__test.STATIC_BOOTNODES, network.bootnodes);
  for (const ma of __test.STATIC_BOOTNODES) {
    assert.match(ma, /\/p2p\/12D3Koo/);
  }
});

test("multiaddrFromUrl handles the shapes the AP emits", () => {
  assert.equal(
    __test.multiaddrFromUrl("http://203.0.113.7:8093/status"),
    "/ip4/203.0.113.7/tcp/8093",
  );
  assert.equal(
    __test.multiaddrFromUrl("https://node.example.org:30333"),
    "/dns4/node.example.org/tcp/30333",
  );
  assert.equal(__test.multiaddrFromUrl("https://[2001:db8::1]:30333"), "/ip6/2001:db8::1/tcp/30333");
  assert.equal(
    __test.multiaddrFromUrl("/dns4/a.example/tcp/30333/p2p/12D3KooWabc"),
    "/dns4/a.example/tcp/30333/p2p/12D3KooWabc",
  );
  assert.equal(__test.multiaddrFromUrl("http://host-without-port/x"), null);
  assert.equal(__test.multiaddrFromUrl(""), null);
});

test("directory is static-only when the AP is unreachable", () => {
  const body = __test.buildDirectory(null);
  assert.equal(body.source, "static-fallback");
  assert.deepEqual(body.multiaddrs, network.bootnodes);
  assert.equal(body.genesis, network.genesis_hash);
  assert.equal(body.chain_id, network.chain_id);
  assert.equal(body.ttl, 60);
  assert.ok(body.updated_at);
  for (const n of body.nodes) assert.equal(n.role, "seed");
});

test("healthy providers are listed; unhealthy ones are dropped", () => {
  const providers = [
    { status_url: "http://203.0.113.7:8093/status", healthy: true, address: "5Abc" },
    { status_url: "http://198.51.100.9:8093/status", healthy: false, address: "5Def" },
  ];
  const body = __test.buildDirectory(providers);
  assert.equal(body.live_providers, 1);
  const live = body.nodes.filter((n) => n.role === "provider");
  assert.equal(live.length, 1);
  assert.equal(live[0].endpoint, "http://203.0.113.7:8093/status");
  // Provider endpoints carry no peer id, so they must not enter `multiaddrs`.
  assert.deepEqual(body.multiaddrs, network.bootnodes);
});

test("a live entry with a /p2p/ id becomes dialable", () => {
  const ma = "/ip4/198.51.100.9/tcp/30333/p2p/12D3KooW9ye8kXwMYT4vBJoJdCkM9VfRrcM7nGgqVDPq3Fy8i6xw";
  const body = __test.buildDirectory([{ status_url: ma, healthy: true }]);
  assert.ok(body.multiaddrs.includes(ma));
  assert.equal(body.nodes.find((n) => n.multiaddr === ma).peer_id, "12D3KooW9ye8kXwMYT4vBJoJdCkM9VfRrcM7nGgqVDPq3Fy8i6xw");
});

test("a provider that publishes a peer id becomes dialable", () => {
  const pid = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
  const body = __test.buildDirectory([
    {
      status_url: "http://203.0.113.7:8093/status",
      healthy: true,
      address: "5Abc",
      peer_id: pid,
      p2p_port: 30333,
      multiaddrs: [`/ip4/203.0.113.7/tcp/30333/p2p/${pid}`],
    },
  ]);
  const ma = `/ip4/203.0.113.7/tcp/30333/p2p/${pid}`;
  assert.ok(body.multiaddrs.includes(ma), "dialable multiaddr is published");
  assert.ok(!body.multiaddrs.includes("/ip4/203.0.113.7/tcp/8093"));
  const node = body.nodes.find((n) => n.multiaddr === ma);
  assert.equal(node.role, "provider");
  assert.equal(node.peer_id, pid);
  assert.equal(node.endpoint, "http://203.0.113.7:8093/status");
  assert.equal(body.live_providers, 1);
});

test("a NATed provider synthesises a dialable address from the host the AP reached", () => {
  // Listening on 0.0.0.0, so the heartbeat can only publish the port.
  // A fresh peer id (not one already published by a static seed).
  const pid = "12D3KooWQ8fU4oXcZf3YhU8yTt4cY5q1mS2n8HkWq7jB6xU4aB3c";
  const body = __test.buildDirectory([
    { status_url: "http://203.0.113.7:8093/status", healthy: true, peer_id: pid, p2p_port: 30333, multiaddrs: [] },
  ]);
  assert.deepEqual(body.multiaddrs.slice(-1), [`/ip4/203.0.113.7/tcp/30333/p2p/${pid}`]);
});

test("a peer id already published elsewhere is not re-synthesised at another host", () => {
  // The 2026-10-20 directory bug: box #1's status_url host (23.241.109.255)
  // was published paired with the Linux node's peer id, which is already
  // published at its own duckdns host. A peer id is one node.
  const linuxPid = __test.peerIdOf(network.bootnodes[0]);
  const body = __test.buildDirectory([
    {
      status_url: "http://23.241.109.255:8093/status",
      healthy: true,
      peer_id: linuxPid,
      p2p_port: 30333,
      multiaddrs: [],
    },
  ]);
  assert.ok(
    !body.multiaddrs.includes(`/ip4/23.241.109.255/tcp/30333/p2p/${linuxPid}`),
    "must not pair another box's host with an already-published peer id",
  );
  assert.ok(!body.multiaddrs.some((m) => m.startsWith("/ip4/23.241.109.255")), "nothing at that host");
  // The endpoint is still visible for monitoring.
  const entry = body.nodes.find((n) => n.endpoint === "http://23.241.109.255:8093/status");
  assert.ok(entry, "the provider endpoint is still listed");
  assert.equal(entry.multiaddr, "/ip4/23.241.109.255/tcp/8093");
});

test("two providers cannot publish the same peer id at two hosts", () => {
  const pid = "12D3KooWQ8fU4oXcZf3YhU8yTt4cY5q1mS2n8HkWq7jB6xU4aB3c";
  const body = __test.buildDirectory([
    // The first record self-reports where it really is.
    {
      status_url: "http://203.0.113.7:8093/status",
      healthy: true,
      peer_id: pid,
      p2p_port: 30333,
      multiaddrs: [`/ip4/198.51.100.9/tcp/30333/p2p/${pid}`],
    },
    // A second record claims the same id at a different host.
    { status_url: "http://203.0.113.8:8093/status", healthy: true, peer_id: pid, p2p_port: 30333, multiaddrs: [] },
  ]);
  const withPid = body.multiaddrs.filter((m) => m.endsWith(`/p2p/${pid}`));
  assert.equal(withPid.length, 1, "one node, one address: " + JSON.stringify(withPid));
  assert.equal(withPid[0], `/ip4/198.51.100.9/tcp/30333/p2p/${pid}`);
});

test("an endpoint owned by one peer id is not re-used for another", () => {
  const pidA = "12D3KooWQ8fU4oXcZf3YhU8yTt4cY5q1mS2n8HkWq7jB6xU4aB3c";
  const pidB = "12D3KooWRWv4yVdJGiVg7xRgdUJY9G57EeutB596gWVpAwGC9qQ3";
  const body = __test.buildDirectory([
    {
      status_url: "http://203.0.113.7:8093/status",
      healthy: true,
      peer_id: pidA,
      multiaddrs: [`/ip4/203.0.113.7/tcp/30333/p2p/${pidA}`],
    },
    // Different node, same host:port, no explicit address of its own.
    { status_url: "http://203.0.113.7:8093/status", healthy: true, peer_id: pidB, p2p_port: 30333, multiaddrs: [] },
  ]);
  assert.ok(body.multiaddrs.includes(`/ip4/203.0.113.7/tcp/30333/p2p/${pidA}`));
  assert.ok(
    !body.multiaddrs.includes(`/ip4/203.0.113.7/tcp/30333/p2p/${pidB}`),
    "the endpoint already belongs to peer A",
  );
});

test("a provider without a peer id stays informational", () => {
  const body = __test.buildDirectory([
    { status_url: "http://203.0.113.7:8093/status", healthy: true, address: "5Abc" },
  ]);
  assert.deepEqual(body.multiaddrs, network.bootnodes);
  const entry = body.nodes.find((n) => n.role === "provider");
  assert.equal(entry.endpoint, "http://203.0.113.7:8093/status");
  assert.equal(entry.peer_id, null);
});

test("a provider multiaddr cannot claim another peer id", () => {
  const pid = "12D3KooWQ8fU4oXcZf3YhU8yTt4cY5q1mS2n8HkWq7jB6xU4aB3c";
  const body = __test.buildDirectory([
    {
      status_url: "http://203.0.113.7:8093/status",
      healthy: true,
      peer_id: pid,
      p2p_port: 30333,
      multiaddrs: ["/ip4/198.51.100.9/tcp/30333/p2p/12D3KooW9ye8kXwMYT4vBJoJdCkM9VfRrcM7nGgqVDPq3Fy8i6xw"],
    },
  ]);
  // The mismatched address is dropped, and the legitimate fallback is used.
  assert.ok(!body.multiaddrs.includes("/ip4/198.51.100.9/tcp/30333/p2p/12D3KooW9ye8kXwMYT4vBJoJdCkM9VfRrcM7nGgqVDPq3Fy8i6xw"));
  assert.ok(body.multiaddrs.includes(`/ip4/203.0.113.7/tcp/30333/p2p/${pid}`));
});

test("a provider already in the static seeds is deduplicated", () => {
  const staticMa = network.bootnodes[0];
  const pid = __test.peerIdOf(staticMa);
  const body = __test.buildDirectory([
    { status_url: "http://203.0.113.7:8093/status", healthy: true, peer_id: pid, multiaddrs: [staticMa] },
  ]);
  assert.equal(body.multiaddrs.filter((m) => m === staticMa).length, 1);
  assert.equal(body.nodes.filter((n) => n.multiaddr === staticMa).length, 1);
  assert.equal(body.live_providers, 1);
});

test("multiaddr / peer-id sanitising rejects junk", () => {
  const pid = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
  assert.equal(__test.sanitizePeerId(pid), pid);
  assert.equal(__test.sanitizePeerId("../../etc/passwd"), null);
  assert.equal(__test.sanitizePeerId(42), null);
  assert.ok(__test.sanitizeMultiaddr(`/ip4/1.2.3.4/tcp/30333/p2p/${pid}`));
  assert.ok(__test.sanitizeMultiaddr(`/dns4/node.example/tcp/30333/p2p/${pid}`));
  assert.equal(__test.sanitizeMultiaddr("/ip4/1.2.3.4/tcp/99999/p2p/12D3KooWAFt"), null);
  assert.equal(__test.sanitizeMultiaddr("/ip4/1.2.3.4/tcp/30333"), null);
  assert.equal(__test.sanitizeMultiaddr(`/ip4/1.2.3.4/tcp/30333/p2p/${pid}/http/evil`), null);
  assert.equal(__test.sanitizeMultiaddr("http://1.2.3.4:30333"), null);
});

test("GET /seeds.json is 200 JSON even with no AP configured", async () => {
  const res = await worker.fetch(new Request("https://gpncoin.ai/seeds.json"), { AP_URL: "" }, { waitUntil() {} });
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("access-control-allow-origin"), "*");
  assert.match(res.headers.get("cache-control"), /max-age=60/);
  const body = await res.json();
  assert.ok(Array.isArray(body.multiaddrs));
  assert.ok(body.multiaddrs.length >= 1);
});

test("GET /bootnodes.txt is the same list, one per line", async () => {
  const res = await worker.fetch(new Request("https://gpncoin.ai/bootnodes.txt"), { AP_URL: "" }, { waitUntil() {} });
  assert.equal(res.status, 200);
  const text = await res.text();
  assert.deepEqual(text.trim().split("\n"), network.bootnodes);
});

test("HTTP parsing: content-length and chunked", () => {
  const enc = new TextEncoder();
  const cl = enc.encode('HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\n{"a":1}');
  assert.equal(__test.parseHttpBody(cl), '{"a":1}');
  const chunked = enc.encode('HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{"a"\r\n3\r\n:1}\r\n0\r\n\r\n');
  assert.equal(__test.parseHttpBody(chunked), '{"a":1}');
  assert.equal(__test.parseHttpBody(enc.encode("HTTP/1.1 500 Nope\r\n\r\n")), null);
  assert.equal(__test.parseHttpBody(enc.encode("garbage")), null);
});

test("other paths fall through to the asset binding", async () => {
  let seen = null;
  const env = { ASSETS: { fetch: async (r) => (seen = r, new Response("site", { status: 200 })) } };
  const res = await worker.fetch(new Request("https://gpncoin.ai/docs/index.html"), env, { waitUntil() {} });
  assert.equal(res.status, 200);
  assert.equal(await res.text(), "site");
  assert.equal(new URL(seen.url).pathname, "/docs/index.html");
});

// ─────────────────────────────────────────────────────────────────────────────
// Signed release manifest (/releases.json) — see docs/updates.md
// ─────────────────────────────────────────────────────────────────────────────

const RELEASES = join(here, "..", "website", "releases.json");

/** The canonical text a node signs/verifies. Must match
 *  gpn-agent/src/update.rs::ReleaseManifest::canonical_text. */
function canonicalText(man) {
  const assets = [...(man.assets || [])].sort((a, b) =>
    a.os === b.os
      ? a.arch === b.arch
        ? a.url < b.url
          ? -1
          : a.url > b.url
            ? 1
            : 0
        : a.arch < b.arch
          ? -1
          : 1
      : a.os < b.os
        ? -1
        : 1,
  );
  const lines = [
    "gpn-release-v1",
    `version=${man.version}`,
    `min_version=${man.min_version || ""}`,
    `published_at=${man.published_at || ""}`,
    `signing_key=${(man.signing_key || "").toLowerCase()}`,
  ];
  for (const a of assets) {
    lines.push(
      `asset=${a.os}/${a.arch} ${a.url} ${a.sha256.toLowerCase()} ${Number(a.size || 0)}`,
    );
  }
  return Buffer.from(lines.join("\n") + "\n", "utf8");
}

/** Wrap a raw 32-byte ed25519 public key as SPKI DER so node:crypto accepts it. */
function ed25519PublicKey(rawHex) {
  const der = Buffer.concat([
    Buffer.from("302a300506032b6570032100", "hex"),
    Buffer.from(rawHex, "hex"),
  ]);
  return crypto.createPublicKey({ key: der, format: "der", type: "spki" });
}

test("the published manifest verifies against the compiled-in release key", () => {
  let man;
  try {
    man = JSON.parse(readFileSync(RELEASES, "utf8"));
  } catch {
    return; // no manifest generated in this checkout
  }
  // The same constant the node ships with (gpn-agent/src/update.rs).
  const embedded = readFileSync(join(here, "..", "gpn-agent", "src", "update.rs"), "utf8")
    .match(/RELEASE_PUBKEY_HEX: &str = "([0-9a-f]{64})"/)[1];
  assert.equal(man.signing_key.toLowerCase(), embedded, "manifest names the embedded key");
  assert.ok(man.signature, "manifest is signed");
  assert.match(man.version, /^\d+\.\d+\.\d+/, "version is M.m.p");
  assert.match(man.min_version, /^\d+\.\d+\.\d+/, "min_version is M.m.p");

  const ok = crypto.verify(
    null,
    canonicalText(man),
    ed25519PublicKey(embedded),
    Buffer.from(man.signature, "hex"),
  );
  assert.ok(ok, "ed25519 signature must verify over the canonical text");
});

test("a tampered manifest does not verify", () => {
  let man;
  try {
    man = JSON.parse(readFileSync(RELEASES, "utf8"));
  } catch {
    return;
  }
  const key = ed25519PublicKey(man.signing_key);
  const sig = Buffer.from(man.signature, "hex");

  for (const mutate of [
    (m) => (m.version = "9.9.9"),
    (m) => (m.min_version = "9.9.9"),
    (m) =>
      m.assets.push({ os: "linux", arch: "x86_64", url: "https://evil/x", sha256: "00", size: 1 }),
  ]) {
    const copy = JSON.parse(JSON.stringify(man));
    mutate(copy);
    assert.equal(
      crypto.verify(null, canonicalText(copy), key, sig),
      false,
      "a modified manifest must not verify",
    );
  }
});

test("GET /releases.json serves the manifest with updater-friendly headers", async () => {
  const body = '{"version":"0.2.0"}';
  const env = {
    ASSETS: {
      fetch: async (r) => {
        assert.equal(new URL(r.url).pathname, "/releases.json");
        return new Response(body, { status: 200, headers: { "content-type": "text/html" } });
      },
    },
  };
  const res = await worker.fetch(new Request("https://gpncoin.ai/releases.json"), env, {
    waitUntil() {},
  });
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "application/json; charset=utf-8");
  assert.equal(res.headers.get("cache-control"), "public, max-age=300");
  assert.equal(res.headers.get("access-control-allow-origin"), "*");
  assert.equal(await res.text(), body);
});

test("GET /releases.json is a JSON 503 (never an HTML 404) when unpublished", async () => {
  const env = { ASSETS: { fetch: async () => new Response("nope", { status: 404 }) } };
  const res = await worker.fetch(new Request("https://gpncoin.ai/releases.json"), env, {
    waitUntil() {},
  });
  assert.equal(res.status, 503);
  assert.equal(res.headers.get("content-type"), "application/json; charset=utf-8");
  assert.equal(res.headers.get("cache-control"), "no-store");
  const body = await res.json();
  assert.equal(body.error, "release manifest not published");

  // No ASSETS binding at all behaves the same way.
  const bare = await worker.fetch(new Request("https://gpncoin.ai/releases.json"), {}, { waitUntil() {} });
  assert.equal(bare.status, 503);
});

test("POST /releases.json is rejected", async () => {
  const res = await worker.fetch(
    new Request("https://gpncoin.ai/releases.json", { method: "POST" }),
    {},
    { waitUntil() {} },
  );
  assert.equal(res.status, 405);
});

test("HEAD /releases.json reports success with no body", async () => {
  const env = { ASSETS: { fetch: async () => new Response('{"version":"0.2.0"}', { status: 200 }) } };
  const res = await worker.fetch(
    new Request("https://gpncoin.ai/releases.json", { method: "HEAD" }),
    env,
    { waitUntil() {} },
  );
  assert.equal(res.status, 200);
  assert.equal(res.body, null);
  assert.equal(res.headers.get("content-type"), "application/json; charset=utf-8");
});
