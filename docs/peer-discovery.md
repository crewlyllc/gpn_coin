# GPN Peer Discovery

How a GPN node finds peers at startup — Bitcoin-style. This documents the seed
sources, the seed-list wire format, how to host a seed, and the security model.

Historically a GPN node had exactly one way to find peers: a static
`--bootnodes <multiaddr>` passed on the command line (or baked into the chain
spec). That is a single point of failure. Discovery now layers four independent
sources on top of it.

> Implementation: `gpn-node/node/src/discovery.rs` (parsing / DNS / HTTP /
> persistence) and `gpn-node/node/src/service.rs` (`prepare_network_config`,
> the persistent-peer-store task and the on-chain discovery task).

## Sources (all merged, deduplicated, best-effort)

At startup the effective bootnode set is the **union** of:

1. **Static bootnodes** — `--bootnodes <multiaddr>` / chain-spec bootnodes /
   the compiled-in `network.json` bootnodes. Kept first as the hard fallback.
2. **Persistent peer store (addrman)** — `<base-path>/gpn-peers.json`. The node
   periodically snapshots the addresses of peers it is *connected to* and
   re-dials them on the next start.
3. **DNS seed** — `--bootnodes-dns <name>` (repeatable).
4. **HTTP seed list** — `--bootnodes-url <url>` (repeatable). **Defaults to the
   live network directory at <https://gpncoin.ai/seeds.json>** — no flag needed.

Sources are resolved independently. **A source that is unreachable, malformed or
slow is logged and skipped — it never aborts startup.** Only entries that parse
as a dialable libp2p multiaddress (i.e. end in `/p2p/<peer-id>`) survive the
merge.

### CLI flags

```
--bootnodes <MULTIADDR>...        # static (pre-existing), e.g. the hardcoded fallback
--bootnodes-dns <NAME>...         # resolve NAME's DNS TXT records as seeds
--bootnodes-url <URL>...          # fetch a published seed-list JSON
                                  # default: https://gpncoin.ai/seeds.json
```

`--bootnodes-url` **replaces** the default directory when given (pass several to
union them); the compiled-in `--bootnodes` always remain as the last-resort
fallback. The supervisor exposes the same setting as `bootnodes_url` in
`node.toml` and as `GPN_BOOTNODES_URL`.

### On-chain discovery (GPN-native top-up)

Once the node has at least one connection, a background task reads two on-chain
registries via the node's own chain client and dials the endpoints it finds:

- `Liveness::Endpoints` — the endpoint each registered full node declared
  (`gpn-liveness::register_fullnode`), and
- `ComputeMarketplace::ProviderEndpoints` — the endpoint each registered
  compute provider published.

Each endpoint is converted to a multiaddress (a libp2p multiaddr is used as-is;
an `http(s)://host:port`, `ws(s)://…` or bare `host:port` becomes
`/dns4|ip4|ip6/…/tcp/<port>`). Only endpoints that carry a peer id are dialable,
so provider HTTP endpoints (which have none) are recorded but not dialed —
best-effort by design. This runs after bootstrap deliberately: a brand-new node
with zero peers has no way to trust on-chain data yet, so it must first reach
the network through a static / DNS / URL / persisted peer, then top up. Startup
is never blocked on it.

## Seed-list wire format

A seed source yields a list of libp2p multiaddresses, each carrying the peer id
in a trailing `/p2p/<peer-id>` component. JSON payloads may be **either** an
object with a `multiaddrs` array:

```json
{
  "multiaddrs": [
    "/dns4/node1.example.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo",
    "/ip4/203.0.113.7/tcp/30334/p2p/12D3KooW9ye8kXwMYT4vBJoJdCkM9VfRrcM7nGgqVDPq3Fy8i6xw"
  ]
}
```

**or** a plain array:

```json
["/dns4/node1.example.org/tcp/30333/p2p/12D3Koo…", "/ip4/203.0.113.7/tcp/30334/p2p/12D3Koo…"]
```

Non-JSON bodies (and each DNS **TXT** record) are also accepted as
newline-separated multiaddr strings — one multiaddr per line; blank lines and
`#` comments are ignored:

```
# GPN seed list — example.org
/dns4/node1.example.org/tcp/30333/p2p/12D3KooWAFt…
/ip4/203.0.113.7/tcp/30334/p2p/12D3KooW9ye…
```

The peer id after `/p2p/` is what lets a client dial safely: it is verified
during the libp2p handshake, so a wrong or malicious address simply fails to
connect — it cannot impersonate a node.

The **persistent peer store** (`gpn-peers.json`) uses the same object form, so it
is itself a valid seed source you can inspect, copy or hand to another node.

## The network directory (`https://gpncoin.ai/seeds.json`)

The default seed source is a Cloudflare Worker in this repo
(`worker/worker.js`, deployed as the `gpn-coin` Worker that also serves this
site). It is the GPN equivalent of a DDNS name for the whole network: nodes ask
one stable hostname "who is up right now" instead of every operator tracking
every address.

**Endpoints**

| URL | Body |
|---|---|
| `GET https://gpncoin.ai/seeds.json` | JSON (the shape below) |
| `GET https://gpncoin.ai/bootnodes.txt` | the dialable multiaddrs, one per line |
| `GET https://gpncoin.ai/seeds.txt` | alias of `/bootnodes.txt` |

**Response**

```json
{
  "network": "GPN Public Testnet",
  "chain_id": "gpn_testnet",
  "genesis": "0xb8e4636a65d0c812abb437c38dd7e937bd7c3355c54c54d63060f26a33519a5c",
  "updated_at": "2026-10-19T22:00:00.000Z",
  "updated": 1760990400,
  "ttl": 60,
  "source": "static+ap",
  "live_providers": 1,
  "multiaddrs": [
    "/dns4/gpn-testnet.duckdns.org/tcp/30333/p2p/12D3KooWAFtUNxc6RgC9rq5uSHXd5XUncEcPqp77UU98oN6hSVFo"
  ],
  "nodes": [
    { "multiaddr": "…", "peer_id": "12D3Koo…", "role": "seed", "source": "compiled-in" },
    { "endpoint": "http://203.0.113.7:8093/status", "multiaddr": "/ip4/203.0.113.7/tcp/8093",
      "peer_id": null, "role": "provider", "source": "ap", "address": "5ENq…" }
  ],
  "publisher": "gpncoin.ai"
}
```

- `multiaddrs` is what `gpn-node` consumes; the rest is metadata for humans and
tooling. `source` is `static+ap`, `static+ap-empty`, or `static-fallback` (the
AP was unreachable) — the endpoint **never** returns 5xx for a discovery
failure.
- `genesis`/`chain_id` let a client reject a list for the wrong network
without parsing every address.
- `nodes[]` also carries providers that only publish an HTTP `status_url`: they
have no `/p2p/<peer-id>`, so they appear for tooling but are **not** added to
`multiaddrs` (the node cannot dial a peer without a peer id).

**Where the live half comes from.** The Worker fetches `GET /providers` from
the access point (wrangler var `AP_URL`), keeps entries the AP reports as
`healthy`, converts each `status_url` to a multiaddr, and merges. The result is
cached in-isolate for 60 s and served with `Cache-Control: public, max-age=60`.

The AP listens on a non-standard port (`:8092`), which the Workers `fetch()`
API refuses. The Worker therefore tries `fetch()` first (so an HTTPS ingress
always wins and is the recommended production setup — e.g. a Cloudflare Tunnel
mapping `https://ap.gpncoin.ai` → `127.0.0.1:8092`) and falls back to a raw
HTTP/1.1 GET over `cloudflare:sockets`, which can reach any port. That fallback
is what keeps the directory live today, with no extra infrastructure.

> **Deployment.** The Worker and the site are one deployment: pushing to `main`
> rebuilds it and `https://gpncoin.ai/seeds.json` goes live (the custom domain
> is already attached). Before that build lands — or on a fork without the
> domain — the same endpoint is on the `workers.dev` hostname of the `gpn-coin`
> Worker. Confirm with `curl -s https://gpncoin.ai/seeds.json | head`;
> `"source": "static+ap"` means the live merge worked, `static-fallback` means
> the AP was unreachable (the list is still valid).

## Hosting a seed

Any HTTP host works (static file, object store, small service):

1. Publish the JSON at a stable URL, e.g. `https://gpncoin.ai/seeds.json`.
2. Ensure it is reachable from the machines that run GPN nodes.
3. Operators add it with `--bootnodes-url https://…` — or just point at the
   default directory, which is already wired in.

A **DNS seed** is a hostname whose **TXT records** contain seed lists:

```
$ dig +short TXT seed.example.org
"\"/ip4/203.0.113.7/tcp/30333/p2p/12D3KooW9ye…\""
```

Regenerate it from your node inventory whenever peers change (a DNS provider
with an API, or a tiny cron job, both work).

> **TLS:** `https://` seed sources are fetched natively (rustls + webpki roots —
> already in the node's dependency graph). `http://` sources still use the
> HTTP-only client. Both are best-effort and time-bounded; no TLS stack is
> required on the host.

## Security model

- **Run several independent seeds.** A single DNS seed or a single seed URL is a
  centralization *and* censorship point: whoever controls it can eclipse the
  node from the network. Operate at least two independent DNS seeds and two
  independent HTTP seed sources, ideally on separate infrastructure/providers.
- **Keep the compiled-in bootnode as the hardcoded fallback.** Pass it via
  `--bootnodes` (or bake it into `network.json`); never rely solely on remote
  seeds.
- **Peer identity is content-addressed.** Because every seed entry ends in
  `/p2p/<peer-id>` and libp2p verifies that id during the handshake, an attacker
  who tampers with a seed list cannot impersonate an existing node — they can at
  worst redirect you to nodes they control, which is why several independent
  seeds matter.
- **Seed list poisoning is limited.** Invalid entries are dropped; duplicate
  entries are deduplicated; the effective set is bounded by the persisted-store
  cap.
- **Genesis still rules.** Discovery does not change the network identity model:
  a node is on a network iff its genesis matches. Connecting to a peer on a
  different network is harmless (it is rejected during the handshake).
