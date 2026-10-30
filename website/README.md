# GPN — website

A **no-build static site** for GPN, served at **https://gpncoin.ai** (Cloudflare Pages,
output directory = `website/`). Hand-written HTML/CSS/JS, no bundler, no external CDNs,
no build step at serve time.

## Pages

| File | What it covers |
|---|---|
| `index.html` | Home — the problem, the model (every node is miner + AI provider), a **copy-and-run quickstart** with OS tabs (Windows / Linux / macOS-soon), how it works, positioning, features, token summary, and the **Join the team** section with an application form (`#join`). |
| `download.html` | One-command installers, rolling release artifacts (`windows-latest` / `linux-latest` / `macos-latest`), prerequisites. Downloads come from the public mirror `crewlyllc/gpn_coin` (no token needed). |
| `deploy.html` | Step-by-step: run a node (mine + serve), how serving works, an access point, the Windows path, config and ports. |
| `buy.html` | Honest status: testnet today (faucet / mining / job earnings), the mainnet listing plan, fair launch + treasury/vesting + halving, disclaimer. |
| `ai.html` | The AI marketplace: OpenAI-compatible chat + job API, models, pricing, and a **“Try it”** widget (see the mixed-content note). |
| `access-points.html` | Known access points and the three public testnet nodes (1 Linux bootnode + 2 Windows full nodes). |
| `nodes.html` | **Live node directory** — fetches `/seeds.json` client-side and renders the network's own directory (seeds + live providers, multiaddrs, peer IDs, endpoints), auto-refreshing every 60 s. Canonical URL is `/nodes`. |
| `404.html` | Static not-found page (Cloudflare Pages serves it automatically per directory; root copy). |
| `docs/` | **Generated** static docs pages — see below. |
| `assets/style.css` | Shared styles. Dark-first, light-friendly (theme toggle), responsive, print-friendly. |
| `assets/docs.css` | Prose styles for the docs pages. |
| `assets/app.js` | Theme toggle, mobile nav, footer year, copy buttons, accessible OS tabs (`role=tablist`, arrow keys, remembered in `sessionStorage`), the AI “Try it” widget, and the join form. |
| `assets/og.png` | 1200×630 OpenGraph/Twitter card. |
| `robots.txt`, `sitemap.xml` | Indexing metadata for the canonical domain. |

## Docs (`website/docs/`)

The docs pages are generated from the project's Markdown (`docs/*.md`) and **committed as
plain HTML** — there is no runtime build. When a source doc changes, regenerate:

```bash
/usr/bin/python3 scripts/build-site-docs.py
```

Then commit the changed files under `website/docs/`. The generator rewrites intra-doc
`.md` links to `.html` and strips a level of indentation from fenced blocks nested in list
items (python-markdown otherwise leaves them as literal backticks).

## Preview locally

```bash
python3 -m http.server 8080 --directory website
# then visit http://localhost:8080/
```

Serve it over `http://` (not `file://`) if you want the AI widget to make requests.

## Notes for editors

- **Keep it self-contained.** No external `<script>`/`<link>` to CDNs; inline SVG/data-URI art
  only. This keeps it offline-friendly and CSP-friendly.
- **Nav + footer are repeated** in each page (no templating layer). If you change a link,
  update all pages plus `scripts/build-site-docs.py` (which has its own copy for docs pages) —
  search for `nav-links` and `foot-grid`.
- **Canonical domain is `https://gpncoin.ai`.** Every page carries a canonical link plus
  OG/Twitter tags pointing at that domain. Don't reintroduce `gpn.network`.
- **No secrets.** Never place tokens, seeds, or wallet material in these files. The AI widget
  has an optional token field on purpose — it is typed by the visitor at runtime and is never
  stored or hardcoded.
- **Mixed content:** the “Try it” widget calls a plain-`http://` testnet access point. On the
  HTTPS production site browsers block that request, so `ai.html` tells visitors to use the
  `curl` example instead. This is expected until an HTTPS access point exists.
- **Testnet framing** is intentional and repeated (banner on `buy.html`/`access-points.html`,
  a footer note everywhere). Keep it that way.
- **Downloads are public.** `download.html` and the home quickstart point at the anonymous mirror
  `crewlyllc/gpn_coin` (read-only copy, code + release assets). Development happens in the private
  `jeremyzguo/gpn_coin`.

## `POST /api/join` (home-page applications)

`worker/worker.js` owns this route; `/api/*` is in `assets.run_worker_first` so the request reaches
applications before the asset layer (a POST to a path with no asset is otherwise answered with a
bodyless 405).

- Body: `{name, email, role, message}` — lengths are capped (80 / 200 / 60 / 4000), the honeypot
  field `website` must stay empty, and the route answers **200 JSON** in every case
  (`ok:false` + `error` for a rejected submission, so the page can render one code path).
- Rate limit: 5 applications per IP per hour (`cf-connecting-ip`), in-isolate `Map`.
- Delivery: the Worker proxies the validated application to the access point's `POST /join`
  (`AP_JOB_URL` + `AP_TOKEN`). The AP appends it to `~/.gpn/join-applications.jsonl` on the AP host
  and posts a Discord embed to `GPN_JOIN_WEBHOOK` (see `~/.gpn/join-webhook.env`). Store first,
  deliver second: a Discord outage never loses an application.
- Failure: if the AP is unreachable the route answers
  `{ok:false, stored:false, error:"applications are temporarily unavailable", fallback:"email",
  email:"crewlyllc@gmail.com"}` and the page shows a prefilled mailto draft. That address is never
  shown on success.

Reading applications on the AP host:

```bash
wc -l ~/.gpn/join-applications.jsonl
tail -1 ~/.gpn/join-applications.jsonl | jq .
```

Tests: `node --test worker/worker.test.mjs` (includes 8 `/api/join` cases with a mock AP transport).

## Deploy

Cloudflare Pages: connect the repo, set **root directory** to the repository root and
**build output directory** to `website`, with **no build command** (the site is already
static). Nothing else is required; `_headers`/`_redirects` are not used.
