/**
 * gpncoin.ai — static site + live peer directory (DDNS-style).
 *
 * Routes owned by this Worker (see `.assets.run_worker_first` in wrangler.jsonc):
 *
 *   GET /seeds.json     canonical network directory (JSON)
 *   GET /bootnodes.txt  the same dialable multiaddrs, newline separated
 *   GET /seeds.txt      alias of /bootnodes.txt
 *   GET /releases.json  the SIGNED release manifest (see docs/updates.md)
 *   POST /api/join      job application from the home page (see below)
 *
 * `/api/join` accepts `{name, email, role, message}` as JSON, validates it,
 * rate-limits per IP, and stores each application in the `JOIN_KV` namespace
 * under `app:<ISO timestamp>:<id>`. It **always** answers 200 with JSON so the
 * page can render one code path; `ok:false` carries a human-readable `error`.
 * When `JOIN_KV` is not bound the route still returns 200 but sets
 * `stored:false`, and the page offers a prefilled mailto fallback — a
 * half-configured deploy must never look like a successful submission.
 * Honeypot field: `website` (hidden on the page). A filled honeypot returns the
 * normal success shape and stores nothing.
 *
 * `/releases.json` is generated and signed at release time by
 * `scripts/gen-release-manifest.py` into `website/releases.json`; this route
 * only fixes the headers (long-ish TTL, JSON content type, no HTML fallthrough)
 * and answers a clean 503 JSON — never an HTML 404 — when no release has been
 * published yet, so a node logs a readable error instead of an HTML parse
 * failure.
 *
 * Everything else is served from the `./website` asset bundle.
 *
 * The directory is the union of
 *   (a) the compiled-in seed list (`network.json` at the repo root — the same
 *       list the node ships with), and
 *   (b) the live provider registry of the GPN access point (`GET /providers`),
 *       filtered to entries the AP currently reports as healthy.
 *
 * If the AP is unreachable, slow, or returns garbage the response is still
 * 200 with the static list — the discovery endpoint must never 500. The
 * combined result is cached in-isolate for 60 s, and sent with
 * `Cache-Control: public, max-age=60` so Cloudflare's edge cache absorbs the
 * rest.
 *
 * Wire format (must stay compatible with gpn-node's `parse_seed_payload`):
 *   {
 *     "multiaddrs": ["/dns4/…/tcp/30333/p2p/12D3Koo…", …],
 *     …metadata…
 *   }
 * A client that only understands the legacy object/array form still works,
 * because `multiaddrs` is the first thing it looks for.
 */

/**
 * The compiled-in seed list. This is the same set as `network.json` (the node
 * ships it too). It is duplicated here rather than imported so the Worker has
 * no build-time coupling to files outside its bundle — `worker.test.mjs`
 * asserts the two lists stay identical.
 */
const STATIC_BOOTNODES = [
  "/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo",
  "/ip4/142.129.54.88/tcp/30334/p2p/12D3KooWRWv4yVdJGiVg7xRgdUJY9G57EeutB596gWVpAwGC9qQ2",
];

const GENESIS = "0xb8e4636a65d0c812abb437c38dd7e937bd7c3355c54c54d63060f26a33519a5c";
const CHAIN_ID = "gpn_testnet";
const NETWORK = "GPN Public Testnet";

/** Directory TTL, in seconds. Also the HTTP cache lifetime. */
const TTL_SECONDS = 60;

/** How long to wait for the access point before falling back to static. */
const AP_TIMEOUT_MS = 2000;

/**
 * In-isolate cache. Cloudflare may run several isolates, so this is a
 * best-effort optimisation on top of the HTTP cache, not a correctness
 * requirement.
 * @type {{ at: number, body: object | null }}
 */
let cache = { at: 0, body: null };

export default {
  /**
   * @param {Request} request
   * @param {{ ASSETS?: { fetch: (r: Request) => Promise<Response> }, AP_URL?: string }} env
   * @param {{ waitUntil: (p: Promise<unknown>) => void }} ctx
   */
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    const path = url.pathname.replace(/\/+$/, "") || "/";

    if (path === "/seeds.json" || path === "/seeds" || path === "/bootnodes.txt" || path === "/seeds.txt") {
      if (request.method === "OPTIONS") return preflight();
      if (request.method !== "GET" && request.method !== "HEAD") {
        return withCors(new Response("method not allowed\n", { status: 405 }));
      }
      const body = await directory(env, ctx);
      const isJson = path !== "/bootnodes.txt" && path !== "/seeds.txt";
      return isJson ? jsonResponse(body) : textResponse(body);
    }

    if (path === "/releases.json") {
      if (request.method === "OPTIONS") return preflight();
      if (request.method !== "GET" && request.method !== "HEAD") {
        return withCors(new Response("method not allowed\n", { status: 405 }));
      }
      return releaseManifest(request, env, url);
    }

    if (path === "/api/join") {
      if (request.method === "OPTIONS") return preflight();
      if (request.method !== "POST") {
        return withCors(
          new Response(JSON.stringify({ ok: false, error: "Use POST." }) + "\n", {
            status: 405,
            headers: { "content-type": "application/json; charset=utf-8", allow: "POST, OPTIONS" },
          }),
        );
      }
      return handleJoin(request, env);
    }

    // Static site: hand the untouched request to the assets binding.
    if (env && env.ASSETS && typeof env.ASSETS.fetch === "function") {
      return env.ASSETS.fetch(request);
    }
    return withCors(new Response("not found\n", { status: 404 }));
  },
};

/**
 * Build the directory, using the isolate cache when fresh.
 * @param {{ AP_URL?: string }} env
 * @param {{ waitUntil: (p: Promise<unknown>) => void }} ctx
 */
async function directory(env, ctx) {
  const now = Date.now();
  if (cache.body && now - cache.at < TTL_SECONDS * 1000) return cache.body;

  const live = await fetchLiveProviders(env);
  const body = buildDirectory(live);
  cache = { at: now, body };
  // Nothing to defer today; kept so a future KV/registration write can be
  // pushed off the request path without changing the signature.
  void ctx;
  return body;
}

/**
 * Query the AP's provider registry. Returns `null` when the AP is not
 * configured or not reachable — callers must treat that as "static only".
 * @param {{ AP_URL?: string }} env
 * @returns {Promise<Array<object> | null>}
 */
async function fetchLiveProviders(env) {
  const base = (env && env.AP_URL ? String(env.AP_URL) : "").trim().replace(/\/+$/, "");
  if (!base) return null;

  // Preferred path: a normal fetch. Works for an HTTPS ingress (tunnel or
  // reverse proxy) and for the standard ports.
  // `AP_FORCE_SOCKET=1` is a diagnostics switch (used to exercise the socket
  // path in `wrangler dev`); it is not set in production.
  if (!(env && env.AP_FORCE_SOCKET && String(env.AP_FORCE_SOCKET) !== "0")) {
    const viaFetch = await providersViaFetch(base);
    if (viaFetch) return viaFetch;
  }

  // Fallback: the AP today listens on a non-standard port (8092), which the
  // Workers `fetch()` API refuses. Raw TCP sockets have no such limit, so a
  // minimal HTTP/1.1 GET over `cloudflare:sockets` keeps the directory live
  // without extra infrastructure.
  return providersViaSocket(base);
}

/** @param {string} base */
async function providersViaFetch(base) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), AP_TIMEOUT_MS);
  try {
    const res = await fetch(`${base}/providers`, {
      signal: controller.signal,
      headers: { accept: "application/json", "user-agent": "gpn-directory/1" },
      // The AP is authoritative for liveness; never serve a stale edge copy.
      cf: { cacheTtl: 0, cacheEverything: false },
    });
    if (!res.ok) return null;
    const doc = await res.json();
    return doc && Array.isArray(doc.providers) ? doc.providers : null;
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
  }
}

/**
 * HTTP/1.1 GET over a raw Workers TCP socket. Plain `http://` only — TLS is
 * left to `fetch()`. Returns null on any failure (never throws).
 * @param {string} base
 */
async function providersViaSocket(base) {
  let url;
  try {
    url = new URL(base);
  } catch {
    return null;
  }
  if (url.protocol !== "http:") return null;
  const host = url.hostname.replace(/^\[|\]$/g, "");
  const port = Number(url.port || "80");
  if (!host || !Number.isInteger(port) || port < 1 || port > 65535) return null;

  /** @type {{ connect?: Function }} */
  let sockets;
  try {
    sockets = await import("cloudflare:sockets");
  } catch {
    return null; // e.g. Node test runner: no Workers runtime available
  }
  if (!sockets || typeof sockets.connect !== "function") return null;

  let socket;
  try {
    socket = sockets.connect({ hostname: host, port });
  } catch {
    return null;
  }

  const timer = setTimeout(() => {
    try {
      socket.close();
    } catch {}
  }, AP_TIMEOUT_MS);

  try {
    const writer = socket.writable.getWriter();
    await writer.write(
      new TextEncoder().encode(
        `GET /providers HTTP/1.1\r\nHost: ${host}:${port}\r\n` +
          "User-Agent: gpn-directory/1\r\nAccept: application/json\r\n" +
          "Accept-Encoding: identity\r\nConnection: close\r\n\r\n",
      ),
    );
    writer.releaseLock();

    const reader = socket.readable.getReader();
    const chunks = [];
    let total = 0;
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      if (value) {
        chunks.push(value);
        total += value.byteLength;
        // The AP answers with a bounded JSON document; refuse runaway bodies.
        if (total > 1_000_000) break;
      }
    }
    reader.releaseLock();

    const body = parseHttpBody(concatBytes(chunks));
    if (!body) return null;
    const doc = JSON.parse(body);
    return doc && Array.isArray(doc.providers) ? doc.providers : null;
  } catch {
    return null;
  } finally {
    clearTimeout(timer);
    try {
      socket.close();
    } catch {}
  }
}

/** @param {Uint8Array[]} chunks */
function concatBytes(chunks) {
  const size = chunks.reduce((n, c) => n + c.byteLength, 0);
  const out = new Uint8Array(size);
  let at = 0;
  for (const c of chunks) {
    out.set(c, at);
    at += c.byteLength;
  }
  return out;
}

/**
 * Parse a raw HTTP/1.1 response body (Content-Length or chunked).
 * @param {Uint8Array} raw
 * @returns {string | null}
 */
function parseHttpBody(raw) {
  const needle = [13, 10, 13, 10];
  let split = -1;
  for (let i = 0; i + 3 < raw.length; i++) {
    if (raw[i] === needle[0] && raw[i + 1] === needle[1] && raw[i + 2] === needle[2] && raw[i + 3] === needle[3]) {
      split = i;
      break;
    }
  }
  if (split < 0) return null;
  const head = new TextDecoder().decode(raw.subarray(0, split));
  let body = raw.subarray(split + 4);
  const lines = head.split("\r\n");
  const status = Number((lines.shift() || "").split(/\s+/)[1]);
  if (!(status >= 200 && status < 300)) return null;

  let chunked = false;
  let length = null;
  for (const line of lines) {
    const i = line.indexOf(":");
    if (i < 0) continue;
    const name = line.slice(0, i).trim().toLowerCase();
    const value = line.slice(i + 1).trim();
    if (name === "transfer-encoding" && value.toLowerCase().includes("chunked")) chunked = true;
    if (name === "content-length") length = Number(value);
  }

  const decoder = new TextDecoder();
  if (!chunked) {
    const end = length !== null && length <= body.length ? length : body.length;
    return decoder.decode(body.subarray(0, end));
  }

  let out = "";
  let at = 0;
  for (;;) {
    const lineEnd = indexOfCrlf(body, at);
    if (lineEnd < 0) return out;
    const size = parseInt(decoder.decode(body.subarray(at, lineEnd)).split(";")[0].trim(), 16);
    if (!Number.isFinite(size) || size < 0) return null;
    at = lineEnd + 2;
    if (size === 0) return out;
    if (at + size > body.length) return null;
    out += decoder.decode(body.subarray(at, at + size));
    at += size;
    if (body[at] === 13 && body[at + 1] === 10) at += 2;
  }
}

/**
 * @param {Uint8Array} buf
 * @param {number} from
 */
function indexOfCrlf(buf, from) {
  for (let i = from; i + 1 < buf.length; i++) {
    if (buf[i] === 13 && buf[i + 1] === 10) return i;
  }
  return -1;
}

/**
 * Merge the static seeds with the live provider list into the public shape.
 *
 * Two passes over the live providers, deliberately:
 *
 *  1. **Claim** — every *explicit* multiaddr a provider published about itself
 *     is authoritative: it binds `peer_id -> host` and `host:port -> peer_id`.
 *     Static seeds claim first (they are hard-coded truth).
 *  2. **Publish** — explicit addresses are emitted as-is; a *synthesised*
 *     address (`<host the AP reached us on>:<p2p_port>/p2p/<peer_id>`) is only
 *     emitted when it cannot contradict those claims.
 *
 * That is what prevents the 2026-10-20 directory bug:
 * `/ip4/23.241.109.255/tcp/30333/p2p/12D3KooWAFtUNxc…` = box #1's status_url
 * host paired with the Linux node's peer id. A peer id is one node, so it may
 * not be invented at a second host; and an `ip:port` belongs to one node, so
 * it may not be re-used for a different peer id.
 *
 * @param {Array<object> | null} providers
 */
function buildDirectory(providers) {
  /** @type {Array<{multiaddr: string, peer_id?: string, role: string, source: string, best?: boolean}>} */
  const nodes = [];
  const multiaddrs = [];
  const seen = new Set();

  /** Claims gathered from addresses a node published about itself. */
  const claims = { hostsByPeer: new Map(), ownerByEndpoint: new Map() };

  for (const ma of STATIC_BOOTNODES) {
    if (typeof ma !== "string" || !ma.startsWith("/")) continue;
    const pid = peerIdOf(ma);
    if (pid && sanitizePeerId(pid)) claimEndpoint(claims, pid, ma);
    if (seen.has(ma)) continue;
    seen.add(ma);
    multiaddrs.push(ma);
    nodes.push({ multiaddr: ma, peer_id: peerIdOf(ma), role: "seed", source: "compiled-in" });
  }

  /** Live providers, normalised once. */
  const records = [];
  /** Endpoints that are live but cannot be dialed as libp2p peers yet. */
  const endpoints = [];
  let live = 0;
  if (Array.isArray(providers)) {
    for (const p of providers) {
      if (!p || p.healthy === false) continue;
      const statusUrl = typeof p.status_url === "string" ? p.status_url : "";
      if (!statusUrl) continue;
      live += 1;

      // Peer-id publication: a provider that reports its libp2p identity can
      // be published as a DIALABLE peer. Without a peer id it stays
      // informational (status_url only), exactly as before.
      const statusMa = multiaddrFromUrl(statusUrl);
      const statusPeerId = statusMa ? sanitizePeerId(peerIdOf(statusMa)) : null;
      const peerId = sanitizePeerId(p.peer_id) || statusPeerId;
      records.push({ p, statusUrl, statusMa, peerId });
    }
  }

  // Pass 1: explicit self-reported addresses claim their identity + endpoint.
  for (const r of records) {
    if (!r.peerId) continue;
    for (const ma of explicitMultiaddrs(r.p, r.peerId)) {
      claimEndpoint(claims, r.peerId, ma);
    }
  }

  // Pass 2: publish.
  for (const r of records) {
    const { p, statusUrl, statusMa, peerId } = r;
    const dialable = peerId ? dialableMultiaddrs(p, statusUrl, peerId, claims) : [];

    if (dialable.length > 0) {
      for (const ma of dialable) {
        if (seen.has(ma)) continue;
        seen.add(ma);
        multiaddrs.push(ma);
        nodes.push({
          multiaddr: ma,
          peer_id: peerId,
          role: "provider",
          source: "ap",
          endpoint: statusUrl,
          address: typeof p.address === "string" ? p.address : null,
          best: false,
        });
      }
      continue;
    }

    // Not dialable: keep the endpoint visible for tooling/monitoring.
    endpoints.push({
      endpoint: statusUrl,
      multiaddr: statusMa || null,
      peer_id: peerId || null,
      role: "provider",
      source: "ap",
      address: typeof p.address === "string" ? p.address : null,
      best: false,
    });
  }

  const source = live > 0 ? "static+ap" : Array.isArray(providers) ? "static+ap-empty" : "static-fallback";

  return {
    network: NETWORK,
    chain_id: CHAIN_ID,
    genesis: GENESIS,
    updated_at: new Date().toISOString(),
    updated: Math.floor(Date.now() / 1000),
    ttl: TTL_SECONDS,
    source,
    live_providers: live,
    // The field gpn-node actually reads.
    multiaddrs,
    // Rich form: one entry per node/endpoint, including non-dialable providers.
    nodes: nodes.concat(endpoints),
    publisher: "gpncoin.ai",
  };
}

/**
 * The explicitly self-reported multiaddrs of a provider: sanitised, and only
 * those that end in **that provider's own** peer id.
 * @param {object} p
 * @param {string} peerId
 * @returns {string[]}
 */
function explicitMultiaddrs(p, peerId) {
  const out = [];
  if (!Array.isArray(p.multiaddrs)) return out;
  for (const raw of p.multiaddrs) {
    const ma = sanitizeMultiaddr(raw);
    if (!ma) continue;
    if (peerIdOf(ma) !== peerId) continue; // an address cannot vouch for another id
    if (!out.includes(ma)) out.push(ma);
  }
  return out;
}

/**
 * Record what an address claims about identity and location.
 *
 * `hostsByPeer`: peer id -> hosts it published for itself (a peer id is ONE
 * node, so a second host for the same id is a conflict, not a second location).
 * `ownerByEndpoint`: `host/tcp/port` -> peer id (an endpoint belongs to one
 * node; never synthesise another node's id onto it).
 * @param {{hostsByPeer: Map<string, Set<string>>, ownerByEndpoint: Map<string, string>}} claims
 * @param {string} peerId
 * @param {string} ma
 */
function claimEndpoint(claims, peerId, ma) {
  const host = hostOfMultiaddr(ma);
  const endpoint = endpointOfMultiaddr(ma);
  if (host) {
    if (!claims.hostsByPeer.has(peerId)) claims.hostsByPeer.set(peerId, new Set());
    claims.hostsByPeer.get(peerId).add(host);
  }
  if (endpoint && !claims.ownerByEndpoint.has(endpoint)) {
    claims.ownerByEndpoint.set(endpoint, peerId);
  }
}

/** `/ip4/1.2.3.4/tcp/30333/p2p/…` -> `/ip4/1.2.3.4` (the host half only). */
const HOST_RE = /^\/(ip4|ip6|dns|dns4|dns6)\/([^/]+)/;

/** @param {string} ma @returns {string | null} */
function hostOfMultiaddr(ma) {
  const m = HOST_RE.exec(ma);
  return m ? `/${m[1]}/${m[2]}` : null;
}

/** @param {string} ma @returns {string | null} */
function endpointOfMultiaddr(ma) {
  const m = /^(\/(?:ip4|ip6|dns|dns4|dns6)\/[^/]+\/tcp\/\d{1,5})/.exec(ma);
  return m ? m[1] : null;
}

/**
 * Multiaddrs for a provider that published its peer id. Prefers the addresses
 * the provider's heartbeat reported; when those are absent (a box listening on
 * 0.0.0.0 cannot know its own public IP) it synthesises one from the host the
 * AP already reached it on + the advertised p2p port.
 *
 * A synthesised address must use **this provider's own** peer id and a
 * `p2p_port` from this same record, and it is dropped when it would
 * contradict a claim already made (the id published at another host, or this
 * endpoint owned by another id). Every returned address ends in
 * `/p2p/<peerId>` and passed `sanitizeMultiaddr`.
 * @param {object} p
 * @param {string} statusUrl
 * @param {string} peerId
 * @param {{hostsByPeer?: Map<string, Set<string>>, ownerByEndpoint?: Map<string, string>}} [claims]
 * @returns {string[]}
 */
function dialableMultiaddrs(p, statusUrl, peerId, claims) {
  const explicit = explicitMultiaddrs(p, peerId);
  if (explicit.length > 0) return explicit;

  const port = Number.isInteger(p.p2p_port) && p.p2p_port >= 1 && p.p2p_port <= 65535 ? p.p2p_port : null;
  const base = multiaddrFromUrl(statusUrl);
  // Legacy shape: some publishers list a full multiaddr as `status_url`.
  if (base && peerIdOf(base) === peerId && sanitizeMultiaddr(base)) return [base];
  if (!port || !base) return [];

  const host = hostOfMultiaddr(base);
  if (!host) return [];

  const hostsByPeer = claims?.hostsByPeer;
  const ownerByEndpoint = claims?.ownerByEndpoint;

  // A peer id is one node. If it is already published at a different host
  // (by a static seed, or by an address it reported about itself), do not
  // invent a second location for it: one of the two would be a lie, and this
  // is exactly how box #1's host got paired with the Linux node's id.
  const known = hostsByPeer?.get(peerId);
  if (known && known.size > 0 && !(known.size === 1 && known.has(host))) return [];

  // An endpoint belongs to one node: never reuse another provider's address.
  const endpoint = `${host}/tcp/${port}`;
  const owner = ownerByEndpoint?.get(endpoint);
  if (owner && owner !== peerId) return [];

  const ma = `${endpoint}/p2p/${peerId}`;
  return sanitizeMultiaddr(ma) ? [ma] : [];
}

/**
 * Accept only a plausible libp2p peer id (base58, `12D3Koo…` or `Qm…`).
 * @param {unknown} raw
 * @returns {string | null}
 */
function sanitizePeerId(raw) {
  if (typeof raw !== "string") return null;
  const s = raw.trim();
  if (s.length < 40 || s.length > 64) return null;
  if (!/^[A-Za-z0-9]+$/.test(s)) return null;
  return s.startsWith("12D3Koo") || s.startsWith("Qm") ? s : null;
}

/** Protocols a published p2p multiaddr may contain. */
const P2P_PROTOCOLS = new Set([
  "ip4", "ip6", "dns", "dns4", "dns6", "tcp", "udp", "quic", "quic-v1", "p2p", "tls", "sni", "ws", "wss",
]);

/**
 * Structural multiaddr check: `/proto/value/…`, whitelisted protocols, a
 * numeric TCP/UDP port, and a trailing `/p2p/<peer-id>` (the only form a
 * `gpn-node` can dial). Returns the trimmed string or `null`.
 * @param {unknown} raw
 * @returns {string | null}
 */
function sanitizeMultiaddr(raw) {
  if (typeof raw !== "string") return null;
  const s = raw.trim();
  if (!s.startsWith("/") || s.length > 256) return null;
  if (/[\s"\\]/.test(s)) return null;
  const parts = s.split("/").slice(1);
  if (parts.length < 4 || parts.length % 2 !== 0 || parts.some((p) => p === "")) return null;
  let hasP2p = false;
  for (let i = 0; i + 1 < parts.length; i += 2) {
    const proto = parts[i];
    const value = parts[i + 1];
    if (!P2P_PROTOCOLS.has(proto)) return null;
    if (proto === "tcp" || proto === "udp") {
      if (!/^\d{1,5}$/.test(value)) return null;
      const n = Number(value);
      if (n < 1 || n > 65535) return null;
    }
    if (proto === "p2p") {
      if (i + 2 !== parts.length) return null; // must be last
      if (!sanitizePeerId(value)) return null;
      hasP2p = true;
    }
  }
  return hasP2p ? s : null;
}

/** @param {string} ma */
function peerIdOf(ma) {
  const i = ma.lastIndexOf("/p2p/");
  return i >= 0 ? ma.slice(i + 5) : null;
}

/**
 * Turn `http://203.0.113.7:8093/status` (or `host:port`) into a libp2p
 * multiaddr. The result has no peer id, so it is informational unless a peer
 * id is appended by the producer.
 * @param {string} raw
 * @returns {string | null}
 */
function multiaddrFromUrl(raw) {
  let text = String(raw).trim();
  if (!text) return null;
  if (text.startsWith("/")) return text; // already a multiaddr
  text = text.replace(/^[a-z0-9+.-]+:\/\//i, "");
  const authority = text.split(/[/?#]/, 1)[0];
  const hostPort = authority.includes("@") ? authority.slice(authority.lastIndexOf("@") + 1) : authority;

  let host;
  let port = "";
  if (hostPort.startsWith("[")) {
    const end = hostPort.indexOf("]");
    if (end < 0) return null;
    host = hostPort.slice(1, end);
    const rest = hostPort.slice(end + 1);
    if (!rest.startsWith(":")) return null;
    port = rest.slice(1);
  } else {
    const idx = hostPort.lastIndexOf(":");
    if (idx < 0) return null;
    host = hostPort.slice(0, idx);
    port = hostPort.slice(idx + 1);
  }
  if (!host || !/^\d{1,5}$/.test(port)) return null;
  const p = Number(port);
  if (p < 1 || p > 65535) return null;

  const kind = /^\d{1,3}(\.\d{1,3}){3}$/.test(host)
    ? "ip4"
    : host.includes(":")
      ? "ip6"
      : "dns4";
  return `/${kind}/${host.toLowerCase()}/tcp/${p}`;
}

/** @param {object} body */
function jsonResponse(body) {
  return withCors(
    new Response(JSON.stringify(body, null, 2) + "\n", {
      status: 200,
      headers: {
        "content-type": "application/json; charset=utf-8",
        "cache-control": `public, max-age=${TTL_SECONDS}`,
        "x-gpn-source": String(body.source || "static"),
      },
    }),
  );
}

/** @param {object} body */
function textResponse(body) {
  const lines = (body.multiaddrs || []).join("\n");
  return withCors(
    new Response(lines + "\n", {
      status: 200,
      headers: {
        "content-type": "text/plain; charset=utf-8",
        "cache-control": `public, max-age=${TTL_SECONDS}`,
        "x-gpn-source": String(body.source || "static"),
      },
    }),
  );
}

function preflight() {
  return withCors(new Response(null, { status: 204 }));
}

/**
 * Serve `website/releases.json` with the headers a node's updater expects.
 *
 * The manifest is served from the asset bundle (it is produced by the release
 * script, not by this Worker), so there is one source of truth and no chance of
 * the Worker and the file disagreeing. A missing manifest is a 503 with
 * `no-store` — the node treats that as "nothing published yet" and never as
 * "you are up to date".
 *
 * @param {Request} request
 * @param {{ ASSETS?: { fetch: (r: Request) => Promise<Response> } }} env
 * @param {URL} url
 */
async function releaseManifest(request, env, url) {
  const notPublished = () =>
    withCors(
      new Response(
        JSON.stringify({ error: "release manifest not published" }, null, 2) + "\n",
        {
          status: 503,
          headers: {
            "content-type": "application/json; charset=utf-8",
            "cache-control": "no-store",
            "retry-after": "300",
          },
        },
      ),
    );

  if (!env || !env.ASSETS || typeof env.ASSETS.fetch !== "function") return notPublished();

  const assetUrl = new URL("/releases.json", url);
  const res = await env.ASSETS.fetch(new Request(assetUrl.toString(), { method: "GET" }));
  if (!res || res.status !== 200) return notPublished();

  // HEAD must not carry a body, but must still report success + headers.
  const body = request.method === "HEAD" ? null : res.body;
  return withCors(
    new Response(body, {
      status: 200,
      headers: {
        "content-type": "application/json; charset=utf-8",
        // 5 min: an update is not urgent to the second, and the node checks on
        // the order of hours.
        "cache-control": "public, max-age=300",
        "x-gpn-manifest": "release",
      },
    }),
  );
}

/** Largest JSON body we will read for an application. */
const JOIN_MAX_BODY = 8 * 1024;
/** Applications per IP per window. */
const JOIN_RATE_MAX = 5;
/** Rate-limit window, in seconds (also the KV TTL). */
const JOIN_RATE_WINDOW = 3600;
/** Per-field caps; mirrored by `maxlength` on the page. */
const JOIN_LIMITS = { name: 80, email: 200, role: 60, message: 4000 };

/**
 * Best-effort per-IP rate limit used when `JOIN_KV` is not bound. Resets with
 * the isolate, which is acceptable: it only exists to blunt a naive flood.
 * @type {Map<string, { count: number, reset: number }>}
 */
const joinRateMem = new Map();

/** @param {string} ip @returns {boolean} */
function joinRateOkMemory(ip) {
  const now = Date.now();
  const slot = joinRateMem.get(ip);
  if (!slot || slot.reset <= now) {
    joinRateMem.set(ip, { count: 1, reset: now + JOIN_RATE_WINDOW * 1000 });
    if (joinRateMem.size > 10000) {
      for (const [k, v] of joinRateMem) if (v.reset <= now) joinRateMem.delete(k);
    }
    return true;
  }
  if (slot.count >= JOIN_RATE_MAX) return false;
  slot.count += 1;
  return true;
}

/**
 * @param {{ get: Function, put: Function }} kv
 * @param {string} ip
 */
async function joinRateOkKv(kv, ip) {
  const key = `rl:${ip}`;
  const seen = Number(await kv.get(key)) || 0;
  if (seen >= JOIN_RATE_MAX) return false;
  await kv.put(key, String(seen + 1), { expirationTtl: JOIN_RATE_WINDOW });
  return true;
}

/** @param {unknown} v @returns {string} */
function joinStr(v) {
  return typeof v === "string" ? v.trim() : "";
}

/** @param {unknown} body @returns {string | null} */
function joinEmailError(body) {
  const email = joinStr(body.email);
  if (!email) return "An email address is required.";
  if (email.length > JOIN_LIMITS.email) return "That email address is too long.";
  if (!/^[^\s@]{1,64}@[^\s@]{1,190}\.[^\s@]{2,24}$/.test(email)) return "That email address doesn't look valid.";
  return null;
}

/**
 * Validate a parsed application body.
 * @param {any} body
 * @returns {{ ok: true, value: { name: string, email: string, role: string, message: string } } | { ok: false, error: string }}
 */
function validateJoin(body) {
  if (!body || typeof body !== "object" || Array.isArray(body)) {
    return { ok: false, error: "Expected a JSON object." };
  }
  const name = joinStr(body.name);
  if (name.length < 2) return { ok: false, error: "A name of at least 2 characters is required." };
  if (name.length > JOIN_LIMITS.name) return { ok: false, error: "That name is too long (80 characters max)." };

  const emailErr = joinEmailError(body);
  if (emailErr) return { ok: false, error: emailErr };

  const role = joinStr(body.role).slice(0, JOIN_LIMITS.role) || "unspecified";
  const message = joinStr(body.message);
  if (message.length > JOIN_LIMITS.message) {
    return { ok: false, error: "Please keep the message under 4000 characters." };
  }

  return { ok: true, value: { name, email: joinStr(body.email), role, message } };
}

/** @param {object} payload @param {number} [status] */
function joinJson(payload, status = 200) {
  return withCors(
    new Response(JSON.stringify(payload) + "\n", {
      status,
      headers: {
        "content-type": "application/json; charset=utf-8",
        "cache-control": "no-store",
      },
    }),
  );
}

/**
 * Accept a job application. Never throws, never returns non-JSON: the page
 * relies on one shape, and a failure here must not look like a sent form.
 *
 * @param {Request} request
 * @param {{ JOIN_KV?: { get: Function, put: Function } }} env
 */
async function handleJoin(request, env) {
  try {
    const declared = Number(request.headers.get("content-length") || 0);
    if (declared > JOIN_MAX_BODY) {
      return joinJson({ ok: false, error: "That submission is too large." });
    }

    let raw;
    try {
      raw = await request.text();
    } catch {
      return joinJson({ ok: false, error: "Could not read the request body." });
    }
    if (raw.length > JOIN_MAX_BODY) {
      return joinJson({ ok: false, error: "That submission is too large." });
    }

    let body;
    try {
      body = JSON.parse(raw || "{}");
    } catch {
      return joinJson({ ok: false, error: "Expected valid JSON." });
    }

    /* Honeypot: pretend it worked so a bot gets no signal, store nothing. */
    if (joinStr(body && body.website)) {
      return joinJson({ ok: true, stored: false, reference: "ignored" });
    }

    const checked = validateJoin(body);
    if (!checked.ok) return joinJson({ ok: false, error: checked.error });
    const app = checked.value;

    const ip = request.headers.get("cf-connecting-ip") || "unknown";
    const kv = env && env.JOIN_KV && typeof env.JOIN_KV.put === "function" ? env.JOIN_KV : null;

    const allowed = kv ? await joinRateOkKv(kv, ip) : joinRateOkMemory(ip);
    if (!allowed) {
      return joinJson({ ok: false, error: "Too many applications from this address. Try again in an hour, or email us." });
    }

    if (!kv) {
      /* Not a success: nothing was persisted. The page turns `stored:false`
         into a prefilled mailto draft so the application is not lost. */
      return joinJson({
        ok: true,
        stored: false,
        fallback: "email",
        error: "Applications are not being stored on this deployment (JOIN_KV is not bound). Please email hello@gpncoin.ai.",
      });
    }

    const now = new Date();
    const id = `${now.getTime().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
    const record = {
      id,
      received_at: now.toISOString(),
      name: app.name,
      email: app.email,
      role: app.role,
      message: app.message,
      ip,
      country: (request.cf && request.cf.country) || null,
      user_agent: (request.headers.get("user-agent") || "").slice(0, 300),
    };

    try {
      await kv.put(`app:${record.received_at}:${id}`, JSON.stringify(record));
    } catch {
      return joinJson({
        ok: true,
        stored: false,
        fallback: "email",
        error: "We couldn't save that just now. Please email hello@gpncoin.ai.",
      });
    }

    return joinJson({ ok: true, stored: true, reference: id });
  } catch (err) {
    return joinJson({
      ok: false,
      error: "Something went wrong on our side. Please email hello@gpncoin.ai.",
    });
  }
}

/** @param {Response} res */
function withCors(res) {
  const headers = new Headers(res.headers);
  headers.set("access-control-allow-origin", "*");
  headers.set("access-control-allow-methods", "GET, HEAD, POST, OPTIONS");
  headers.set("access-control-allow-headers", "content-type");
  headers.set("access-control-max-age", "86400");
  return new Response(res.body, { status: res.status, headers });
}

// Exported for unit tests (`node --test`). Not used by the Worker runtime.
export const __test = {
  buildDirectory,
  releaseManifest,
  handleJoin,
  validateJoin,
  joinRateOkMemory,
  JOIN_LIMITS,
  dialableMultiaddrs,
  multiaddrFromUrl,
  peerIdOf,
  sanitizeMultiaddr,
  sanitizePeerId,
  parseHttpBody,
  STATIC_BOOTNODES,
};
