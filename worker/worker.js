/**
 * gpncoin.ai — static site + live peer directory (DDNS-style).
 *
 * Routes owned by this Worker (see `.assets.run_worker_first` in wrangler.jsonc):
 *
 *   GET /seeds.json     canonical network directory (JSON)
 *   GET /bootnodes.txt  the same dialable multiaddrs, newline separated
 *   GET /seeds.txt      alias of /bootnodes.txt
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
 * @param {Array<object> | null} providers
 */
function buildDirectory(providers) {
  /** @type {Array<{multiaddr: string, peer_id?: string, role: string, source: string, best?: boolean}>} */
  const nodes = [];
  const multiaddrs = [];
  const seen = new Set();

  for (const ma of STATIC_BOOTNODES) {
    if (typeof ma !== "string" || !ma.startsWith("/")) continue;
    if (seen.has(ma)) continue;
    seen.add(ma);
    multiaddrs.push(ma);
    nodes.push({ multiaddr: ma, peer_id: peerIdOf(ma), role: "seed", source: "compiled-in" });
  }

  /** Endpoints that are live but cannot be dialed as libp2p peers yet. */
  const endpoints = [];
  let live = 0;
  if (Array.isArray(providers)) {
    for (const p of providers) {
      if (!p || p.healthy === false) continue;
      const statusUrl = typeof p.status_url === "string" ? p.status_url : "";
      if (!statusUrl) continue;
      const ma = multiaddrFromUrl(statusUrl);
      const entry = {
        endpoint: statusUrl,
        multiaddr: ma || null,
        peer_id: ma ? peerIdOf(ma) : null,
        role: "provider",
        source: "ap",
        address: typeof p.address === "string" ? p.address : null,
        best: false,
      };
      live += 1;
      // Only a multiaddr carrying a /p2p/<peer-id> is dialable by gpn-node.
      if (ma && ma.includes("/p2p/")) {
        if (!seen.has(ma)) {
          seen.add(ma);
          multiaddrs.push(ma);
          nodes.push(entry);
        }
      } else {
        endpoints.push(entry);
      }
    }
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

/** @param {Response} res */
function withCors(res) {
  const headers = new Headers(res.headers);
  headers.set("access-control-allow-origin", "*");
  headers.set("access-control-allow-methods", "GET, HEAD, OPTIONS");
  headers.set("access-control-allow-headers", "content-type");
  headers.set("access-control-max-age", "86400");
  return new Response(res.body, { status: res.status, headers });
}

// Exported for unit tests (`node --test`). Not used by the Worker runtime.
export const __test = { buildDirectory, multiaddrFromUrl, peerIdOf, parseHttpBody, STATIC_BOOTNODES };
