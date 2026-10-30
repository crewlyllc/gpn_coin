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
  // A peer id that is not already a compiled-in seed (those win the row).
  const pid = "12D3KooWQ8fU4oXcZf3YhU8yTt4cY5q1mS2n8HkWq7jB6xU4aB3c";
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
      status_url: "http://198.51.100.44:8093/status",
      healthy: true,
      peer_id: linuxPid,
      p2p_port: 30333,
      multiaddrs: [],
    },
  ]);
  assert.ok(
    !body.multiaddrs.includes(`/ip4/198.51.100.44/tcp/30333/p2p/${linuxPid}`),
    "must not pair another box's host with an already-published peer id",
  );
  assert.ok(!body.multiaddrs.some((m) => m.startsWith("/ip4/198.51.100.44")), "nothing at that host");
  // The endpoint is still visible for monitoring.
  const entry = body.nodes.find((n) => n.endpoint === "http://198.51.100.44:8093/status");
  assert.ok(entry, "the provider endpoint is still listed");
  assert.equal(entry.multiaddr, "/ip4/198.51.100.44/tcp/8093");
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
    // The constant's value may be wrapped onto the next line (rustfmt), so
    // allow whitespace before the opening quote.
    .match(/RELEASE_PUBKEY_HEX: &str =\s*"([0-9a-f]{64})"/)[1];
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

/* ───────────────────────── /api/join ───────────────────────── */

/**
 * The AP transport seam (`env.__test_ap_transport`): records each call and
 * answers with whatever the test wants. No sockets, no network.
 */
function mockAp(reply) {
  const calls = [];
  return {
    calls,
    env: {
      async __test_ap_transport(req) {
        calls.push(req);
        return typeof reply === "function" ? reply(req) : reply;
      },
    },
  };
}

function apStored(extra = {}) {
  return {
    status: 200,
    body: JSON.stringify({ ok: true, stored: true, delivered: true, reference: "abc123", ...extra }),
  };
}

function joinRequest(payload, { ip = "203.0.113.9", headers = {} } = {}) {
  return new Request("https://gpncoin.ai/api/join", {
    method: "POST",
    headers: { "content-type": "application/json", "cf-connecting-ip": ip, ...headers },
    body: typeof payload === "string" ? payload : JSON.stringify(payload),
  });
}

const GOOD_APP = {
  name: "Ada Lovelace",
  email: "ada@example.com",
  role: "GPU / CUDA engineering",
  message: "I wrote a memory-hard kernel once.",
};

test("/api/join proxies a valid application to the AP's /join", async () => {
  const ap = mockAp(apStored());
  const res = await worker.fetch(joinRequest(GOOD_APP), ap.env, { waitUntil() {} });
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "application/json; charset=utf-8");
  const body = await res.json();
  assert.deepEqual(body, {
    ok: true,
    stored: true,
    delivered: true,
    reference: "abc123",
  });

  assert.equal(ap.calls.length, 1);
  assert.equal(ap.calls[0].method, "POST");
  assert.equal(ap.calls[0].path, "/join");
  const sent = JSON.parse(ap.calls[0].body);
  assert.deepEqual(sent, GOOD_APP);
});

test("/api/join reports delivered:false when the AP stored but could not deliver", async () => {
  const ap = mockAp({
    status: 200,
    body: JSON.stringify({ ok: true, stored: true, delivered: false, reference: "r1" }),
  });
  const body = await (
    await worker.fetch(joinRequest(GOOD_APP), ap.env, { waitUntil() {} })
  ).json();
  assert.equal(body.ok, true);
  assert.equal(body.stored, true);
  assert.equal(body.delivered, false);
});

test("/api/join: unreachable AP → 200 JSON with the mailto fallback", async () => {
  const ap = mockAp({ error: "connection refused" });
  const res = await worker.fetch(joinRequest(GOOD_APP, { ip: "198.51.100.7" }), ap.env, {
    waitUntil() {},
  });
  assert.equal(res.status, 200);
  const body = await res.json();
  assert.equal(body.ok, false);
  assert.equal(body.stored, false);
  assert.equal(body.error, "applications are temporarily unavailable");
  assert.equal(body.fallback, "email");
  assert.equal(body.email, "crewlyllc@gmail.com");
});

test("/api/join: AP 5xx or non-JSON → same unavailable shape", async () => {
  for (const reply of [
    { status: 500, body: "<html>oops</html>" },
    { status: 200, body: "not json" },
    { status: 200, body: JSON.stringify({ ok: false, error: "boom" }) },
  ]) {
    const ap = mockAp(reply);
    const body = await (
      await worker.fetch(joinRequest(GOOD_APP, { ip: "198.51.100.8" }), ap.env, { waitUntil() {} })
    ).json();
    assert.equal(body.ok, false);
    assert.equal(body.error, "applications are temporarily unavailable");
    assert.equal(body.email, "crewlyllc@gmail.com");
  }
});

test("/api/join validates fields locally and never calls the AP", async () => {
  const ap = mockAp(apStored());
  const cases = [
    [{ ...GOOD_APP, name: "" }, /name/i],
    [{ ...GOOD_APP, name: "x".repeat(81) }, /too long/i],
    [{ ...GOOD_APP, email: "not-an-email" }, /email/i],
    [{ ...GOOD_APP, email: "a@b.co", message: "x".repeat(4001) }, /4000/],
  ];
  for (const [payload, re] of cases) {
    const res = await worker.fetch(joinRequest(payload, { ip: "203.0.113.11" }), ap.env, {
      waitUntil() {},
    });
    assert.equal(res.status, 200);
    const body = await res.json();
    assert.equal(body.ok, false, JSON.stringify(payload).slice(0, 60));
    assert.match(body.error, re);
  }
  assert.equal(ap.calls.length, 0);

  // Malformed JSON and oversized bodies are also 200 JSON.
  const bad = await worker.fetch(joinRequest("{not json", { ip: "203.0.113.12" }), ap.env, {
    waitUntil() {},
  });
  assert.equal(bad.status, 200);
  assert.equal((await bad.json()).ok, false);

  const huge = await worker.fetch(
    joinRequest("x".repeat(9000), { ip: "203.0.113.13" }),
    ap.env,
    { waitUntil() {} },
  );
  assert.equal(huge.status, 200);
  assert.equal((await huge.json()).ok, false);
  assert.equal(ap.calls.length, 0);
});

test("/api/join honeypot is silently ignored (nothing sent)", async () => {
  const ap = mockAp(apStored());
  const res = await worker.fetch(
    joinRequest({ ...GOOD_APP, website: "http://spam.example" }, { ip: "203.0.113.14" }),
    ap.env,
    { waitUntil() {} },
  );
  assert.equal(res.status, 200);
  const body = await res.json();
  assert.equal(body.ok, true);
  assert.equal(body.stored, false);
  assert.equal(ap.calls.length, 0);
});

test("/api/join rate-limits per IP (5/hour) without touching the AP", async () => {
  const ap = mockAp(apStored());
  const ip = "203.0.113.20";
  for (let i = 0; i < 5; i++) {
    const res = await worker.fetch(joinRequest(GOOD_APP, { ip }), ap.env, { waitUntil() {} });
    assert.equal((await res.json()).ok, true, `attempt ${i + 1}`);
  }
  const blocked = await worker.fetch(joinRequest(GOOD_APP, { ip }), ap.env, { waitUntil() {} });
  assert.equal(blocked.status, 200);
  const body = await blocked.json();
  assert.equal(body.ok, false);
  assert.match(body.error, /Too many/i);
  assert.equal(body.email, "crewlyllc@gmail.com");
  assert.equal(ap.calls.length, 5);

  // A different IP is unaffected.
  const other = await worker.fetch(joinRequest(GOOD_APP, { ip: "203.0.113.21" }), ap.env, {
    waitUntil() {},
  });
  assert.equal((await other.json()).ok, true);
});

test("/api/join success path never exposes the fallback address", async () => {
  const ap = mockAp(apStored());
  const res = await worker.fetch(joinRequest(GOOD_APP, { ip: "203.0.113.30" }), ap.env, {
    waitUntil() {},
  });
  const text = await res.text();
  assert.ok(!text.includes("crewlyllc@gmail.com"), `success body leaked email: ${text}`);
  assert.ok(!text.includes("hello@gpncoin.ai"));
  const body = JSON.parse(text);
  assert.equal(body.email, undefined);
  assert.equal(body.fallback, undefined);
});

test("/api/join rejects GET, answers OPTIONS preflight", async () => {
  const get = await worker.fetch(new Request("https://gpncoin.ai/api/join"), {}, { waitUntil() {} });
  assert.equal(get.status, 405);
  assert.equal((await get.json()).ok, false);

  const opt = await worker.fetch(
    new Request("https://gpncoin.ai/api/join", { method: "OPTIONS" }),
    {},
    { waitUntil() {} },
  );
  assert.equal(opt.status, 204);
  assert.match(opt.headers.get("access-control-allow-methods"), /POST/);
});

/* ── /api/* access-point proxy ─────────────────────────────────────────────── */

test("parseHttpResponse preserves non-2xx status, headers and body", () => {
  const enc = new TextEncoder();
  const raw = enc.encode(
    "HTTP/1.1 451 Unavailable For Legal Reasons\r\nContent-Type: application/json\r\n" +
      "Content-Length: 28\r\n\r\n" + '{"error":"policy_violation"}' + "\n",
  );
  const res = __test.parseHttpResponse(raw);
  assert.equal(res.status, 451);
  assert.equal(res.headers["content-type"], "application/json");
  assert.equal(res.body, '{"error":"policy_violation"}');

  // The 2xx-only reader the directory uses still refuses it.
  assert.equal(__test.parseHttpBody(raw), null);
});

test("/api proxy: unknown route, wrong method, no AP configured", async () => {
  const call = (req, env = {}) => worker.fetch(req, env, { waitUntil() {} });

  const unknown = await call(new Request("https://gpncoin.ai/api/nope"));
  assert.equal(unknown.status, 404);
  assert.equal(unknown.headers.get("access-control-allow-origin"), "*");

  const wrong1 = await call(new Request("https://gpncoin.ai/api/v1/models", { method: "POST" }));
  assert.equal(wrong1.status, 405);
  const wrong2 = await call(new Request("https://gpncoin.ai/api/v1/chat/completions"));
  assert.equal(wrong2.status, 405);

  const down = await call(
    new Request("https://gpncoin.ai/api/v1/chat/completions", {
      method: "POST",
      headers: { "content-type": "application/json", "cf-connecting-ip": "198.51.100.7" },
      body: JSON.stringify({ model: "llama3.2:1b", messages: [{ role: "user", content: "hi" }] }),
    }),
  );
  assert.equal(down.status, 502);
  assert.match((await down.json()).error.message, /not configured|unavailable/);
});

test("/api proxy: body cap, bad JSON, missing prompt", async () => {
  const post = (body, ip) =>
    worker.fetch(
      new Request("https://gpncoin.ai/api/v1/chat/completions", {
        method: "POST",
        headers: { "content-type": "application/json", "cf-connecting-ip": ip },
        body,
      }),
      {},
      { waitUntil() {} },
    );

  const big = await post(
    JSON.stringify({ model: "m", messages: [{ role: "user", content: "x".repeat(__test.AP_PROXY_MAX_BODY + 32) }] }),
    "198.51.100.8",
  );
  assert.equal(big.status, 413);

  const bad = await post("{nope", "198.51.100.9");
  assert.equal(bad.status, 400);
  assert.match((await bad.json()).error.message, /JSON/);

  const empty = await post(JSON.stringify({ model: "m" }), "198.51.100.10");
  assert.equal(empty.status, 400);
  assert.match((await empty.json()).error.message, /prompt/);
});

test("/api proxy: CORS is public for reads, site-scoped for chat POST", async () => {
  const pre = (origin) =>
    worker.fetch(
      new Request("https://gpncoin.ai/api/v1/chat/completions", {
        method: "OPTIONS",
        headers: { origin, "access-control-request-method": "POST" },
      }),
      {},
      { waitUntil() {} },
    );

  const good = await pre("https://gpncoin.ai");
  assert.equal(good.status, 204);
  assert.equal(good.headers.get("access-control-allow-origin"), "https://gpncoin.ai");
  assert.match(good.headers.get("access-control-allow-methods"), /POST/);

  const bad = await pre("https://evil.example");
  assert.notEqual(bad.headers.get("access-control-allow-origin"), "https://evil.example");

  // Reads are open to any origin.
  const read = await worker.fetch(
    new Request("https://gpncoin.ai/api/nope", { headers: { origin: "https://evil.example" } }),
    {},
    { waitUntil() {} },
  );
  assert.equal(read.headers.get("access-control-allow-origin"), "*");
});

test("rateLimit counts per key and resets after the window", async () => {
  assert.equal(await __test.rateLimit({}, "203.0.113.99", "unit-a", 2, 60), true);
  assert.equal(await __test.rateLimit({}, "203.0.113.99", "unit-a", 2, 60), true);
  assert.equal(await __test.rateLimit({}, "203.0.113.99", "unit-a", 2, 60), false);
  // A different bucket is independent.
  assert.equal(await __test.rateLimit({}, "203.0.113.99", "unit-b", 2, 60), true);
  // Zero-length window: every call starts a fresh slot.
  assert.equal(await __test.rateLimit({}, "203.0.113.99", "unit-c", 0, 0), true);
});

/* ── /api/v1/finetunes proxy ──────────────────────────────────────────────── */

/**
 * A fake AP transport: records the upstream call and answers with `reply`.
 * Mirrors what `apRequest` returns on a real socket.
 */
function stubAp(reply, seen = []) {
  return async (req) => {
    seen.push(req);
    return reply;
  };
}

const jsonReply = (status, payload) => ({
  status,
  contentType: "application/json",
  body: JSON.stringify(payload),
});

test("finetune submit: 202 is passed through with the AP body", async () => {
  const seen = [];
  const res = await worker.fetch(
    new Request("https://gpncoin.ai/api/v1/finetunes", {
      method: "POST",
      headers: {
        "content-type": "application/json",
        origin: "https://gpncoin.ai",
        "cf-connecting-ip": "203.0.113.41",
      },
      body: JSON.stringify({
        model: "llama3.2:1b",
        dataset: [{ prompt: "hi", completion: "hello" }],
        epochs: 1,
      }),
    }),
    {
      __test_ap_transport: stubAp(
        jsonReply(202, {
          id: "1234",
          object: "finetune",
          status: "queued",
          tuned_model: "tuned-1234",
          poll_url: "/v1/finetunes/1234",
        }),
        seen,
      ),
    },
    { waitUntil() {} },
  );

  assert.equal(res.status, 202);
  assert.equal(res.headers.get("x-gpn-proxy"), "access-point");
  assert.equal(res.headers.get("access-control-allow-origin"), "https://gpncoin.ai");
  const body = await res.json();
  assert.equal(body.id, "1234");
  assert.equal(body.tuned_model, "tuned-1234");
  // The proxy forwarded the submit to the AP's own path, method intact.
  assert.equal(seen.length, 1);
  assert.equal(seen[0].method, "POST");
  assert.equal(seen[0].path, "/v1/finetunes");
  assert.match(seen[0].body, /"model":"llama3.2:1b"/);
});

test("finetune submit: validates model and dataset before any upstream call", async () => {
  const seen = [];
  const env = { __test_ap_transport: stubAp(jsonReply(202, { id: "1" }), seen) };
  const post = (payload, ip) =>
    worker.fetch(
      new Request("https://gpncoin.ai/api/v1/finetunes", {
        method: "POST",
        headers: { "content-type": "application/json", "cf-connecting-ip": ip },
        body: JSON.stringify(payload),
      }),
      env,
      { waitUntil() {} },
    );

  const noModel = await post({ dataset: [{ prompt: "a", completion: "b" }] }, "203.0.113.42");
  assert.equal(noModel.status, 400);
  assert.match((await noModel.json()).error.message, /model/);

  const noData = await post({ model: "llama3.2:1b" }, "203.0.113.43");
  assert.equal(noData.status, 400);
  assert.match((await noData.json()).error.message, /dataset/);

  const emptyData = await post({ model: "llama3.2:1b", dataset: [] }, "203.0.113.44");
  assert.equal(emptyData.status, 400);

  // None of the rejects reached the AP.
  assert.equal(seen.length, 0);

  // GET on the submit route is a 405, not a 404.
  const wrong = await worker.fetch(
    new Request("https://gpncoin.ai/api/v1/finetunes", { headers: { "cf-connecting-ip": "203.0.113.45" } }),
    env,
    { waitUntil() {} },
  );
  assert.equal(wrong.status, 405);
  assert.match(wrong.headers.get("allow") || "", /POST/);
});

test("finetune poll: status passthrough including a 404 for an unknown id", async () => {
  const run = (id, reply, ip) =>
    worker.fetch(
      new Request(`https://gpncoin.ai/api/v1/finetunes/${id}`, {
        headers: { "cf-connecting-ip": ip },
      }),
      { __test_ap_transport: stubAp(reply) },
      { waitUntil() {} },
    );

  const running = await run(
    "1234",
    jsonReply(200, { id: "1234", object: "finetune", status: "running" }),
    "203.0.113.46",
  );
  assert.equal(running.status, 200);
  assert.equal((await running.json()).status, "running");
  assert.equal(running.headers.get("access-control-allow-origin"), "*");

  const missing = await run(
    "99999999",
    jsonReply(404, { error: { message: "unknown fine-tune id", type: "gpn_not_found" } }),
    "203.0.113.47",
  );
  assert.equal(missing.status, 404);
  assert.equal((await missing.json()).error.type, "gpn_not_found");
});

test("finetune artifact: /artifact/<id> is proxied as binary", async () => {
  const seen = [];
  const bytes = new TextEncoder().encode("PK\u0003\u0004fake-lora-zip");
  const res = await worker.fetch(
    new Request("https://gpncoin.ai/api/v1/finetunes/1234/artifact", {
      headers: { "cf-connecting-ip": "203.0.113.48" },
    }),
    {
      __test_ap_transport: stubAp(
        { status: 200, contentType: "application/zip", body: bytes, binary: true },
        seen,
      ),
    },
    { waitUntil() {} },
  );
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "application/zip");
  assert.equal(seen[0].path, "/artifact/1234");
  assert.equal(seen[0].binary, true);
  assert.equal((await res.arrayBuffer()).byteLength, bytes.byteLength);
});

test("finetune: CORS preflight allows POST from the site origin", async () => {
  const pre = (origin) =>
    worker.fetch(
      new Request("https://gpncoin.ai/api/v1/finetunes", {
        method: "OPTIONS",
        headers: { origin, "access-control-request-method": "POST" },
      }),
      {},
      { waitUntil() {} },
    );

  const ok = await pre("https://gpncoin.ai");
  assert.equal(ok.status, 204);
  assert.equal(ok.headers.get("access-control-allow-origin"), "https://gpncoin.ai");
  assert.match(ok.headers.get("access-control-allow-methods"), /POST/);

  const evil = await pre("https://evil.example");
  assert.notEqual(evil.headers.get("access-control-allow-origin"), "https://evil.example");
});

test("finetune: submissions are rate-limited (4/hour per IP)", async () => {
  const env = { __test_ap_transport: stubAp(jsonReply(202, { id: "1", status: "queued" })) };
  const post = (ip) =>
    worker.fetch(
      new Request("https://gpncoin.ai/api/v1/finetunes", {
        method: "POST",
        headers: { "content-type": "application/json", "cf-connecting-ip": ip },
        body: JSON.stringify({ model: "llama3.2:1b", dataset: [{ prompt: "a", completion: "b" }] }),
      }),
      env,
      { waitUntil() {} },
    );

  for (let i = 0; i < __test.FINETUNE_RATE_MAX; i += 1) {
    assert.equal((await post("203.0.113.49")).status, 202);
  }
  const limited = await post("203.0.113.49");
  assert.equal(limited.status, 429);
  assert.equal(limited.headers.get("retry-after"), String(__test.FINETUNE_RATE_WINDOW_SECONDS));
  assert.match((await limited.json()).error.type, /rate_limited/);

  // A different IP is unaffected.
  assert.equal((await post("203.0.113.50")).status, 202);
});

test("finetune: a dataset body over the chat cap is accepted, past 256 KB is not", async () => {
  const submit = (body, ip) =>
    worker.fetch(
      new Request("https://gpncoin.ai/api/v1/finetunes", {
        method: "POST",
        headers: { "content-type": "application/json", "cf-connecting-ip": ip },
        body,
      }),
      { __test_ap_transport: stubAp(jsonReply(202, { id: "7", status: "queued" })) },
      { waitUntil() {} },
    );

  // ~100 KB of prompt/completion text: over AP_PROXY_MAX_BODY (64 KB) on
  // purpose — 200 pairs are allowed, so the chat cap must not apply here.
  const pair = { prompt: "p".repeat(450), completion: "c".repeat(450) };
  const many = JSON.stringify({ model: "llama3.2:1b", dataset: new Array(110).fill(pair) });
  assert.ok(many.length > __test.AP_PROXY_MAX_BODY);
  assert.ok(many.length < __test.FINETUNE_MAX_BODY);
  assert.equal((await submit(many, "203.0.113.51")).status, 202);

  const huge = JSON.stringify({
    model: "llama3.2:1b",
    dataset: [{ prompt: "x".repeat(__test.FINETUNE_MAX_BODY), completion: "y" }],
  });
  assert.equal((await submit(huge, "203.0.113.52")).status, 413);
});

test("GET /chain-spec proxies the AP's /genesis-spec as JSON", async () => {
  const spec = {
    name: "GPN Public Testnet",
    id: "gpn_testnet",
    bootNodes: [
      "/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo",
    ],
    genesis: { runtimeGenesis: { code: "0x0061736d" } },
  };
  const seen = [];
  const env = {
    AP_JOB_URL: "http://203.0.113.7:8092",
    __test_ap_transport: async (opts) => {
      seen.push(opts);
      return { status: 200, contentType: "application/json", body: JSON.stringify(spec) };
    },
  };
  const res = await worker.fetch(new Request("https://gpncoin.ai/chain-spec"), env, {
    waitUntil() {},
  });
  assert.equal(res.status, 200);
  assert.match(res.headers.get("content-type"), /application\/json/);
  assert.equal(res.headers.get("access-control-allow-origin"), "*");
  assert.equal(res.headers.get("x-gpn-spec-source"), "access-point");
  assert.match(res.headers.get("content-disposition"), /gpn-testnet-spec\.json/);
  const body = await res.json();
  assert.deepEqual(body.bootNodes, spec.bootNodes);
  assert.deepEqual(body.genesis, spec.genesis);
  assert.equal(seen[0].method, "GET");
  assert.equal(seen[0].path, "/genesis-spec");
});

test("GET /genesis-spec is an alias of /chain-spec", async () => {
  const env = {
    AP_URL: "http://203.0.113.7:8092",
    __test_ap_transport: async () => ({
      status: 200,
      contentType: "application/json",
      body: '{"genesis":{},"bootNodes":[]}',
    }),
  };
  const res = await worker.fetch(new Request("https://gpncoin.ai/genesis-spec"), env, {
    waitUntil() {},
  });
  assert.equal(res.status, 200);
  assert.equal((await res.json()).bootNodes.length, 0);
});

test("/chain-spec is a JSON 503 (never HTML) when the AP is unreachable", async () => {
  const res = await worker.fetch(new Request("https://gpncoin.ai/chain-spec"), {}, {
    waitUntil() {},
  });
  assert.equal(res.status, 503);
  assert.match(res.headers.get("content-type"), /application\/json/);
  assert.match((await res.json()).error, /chain spec unavailable/);
});

test("/chain-spec passes through upstream errors and rejects non-GET", async () => {
  const env = {
    AP_JOB_URL: "http://203.0.113.7:8092",
    __test_ap_transport: async () => ({
      status: 500,
      contentType: "application/json",
      body: '{"error":"no spec file"}',
    }),
  };
  const res = await worker.fetch(new Request("https://gpncoin.ai/chain-spec"), env, {
    waitUntil() {},
  });
  assert.equal(res.status, 500);
  assert.equal((await res.json()).error, "no spec file");

  const post = await worker.fetch(
    new Request("https://gpncoin.ai/chain-spec", { method: "POST" }),
    env,
    { waitUntil() {} },
  );
  assert.equal(post.status, 405);
  assert.match(post.headers.get("allow"), /GET/);

  const opt = await worker.fetch(
    new Request("https://gpncoin.ai/chain-spec", { method: "OPTIONS" }),
    env,
    { waitUntil() {} },
  );
  assert.equal(opt.status, 204);
});

test("HEAD /chain-spec answers 200 with no body", async () => {
  const env = {
    AP_JOB_URL: "http://203.0.113.7:8092",
    __test_ap_transport: async () => ({
      status: 200,
      contentType: "application/json",
      body: '{"genesis":{},"bootNodes":[]}',
    }),
  };
  const res = await worker.fetch(
    new Request("https://gpncoin.ai/chain-spec", { method: "HEAD" }),
    env,
    { waitUntil() {} },
  );
  assert.equal(res.status, 200);
  assert.equal(await res.text(), "");
});

/* ── live network directory (`/api/network`, `/seeds.json` liveness) ────── */

const LIVE_A = "12D3KooWFqZ2msYzP3263AKGSUfLzUC7Am7FyRDqi2KKfgREmxGz";
const LIVE_B = "12D3KooWRfKQbKnynFsKVyi76FjFDWLQqQPTKZFrMFF6Zt7zoZ2o";

test("seeds.json dedupes one machine published as a seed and a provider", () => {
  const staticMa = network.bootnodes[0];
  const pid = __test.peerIdOf(staticMa);
  const body = __test.buildDirectory(
    [{ status_url: "http://203.0.113.7:8093/status", healthy: true, peer_id: pid, multiaddrs: [staticMa] }],
    [pid],
  );
  // One row for that peer, not seed + provider.
  assert.equal(body.nodes.filter((n) => n.peer_id === pid).length, 1);
  assert.equal(body.nodes.find((n) => n.peer_id === pid).role, "seed");
  assert.equal(body.nodes.find((n) => n.peer_id === pid).live, true);
  // The dial list still carries the address once.
  assert.equal(body.multiaddrs.filter((m) => m === staticMa).length, 1);
});

test("seeds.json marks a stale seed not live and exposes it in `seeds`", () => {
  const body = __test.buildDirectory(null, [LIVE_A, LIVE_B]);
  const byPeer = new Map(body.seeds.map((s) => [s.peer_id, s]));
  // The Linux box's own seed (dns4) is not in the live set here → false.
  const selfPid = __test.peerIdOf(network.bootnodes[0]);
  assert.equal(byPeer.get(selfPid).live, false);
  // Box #1 is live.
  assert.equal(byPeer.get(LIVE_A).live, true);
  // Every published seed has a real multiaddr and a peer id.
  for (const s of body.seeds) {
    assert.ok(s.multiaddr.startsWith("/"));
    assert.match(s.multiaddr, /\/p2p\/12D3Koo/);
  }
});

test("seeds.json says live:null when the AP could not report liveness", () => {
  const body = __test.buildDirectory(null);
  for (const n of body.nodes) assert.equal(n.live, null, "must never claim offline without evidence");
});

test("GET /api/network proxies the AP's live document unchanged", async () => {
  const seen = [];
  const doc = {
    generated_at: "2026-10-30T20:00:00Z",
    peers: 4,
    peer_ids: [LIVE_A, LIVE_B],
    providers: [{ peer_id: LIVE_A, healthy: true, vram_mb: 8188, models: ["m"], multiaddrs: [], last_seen_secs: 3, status_url: "http://203.0.113.7:8093/status", address: "5A" }],
    seeds: [{ multiaddr: network.bootnodes[0], peer_id: __test.peerIdOf(network.bootnodes[0]), live: true, source: "compiled-in" }],
    counts: { peers_live: 4, serving_ai: 1, offline: 1 },
    stale: false,
  };
  const env = { __test_ap_transport: stubAp(jsonReply(200, doc), seen) };
  const res = await worker.fetch(
    new Request("https://gpncoin.ai/api/network", { headers: { "cf-connecting-ip": "203.0.113.60" } }),
    env,
    { waitUntil() {} },
  );
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("x-gpn-proxy"), "access-point");
  assert.equal(res.headers.get("access-control-allow-origin"), "*");
  // Passed through byte-for-byte: the AP is authoritative for counts.
  assert.deepEqual(await res.json(), doc);
  assert.equal(seen.length, 1);
  assert.equal(seen[0].method, "GET");
  assert.equal(seen[0].path, "/network");
});

test("GET /api/network is a JSON 502 (never HTML) when the AP is unreachable", async () => {
  const res = await worker.fetch(
    new Request("https://gpncoin.ai/api/network"),
    { AP_URL: "http://127.0.0.1:1" },
    { waitUntil() {} },
  );
  assert.equal(res.status, 502);
  assert.match(res.headers.get("content-type"), /application\/json/);
  const body = await res.json();
  assert.equal(body.error.type, "gpn_proxy_error");
});
