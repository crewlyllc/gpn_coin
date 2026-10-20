// Unit tests for the gpncoin.ai peer-directory Worker.
// Run: node --test worker/worker.test.mjs
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

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
  const pid = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
  const body = __test.buildDirectory([
    { status_url: "http://203.0.113.7:8093/status", healthy: true, peer_id: pid, p2p_port: 30333, multiaddrs: [] },
  ]);
  assert.deepEqual(body.multiaddrs.slice(-1), [`/ip4/203.0.113.7/tcp/30333/p2p/${pid}`]);
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
  const pid = "12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo";
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
