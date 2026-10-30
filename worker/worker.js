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
 *   GET /api/health     proxy -> access point `GET /health`
 *   GET /api/v1/models  proxy -> `GET /v1/models` (OpenAI model list)
 *   GET /api/providers  proxy -> `GET /providers` (live capabilities)
 *   GET /api/mode       proxy -> `GET /mode`
 *   GET /api/jobs/<id>  proxy -> `GET /job/<id>`
 *   POST /api/v1/chat/completions  proxy -> OpenAI-compatible chat
 *
 * The `/api/*` proxies exist because the public testnet access point speaks
 * plain `http://` on port 8092: a page served from `https://gpncoin.ai` cannot
 * call that (mixed content), and Workers `fetch()` refuses non-standard ports.
 * The proxy terminates TLS at the edge and speaks raw HTTP/1.1 over
 * `cloudflare:sockets`, the same path `/seeds.json` uses for `/providers`.
 * `AP_JOB_URL` names the target and `AP_TOKEN` (a secret) supplies the
 * `Authorization: Bearer` the AP requires from non-loopback callers.
 *
 * `/api/join` accepts `{name, email, role, message}` as JSON, validates it,
 * rate-limits per IP, then proxies the application to the access point's
 * `POST /join`, which durably appends it to `~/.gpn/join-applications.jsonl`
 * and announces it in Discord. It **always** answers 200 with JSON so the page
 * can render one code path; `ok:false` carries a human-readable `error`.
 * If the AP is unreachable the route answers `ok:false` with a mailto fallback
 * address (`crewlyllc@gmail.com`) so a real application is never lost silently.
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
 * Timeout for the user-facing `/api/*` proxy. Chat completions are one job on
 * the chain: the AP waits for the `create` extrinsic to be included and for a
 * provider to run the prompt, which on this testnet is routinely 15-60 s and
 * occasionally >90 s. A short timeout here would turn a working network into a
 * spurious 504, so the proxy is deliberately patient.
 */
const AP_PROXY_TIMEOUT_MS = 110_000;

/** Largest JSON request body the proxy will forward (chat prompts). */
const AP_PROXY_MAX_BODY = 64 * 1024;

/** Refuse runaway upstream bodies (a chat answer is a few KB). */
const AP_PROXY_MAX_RESPONSE = 8 * 1024 * 1024;

/** Free testnet inference is expensive (one GPU job per call): keep it modest. */
const CHAT_RATE_MAX = 12;
const CHAT_RATE_WINDOW_SECONDS = 600;
/** Concurrent chat jobs this isolate will hold open. */
const CHAT_INFLIGHT_MAX = 8;
/** Fine-tunes per IP per window. A tune is GPU-minutes and a flat 50 GPN of
 * escrow, so it gets its own, tighter bucket than chat. */
const FINETUNE_RATE_MAX = 4;
/** Rate-limit window for fine-tune submissions, in seconds. */
const FINETUNE_RATE_WINDOW_SECONDS = 3600;
/** A fine-tune body carries up to 200 prompt/completion pairs, so it gets a
 * larger cap than chat (64 KB) — but still a bounded one. */
const FINETUNE_MAX_BODY = 256 * 1024;

/** Cheap read endpoints (models/health/providers). */
const READ_RATE_MAX = 120;
const READ_RATE_WINDOW_SECONDS = 60;

/**
 * Best-effort per-IP counters. They reset with the isolate; that is acceptable
 * for a free testnet endpoint — the point is to blunt a naive loop, not to be
 * an accounting system. If a KV namespace is ever bound, `rateLimit` uses it.
 * @type {Map<string, { count: number, reset: number }>}
 */
const apiRateMem = new Map();

/** Chat jobs currently held open by this isolate. */
let chatInflight = 0;

/** Origins allowed to call the POST proxy from a browser. */
const SITE_ORIGINS = new Set(["https://gpncoin.ai", "https://www.gpncoin.ai"]);

/** @param {string} origin */
function originAllowed(origin) {
  if (!origin) return false;
  if (SITE_ORIGINS.has(origin)) return true;
  try {
    const u = new URL(origin);
    return u.hostname === "localhost" || u.hostname === "127.0.0.1" || u.hostname === "[::1]";
  } catch {
    return false;
  }
}

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

    // Canonical chain spec over HTTPS. A fresh `--setup` fetches this instead
    // of plain HTTP from a home box; the AP's `/genesis-spec` is the upstream
    // (bootNodes rewritten there at request time). `/genesis-spec` is kept as
    // an alias so existing installers keep working during the transition.
    if (path === "/chain-spec" || path === "/genesis-spec") {
      if (request.method === "OPTIONS") return preflight();
      if (request.method !== "GET" && request.method !== "HEAD") {
        return withCors(
          new Response("method not allowed\n", {
            status: 405,
            headers: { allow: "GET, HEAD, OPTIONS" },
          }),
        );
      }
      return chainSpec(request, env);
    }

    if (path === "/api" || path.startsWith("/api/")) {
      return handleApi(request, env, path);
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
/**
 * Query the AP's provider registry. Returns `null` when the AP is not
 * configured or not reachable — callers must treat that as "static only".
 * @param {{ AP_URL?: string, AP_JOB_URL?: string }} env
 * @returns {Promise<Array<object> | null>}
 */
async function fetchLiveProviders(env) {
  const res = await apRequest(env, { method: "GET", path: "/providers", timeoutMs: AP_TIMEOUT_MS });
  if (!res || res.error || res.status !== 200) return null;
  try {
    const doc = JSON.parse(res.body);
    return doc && Array.isArray(doc.providers) ? doc.providers : null;
  } catch {
    return null;
  }
}

/**
 * The access-point base URL. `AP_JOB_URL` (the documented job/chat endpoint)
 * wins; `AP_URL` stays as the directory fallback so an existing deployment
 * keeps working with one var.
 * @param {{ AP_URL?: string, AP_JOB_URL?: string }} env
 */
function apBase(env) {
  return String((env && (env.AP_JOB_URL || env.AP_URL)) || "").trim().replace(/\/+$/, "");
}

/**
 * Perform one request against the access point and normalise the result.
 *
 * `fetch()` is used when the URL is HTTPS or on a standard port. The public AP
 * is plain HTTP on :8092, which `fetch()` refuses, so the fallback is a
 * minimal HTTP/1.1 exchange over `cloudflare:sockets` — the same mechanism the
 * peer directory already uses.
 *
 * Never throws. On failure returns `{ error: string }`; otherwise
 * `{ status, contentType, body }` with the upstream status preserved.
 *
 * @param {{ AP_JOB_URL?: string, AP_URL?: string, AP_TOKEN?: string, AP_FORCE_SOCKET?: string }} env
 * @param {{ method: string, path: string, body?: string | null, timeoutMs?: number }} opts
 */
async function apRequest(env, opts) {
  const binary = !!(opts && opts.binary);
  // Test seam (`node --test`): a harness may supply a transport instead of a
  // real socket. Worker env is configuration, never code, so production cannot
  // accidentally set this.
  if (env && typeof env.__test_ap_transport === "function") {
    return await env.__test_ap_transport({
      method: opts.method,
      path: opts.path,
      body: opts.body == null ? null : opts.body,
      binary,
    });
  }
  const base = apBase(env);
  if (!base) return { error: "access point is not configured (AP_JOB_URL)" };

  let url;
  try {
    url = new URL(base + opts.path);
  } catch {
    return { error: "access point URL is malformed" };
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    return { error: "access point URL must be http(s)" };
  }

  const timeoutMs = opts.timeoutMs || AP_PROXY_TIMEOUT_MS;
  const headers = {
    accept: "application/json, text/event-stream",
    "user-agent": "gpn-proxy/1",
  };
  if (opts.body != null) headers["content-type"] = "application/json";

  // The AP gate requires a bearer token for non-loopback POSTs. Without the
  // secret we still proxy: loopback callers and an AP with no token configured
  // both work, so a missing secret must not take the page down.
  const token = env && env.AP_TOKEN ? String(env.AP_TOKEN).trim() : "";
  if (token) headers.authorization = `Bearer ${token}`;
  else console.warn("[api] AP_TOKEN secret is not set; proxying without Authorization");
  if (opts && opts.headers) Object.assign(headers, opts.headers);

  const forceSocket = !!(env && env.AP_FORCE_SOCKET && String(env.AP_FORCE_SOCKET) !== "0");
  const standardPort =
    url.protocol === "https:"
      ? url.port === "" || url.port === "443"
      : url.port === "" || url.port === "80";

  if (!forceSocket && standardPort && typeof fetch === "function") {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), timeoutMs);
    try {
      const res = await fetch(url.toString(), {
        method: opts.method,
        headers,
        body: opts.body == null ? undefined : opts.body,
        signal: controller.signal,
        // Upstream is authoritative; never serve an edge-cached answer.
        cf: { cacheTtl: 0, cacheEverything: false },
      });
      if (binary) {
        const buf = new Uint8Array(await res.arrayBuffer());
        return {
          status: res.status,
          contentType: res.headers.get("content-type") || "application/octet-stream",
          body: buf,
          binary: true,
        };
      }
      const body = await res.text();
      return {
        status: res.status,
        contentType: res.headers.get("content-type") || "application/json",
        body,
      };
    } catch {
      /* fall through to the socket path */
    } finally {
      clearTimeout(timer);
    }
  }

  return socketHttpRequest(
    url,
    opts.method,
    headers,
    opts.body == null ? null : opts.body,
    timeoutMs,
    binary,
  );
}

/**
 * One HTTP/1.1 request over a raw Workers TCP socket. Plain `http://` only —
 * TLS is left to `fetch()`. Never throws.
 *
 * @param {URL} url
 * @param {string} method
 * @param {Record<string, string>} headers
 * @param {string | null} body
 * @param {number} timeoutMs
 */
async function socketHttpRequest(url, method, headers, body, timeoutMs, binary = false) {
  if (url.protocol !== "http:") return { error: "raw sockets only speak plain http" };
  const host = url.hostname.replace(/^\[|\]$/g, "");
  const port = Number(url.port || "80");
  if (!host || !Number.isInteger(port) || port < 1 || port > 65535) {
    return { error: "access point host/port is invalid" };
  }

  /** @type {{ connect?: Function }} */
  let sockets;
  try {
    sockets = await import("cloudflare:sockets");
  } catch {
    return { error: "cloudflare:sockets unavailable in this runtime" };
  }
  if (!sockets || typeof sockets.connect !== "function") {
    return { error: "cloudflare:sockets unavailable in this runtime" };
  }

  let socket;
  try {
    socket = sockets.connect({ hostname: host, port });
  } catch {
    return { error: "could not open a socket to the access point" };
  }

  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    try {
      socket.close();
    } catch {}
  }, timeoutMs);

  try {
    const bytes = new TextEncoder().encode(body == null ? "" : body);
    let head =
      `${method} ${url.pathname}${url.search} HTTP/1.1\r\n` +
      `Host: ${host}:${port}\r\n` +
      `User-Agent: ${headers["user-agent"] || "gpn-proxy/1"}\r\n` +
      `Accept: ${headers.accept || "application/json"}\r\n` +
      "Accept-Encoding: identity\r\n";
    if (body != null) head += `Content-Type: ${headers["content-type"] || "application/json"}\r\n`;
    if (headers.authorization) head += `Authorization: ${headers.authorization}\r\n`;
    head += `Content-Length: ${bytes.byteLength}\r\nConnection: close\r\n\r\n`;

    const writer = socket.writable.getWriter();
    await writer.write(new TextEncoder().encode(head));
    if (bytes.byteLength) await writer.write(bytes);
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
        if (total > AP_PROXY_MAX_RESPONSE) break;
      }
    }
    reader.releaseLock();

    if (timedOut) return { error: "the access point did not answer in time" };
    const parsed = parseHttpResponse(concatBytes(chunks), binary);
    if (!parsed) return { error: "the access point sent a malformed response" };
    return {
      status: parsed.status,
      contentType: parsed.headers["content-type"] || "application/json",
      body: parsed.body,
      binary: parsed.binary || false,
    };
  } catch {
    return {
      error: timedOut
        ? "the access point did not answer in time"
        : "could not complete the request to the access point",
    };
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
 * Parse a raw HTTP/1.1 response (Content-Length or chunked). Returns the
 * status, a lower-cased header map, and the decoded body, or `null` when the
 * bytes are not an HTTP response. Status codes are preserved, not filtered:
 * the `/api/*` proxy must pass a 400/401/451 from the AP through unchanged.
 *
 * @param {Uint8Array} raw
 * @returns {{ status: number, headers: Record<string, string>, body: string } | null}
 */
function parseHttpResponse(raw, binary = false) {
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
  const body = raw.subarray(split + 4);
  const lines = head.split("\r\n");
  const status = Number((lines.shift() || "").split(/\s+/)[1]);
  if (!Number.isFinite(status) || status < 100 || status > 599) return null;

  /** @type {Record<string, string>} */
  const headers = {};
  for (const line of lines) {
    const i = line.indexOf(":");
    if (i < 0) continue;
    const name = line.slice(0, i).trim().toLowerCase();
    const value = line.slice(i + 1).trim();
    headers[name] = headers[name] ? `${headers[name]}, ${value}` : value;
  }

  const chunked = String(headers["transfer-encoding"] || "").toLowerCase().includes("chunked");
  const length = headers["content-length"] != null ? Number(headers["content-length"]) : null;
  const decoder = new TextDecoder();

  if (!binary) {
    if (!chunked) {
      const end = length !== null && Number.isFinite(length) && length <= body.length ? length : body.length;
      return { status, headers, body: decoder.decode(body.subarray(0, end)) };
    }
    let out = "";
    let at = 0;
    for (;;) {
      const lineEnd = indexOfCrlf(body, at);
      if (lineEnd < 0) return { status, headers, body: out };
      const size = parseInt(decoder.decode(body.subarray(at, lineEnd)).split(";")[0].trim(), 16);
      if (!Number.isFinite(size) || size < 0) return null;
      at = lineEnd + 2;
      if (size === 0) return { status, headers, body: out };
      if (at + size > body.length) return null;
      out += decoder.decode(body.subarray(at, at + size));
      at += size;
      if (body[at] === 13 && body[at + 1] === 10) at += 2;
    }
  }

  // Binary (image artifacts): keep the bytes, never decode them as UTF-8.
  if (!chunked) {
    const end = length !== null && Number.isFinite(length) && length <= body.length ? length : body.length;
    return { status, headers, body: body.subarray(0, end), binary: true };
  }
  const parts = [];
  let total = 0;
  let at = 0;
  for (;;) {
    const lineEnd = indexOfCrlf(body, at);
    if (lineEnd < 0) return null;
    const size = parseInt(decoder.decode(body.subarray(at, lineEnd)).split(";")[0].trim(), 16);
    if (!Number.isFinite(size) || size < 0) return null;
    at = lineEnd + 2;
    if (size === 0) break;
    if (at + size > body.length) return null;
    parts.push(body.subarray(at, at + size));
    total += size;
    at += size;
    if (body[at] === 13 && body[at + 1] === 10) at += 2;
  }
  const out = new Uint8Array(total);
  let off = 0;
  for (const p of parts) {
    out.set(p, off);
    off += p.byteLength;
  }
  return { status, headers, body: out, binary: true };
}

/**
 * The 2xx-only body reader used by the peer directory (kept for its tests and
 * for callers that want "usable answer or nothing").
 * @param {Uint8Array} raw
 * @returns {string | null}
 */
function parseHttpBody(raw) {
  const parsed = parseHttpResponse(raw);
  if (!parsed) return null;
  return parsed.status >= 200 && parsed.status < 300 ? parsed.body : null;
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

/* ────────────────────────── /api/* AP proxy ────────────────────────── */

/**
 * JSON response for an API route, with browser-CORS headers.
 * @param {object} payload
 * @param {number} status
 * @param {string} origin
 * @param {string} method
 * @param {Record<string, string>} [extra]
 */
function apiJson(payload, status, origin, method, extra) {
  const headers = new Headers({
    "content-type": "application/json; charset=utf-8",
    "cache-control": "no-store",
  });
  if (extra) for (const [k, v] of Object.entries(extra)) headers.set(k, v);
  return apiCors(new Response(JSON.stringify(payload) + "\n", { status, headers }), origin, method);
}

/**
 * Add CORS to an API response. GET/HEAD are public (`*`); the POST chat proxy
 * echoes the site origin only, so another page cannot quietly spend the free
 * testnet inference from a visitor's browser.
 * @param {Response} res
 * @param {string} origin
 * @param {string} method  GET | HEAD | POST
 */
function apiCors(res, origin, method) {
  const headers = new Headers(res.headers);
  if (method === "POST") {
    if (originAllowed(origin)) {
      headers.set("access-control-allow-origin", origin);
      headers.set("vary", "origin");
    }
  } else {
    headers.set("access-control-allow-origin", "*");
  }
  headers.set("access-control-allow-methods", method === "POST" ? "POST, OPTIONS" : "GET, HEAD, OPTIONS");
  headers.set("access-control-allow-headers", "content-type, authorization");
  headers.set("access-control-max-age", "86400");
  headers.set("x-content-type-options", "nosniff");
  return new Response(res.body, { status: res.status, headers });
}

/**
 * @param {string} origin
 * @param {boolean} [allowPost] whether the path under CORS accepts POST
 */
function apiPreflight(origin, allowPost) {
  const headers = new Headers({
    "access-control-allow-methods": allowPost ? "POST, OPTIONS" : "GET, HEAD, POST, OPTIONS",
    "access-control-allow-headers": "content-type, authorization",
    "access-control-max-age": "86400",
    "access-control-allow-origin": allowPost ? (originAllowed(origin) ? origin : SITE_ORIGINS.values().next().value) : "*",
    vary: "origin",
  });
  return new Response(null, { status: 204, headers });
}

/**
 * Per-IP counter for the free endpoints. KV when bound, isolate memory
 * otherwise. A KV failure must never reject a legitimate request, so a broken
 * KV degrades to "allow".
 * @param {{ API_KV?: { get: Function, put: Function } }} env
 * @param {string} ip
 * @param {string} bucket
 * @param {number} max
 * @param {number} windowSeconds
 */
async function rateLimit(env, ip, bucket, max, windowSeconds) {
  const kv = env && env.API_KV && typeof env.API_KV.get === "function" ? env.API_KV : null;
  if (kv) {
    try {
      const key = `rl:api:${bucket}:${ip}`;
      const seen = Number(await kv.get(key)) || 0;
      if (seen >= max) return false;
      await kv.put(key, String(seen + 1), { expirationTtl: windowSeconds });
      return true;
    } catch {
      /* fall through to memory */
    }
  }

  const now = Date.now();
  const key = `${bucket}:${ip}`;
  const slot = apiRateMem.get(key);
  if (!slot || slot.reset <= now) {
    apiRateMem.set(key, { count: 1, reset: now + windowSeconds * 1000 });
    if (apiRateMem.size > 20000) {
      for (const [k, v] of apiRateMem) if (v.reset <= now) apiRateMem.delete(k);
    }
    return true;
  }
  if (slot.count >= max) return false;
  slot.count += 1;
  return true;
}

/**
 * The `/api/*` surface: a thin, status-preserving proxy to the access point.
 *
 *   GET  /api/health             -> GET  /health
 *   GET  /api/ping               -> GET  /health
 *   GET  /api/mode               -> GET  /mode
 *   GET  /api/providers          -> GET  /providers
 *   GET  /api/v1/models          -> GET  /v1/models
 *   GET  /api/jobs/<id>          -> GET  /job/<id>
 *   POST /api/v1/chat/completions -> POST /v1/chat/completions
 *   POST /api/v1/finetunes        -> POST /v1/finetunes (202 + id)
 *   GET  /api/v1/finetunes/<id>   -> GET  /v1/finetunes/<id> (poll)
 *   GET  /api/v1/finetunes/<id>/artifact -> GET /artifact/<id> (adapter zip)
 *
 * The AP's status code and body are passed through; only transport failures
 * become a 502 here.
 *
 * @param {Request} request
 * @param {object} env
 * @param {string} path
 */
async function handleApi(request, env, path) {
  const origin = request.headers.get("origin") || "";
  const method = request.method;
  const route = (path.slice(4) || "/").replace(/\/+$/, "") || "/";
  if (method === "OPTIONS") {
    const postRoute =
      route === "/v1/chat/completions" ||
      route === "/chat/completions" ||
      route === "/v1/images/generations" ||
      route === "/v1/finetunes";
    return apiPreflight(origin, postRoute);
  }

  const ip = request.headers.get("cf-connecting-ip") || "unknown";

  /** @type {string | null} */
  let upstream = null;
  let allow = "GET";
  let cache = "no-store";
  let binary = false;
  let finetune = false;

  if (route === "/health" || route === "/ping") upstream = "/health";
  else if (route === "/mode") upstream = "/mode";
  else if (route === "/providers") {
    upstream = "/providers";
    cache = "public, max-age=30";
  } else if (route === "/v1/models" || route === "/models") {
    upstream = "/v1/models";
    cache = "public, max-age=60";
  } else if (route === "/v1/chat/completions" || route === "/chat/completions") {
    upstream = "/v1/chat/completions";
    allow = "POST";
  } else if (route === "/v1/images/generations") {
    // OpenAI-compatible image generation: 202 + poll. The AP returns in
    // milliseconds; the render happens in the background.
    upstream = "/v1/images/generations";
    allow = "POST";
  } else if (/^\/v1\/images\/generations\/[0-9a-f]{32}$/.test(route)) {
    upstream = route; // poll: pending -> completed
  } else if (/^\/v1\/images\/[0-9a-f]{32}(\/\d{1,2})?$/.test(route)) {
    // The finished PNG. Binary: never decode/rewrite it as text.
    upstream = route;
    binary = true;
    cache = "public, max-age=86400";
  } else if (route === "/v1/finetunes") {
    // OpenAI-style fine-tune submit: 202 + id + poll_url. A LoRA tune is
    // GPU-minutes, so — like images — the AP answers immediately and the
    // training happens in the background.
    upstream = "/v1/finetunes";
    allow = "POST";
    finetune = true;
  } else if (/^\/v1\/finetunes\/\d{1,20}\/artifact$/.test(route)) {
    // The finished LoRA adapter (zip). Binary: never decode it as text.
    upstream = `/artifact/${route.split("/")[3]}`;
    binary = true;
    cache = "public, max-age=86400";
  } else if (/^\/v1\/finetunes\/\d{1,20}$/.test(route)) {
    // Poll: queued -> running -> succeeded | failed.
    upstream = route;
  } else {
    const m = /^\/(?:jobs?)\/(\d{1,20})$/.exec(route);
    if (m) upstream = `/job/${m[1]}`;
  }

  if (!upstream) {
    return apiJson(
      { error: { message: "unknown API route — see https://gpncoin.ai/ai", type: "gpn_not_found" } },
      404,
      origin,
      method === "POST" ? "POST" : "GET",
    );
  }

  const okMethod =
    allow === "POST" ? method === "POST" : method === "GET" || method === "HEAD";
  if (!okMethod) {
    return apiJson(
      { error: { message: `use ${allow} on this route`, type: "gpn_method_not_allowed" } },
      405,
      origin,
      allow === "POST" ? "POST" : "GET",
      { allow: `${allow}, OPTIONS` },
    );
  }

  const isChat = allow === "POST";
  const bucket = finetune ? "finetune" : isChat ? "chat" : "read";
  const max = finetune ? FINETUNE_RATE_MAX : isChat ? CHAT_RATE_MAX : READ_RATE_MAX;
  const windowSeconds = finetune
    ? FINETUNE_RATE_WINDOW_SECONDS
    : isChat
      ? CHAT_RATE_WINDOW_SECONDS
      : READ_RATE_WINDOW_SECONDS;
  if (!(await rateLimit(env, ip, bucket, max, windowSeconds))) {
    return apiJson(
      {
        error: {
          message: `rate limit reached (${max} per ${Math.round(windowSeconds / 60)} min). The testnet AP is free but each call is one GPU job — try again shortly.`,
          type: "gpn_rate_limited",
        },
      },
      429,
      origin,
      isChat ? "POST" : "GET",
      { "retry-after": String(windowSeconds) },
    );
  }

  /** @type {string | null} */
  let body = null;
  if (isChat) {
    const bodyCap = finetune ? FINETUNE_MAX_BODY : AP_PROXY_MAX_BODY;
    const declared = Number(request.headers.get("content-length") || 0);
    if (declared > bodyCap) {
      return apiJson(
        { error: { message: `request body too large (max ${bodyCap} bytes)`, type: "gpn_body_too_large" } },
        413,
        origin,
        "POST",
      );
    }
    try {
      body = await request.text();
    } catch {
      return apiJson({ error: { message: "could not read the request body", type: "gpn_bad_request" } }, 400, origin, "POST");
    }
    if (body.length > bodyCap) {
      return apiJson(
        { error: { message: `request body too large (max ${bodyCap} bytes)`, type: "gpn_body_too_large" } },
        413,
        origin,
        "POST",
      );
    }
    let parsed;
    try {
      parsed = JSON.parse(body || "{}");
    } catch {
      return apiJson({ error: { message: "expected a JSON body", type: "gpn_bad_request" } }, 400, origin, "POST");
    }
    // Refuse a request with no usable payload before it becomes an on-chain
    // job (the AP rejects it too, but a clear message here is cheaper).
    if (finetune) {
      // A tune needs a base model and a non-empty dataset of prompt/completion
      // pairs. The AP's parser is authoritative; this is only the cheap guard.
      const ds = parsed && typeof parsed === "object" ? parsed.dataset : null;
      const usable =
        parsed &&
        typeof parsed === "object" &&
        !Array.isArray(parsed) &&
        typeof parsed.model === "string" &&
        parsed.model.trim() !== "" &&
        ((Array.isArray(ds) && ds.length > 0) || typeof ds === "string");
      if (!usable) {
        return apiJson(
          {
            error: {
              message:
                'missing fine-tune fields: send {"model": "llama3.2:1b", "dataset": [{"prompt": "…", "completion": "…"}]}',
              type: "gpn_bad_request",
            },
          },
          400,
          origin,
          "POST",
        );
      }
    } else {
      const usable =
        parsed &&
        typeof parsed === "object" &&
        !Array.isArray(parsed) &&
        ((Array.isArray(parsed.messages) && parsed.messages.length > 0) ||
          typeof parsed.prompt === "string" ||
          typeof parsed.input === "string");
      if (!usable) {
        return apiJson(
          {
            error: {
              message: "missing prompt: send messages[] (OpenAI style) or a top-level prompt/input",
              type: "gpn_bad_request",
            },
          },
          400,
          origin,
          "POST",
        );
      }
    }
    if (chatInflight >= CHAT_INFLIGHT_MAX) {
      return apiJson(
        { error: { message: "the free endpoint is busy — retry in a few seconds", type: "gpn_busy" } },
        429,
        origin,
        "POST",
        { "retry-after": "10" },
      );
    }
  }

  if (isChat) chatInflight += 1;
  /** @type {any} */
  let res;
  try {
    res = await apRequest(env, {
      method: allow,
      path: upstream,
      body,
      binary,
      timeoutMs: AP_PROXY_TIMEOUT_MS,
    });
  } finally {
    if (isChat) chatInflight -= 1;
  }

  if (!res || res.error) {
    return apiJson(
      {
        error: {
          message: `access point unavailable: ${res && res.error ? res.error : "unknown error"}`,
          type: "gpn_proxy_error",
        },
      },
      502,
      origin,
      isChat ? "POST" : "GET",
    );
  }

  // A 401 here is OUR misconfiguration, not the caller's: the browser never
  // sends a token, the Worker does. Say so plainly instead of echoing an
  // `unauthorized` the visitor cannot act on.
  if (res.status === 401) {
    const configured = !!(env && env.AP_TOKEN && String(env.AP_TOKEN).trim());
    return apiJson(
      {
        error: {
          message: configured
            ? "the access point rejected the proxy's token — AP_TOKEN is set but not accepted (rotate it)"
            : "the AI proxy is not configured yet: the AP_TOKEN secret is missing on this Worker",
          type: "gpn_proxy_auth",
        },
      },
      502,
      origin,
      isChat ? "POST" : "GET",
    );
  }

  const headers = new Headers();
  headers.set(
    "content-type",
    res.contentType || (binary ? "image/png" : "application/json; charset=utf-8"),
  );
  headers.set("cache-control", cache);
  headers.set("x-gpn-proxy", "access-point");
  // HEAD must not carry a body.
  const outBody = method === "HEAD" ? null : res.body;
  return apiCors(
    new Response(outBody, { status: res.status, headers }),
    origin,
    isChat ? "POST" : "GET",
  );
}

/** Largest JSON body we will read for an application. */
const JOIN_MAX_BODY = 8 * 1024;
/** Applications per IP per window. The AP enforces its own 5/hour too. */
const JOIN_RATE_MAX = 5;
/** Rate-limit window, in seconds. */
const JOIN_RATE_WINDOW = 3600;
/** Per-field caps; mirrored by `maxlength` on the page. */
const JOIN_LIMITS = { name: 80, email: 200, role: 60, message: 4000 };
/**
 * Where a failed application can be emailed instead. Shown ONLY when the AP is
 * unreachable — never on success (a successful submission is delivered to
 * Discord, so there is nothing for the applicant to do).
 */
const JOIN_FALLBACK_EMAIL = "crewlyllc@gmail.com";
/** The message the page shows when no application could be stored. */
const JOIN_UNAVAILABLE = "applications are temporarily unavailable";

/**
 * Best-effort per-IP rate limit. Resets with the isolate, which is acceptable:
 * it only exists to blunt a naive flood.
 * @type {Map<string, { count: number, reset: number }>}
 */
const joinRateMem = new Map();

/**
 * @param {string} ip
 * @param {number} [now]
 */
function joinRateOkMemory(ip, now = Date.now()) {
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
 * The AP is the durable store (JSONL ledger + Discord). Validation, the
 * honeypot and the per-IP limit stay here so a bad request never reaches it.
 * If the AP cannot be reached the response carries the mailto fallback — the
 * page only shows that address on failure.
 *
 * @param {Request} request
 * @param {{ AP_JOB_URL?: string, AP_TOKEN?: string, AP_FORCE_SOCKET?: string }} env
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
    if (!joinRateOkMemory(ip)) {
      return joinJson({
        ok: false,
        error: "Too many applications from this address. Try again in an hour.",
        fallback: "email",
        email: JOIN_FALLBACK_EMAIL,
      });
    }

    /* The AP owns the durable store. `X-Forwarded-For` carries the applicant's
       IP so the AP's own per-IP limit keys on the person, not on this Worker's
       egress address (the AP only honours it on the token-gated POST). */
    const upstream = await apRequest(env, {
      method: "POST",
      path: "/join",
      body: JSON.stringify(app),
      headers: { "x-forwarded-for": ip },
      timeoutMs: AP_TIMEOUT_MS,
    });

    if (!upstream || upstream.error) {
      console.warn(`[join] access point unreachable: ${upstream && upstream.error}`);
      return joinJson({
        ok: false,
        stored: false,
        error: JOIN_UNAVAILABLE,
        fallback: "email",
        email: JOIN_FALLBACK_EMAIL,
      });
    }

    let stored;
    try {
      stored = JSON.parse(upstream.body);
    } catch {
      stored = null;
    }
    if (!stored || typeof stored !== "object") {
      console.warn(`[join] access point answered ${upstream.status} with a non-JSON body`);
      return joinJson({
        ok: false,
        stored: false,
        error: JOIN_UNAVAILABLE,
        fallback: "email",
        email: JOIN_FALLBACK_EMAIL,
      });
    }

    /* Validation / rate-limit / store failures keep the AP's message (they are
       already written for an applicant); anything else is treated the same as
       an unreachable AP so the page renders one fallback. */
    if (stored.ok !== true) {
      if (upstream.status === 400 || upstream.status === 429) {
        return joinJson({
          ok: false,
          error: String(stored.error || "That application could not be accepted."),
          fallback: "email",
          email: JOIN_FALLBACK_EMAIL,
        });
      }
      return joinJson({
        ok: false,
        stored: false,
        error: JOIN_UNAVAILABLE,
        fallback: "email",
        email: JOIN_FALLBACK_EMAIL,
      });
    }

    return joinJson({
      ok: true,
      stored: stored.stored === true,
      delivered: stored.delivered === true,
      reference: stored.reference || null,
    });
  } catch (err) {
    console.warn(`[join] unexpected failure: ${err && err.message}`);
    return joinJson({
      ok: false,
      stored: false,
      error: JOIN_UNAVAILABLE,
      fallback: "email",
      email: JOIN_FALLBACK_EMAIL,
    });
  }
}

/**
 * Proxy the canonical chain spec from the access point.
 *
 * The installer's `--setup` now fetches `https://gpncoin.ai/chain-spec`; the
 * AP serves it (with `bootNodes` rewritten from the canonical list) at
 * `/genesis-spec`. Failures are JSON, never an HTML error page, because the
 * installer parses the body as JSON.
 *
 * @param {Request} request
 * @param {{ AP_JOB_URL?: string, AP_URL?: string, AP_TOKEN?: string, AP_FORCE_SOCKET?: string }} env
 */
async function chainSpec(request, env) {
  const upstream = await apRequest(env, { method: "GET", path: "/genesis-spec" });
  if (upstream.error) {
    return withCors(
      new Response(JSON.stringify({ error: `chain spec unavailable: ${upstream.error}` }) + "\n", {
        status: 503,
        headers: {
          "content-type": "application/json; charset=utf-8",
          "cache-control": "no-store",
        },
      }),
    );
  }
  const status = upstream.status || 200;
  const headers = {
    "content-type": "application/json; charset=utf-8",
    "cache-control": "public, max-age=300",
    "x-gpn-spec-source": "access-point",
  };
  if (status === 200) {
    headers["content-disposition"] = 'attachment; filename="gpn-testnet-spec.json"';
  }
  const body = request.method === "HEAD" ? null : upstream.body;
  return withCors(new Response(body, { status, headers }));
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
  JOIN_FALLBACK_EMAIL,
  JOIN_UNAVAILABLE,
  dialableMultiaddrs,
  multiaddrFromUrl,
  peerIdOf,
  sanitizeMultiaddr,
  sanitizePeerId,
  parseHttpBody,
  parseHttpResponse,
  handleApi,
  chainSpec,
  apRequest,
  rateLimit,
  originAllowed,
  AP_PROXY_MAX_BODY,
  CHAT_RATE_MAX,
  FINETUNE_RATE_MAX,
  FINETUNE_RATE_WINDOW_SECONDS,
  FINETUNE_MAX_BODY,
  STATIC_BOOTNODES,
};
