# GPN — website

A **no-build static site** for GPN, served at **https://gpncoin.ai** (Cloudflare Pages,
output directory = `website/`). Hand-written HTML/CSS/JS, no bundler, no external CDNs,
no build step at serve time.

## Pages

| File | What it covers |
|---|---|
| `index.html` | Home — the problem, the model (every node is miner + AI provider), how it works, positioning, features, token summary. |
| `download.html` | One-command installers, rolling release artifacts (`windows-latest` / `linux-latest` / `macos-latest`), prerequisites. Notes that the repo is private today. |
| `deploy.html` | Step-by-step: run a node (mine + serve), how serving works, an access point, the Windows path, config and ports. |
| `buy.html` | Honest status: testnet today (faucet / mining / job earnings), the mainnet listing plan, fair launch + treasury/vesting + halving, disclaimer. |
| `ai.html` | The AI marketplace: OpenAI-compatible chat + job API, models, pricing, and a **“Try it”** widget (see the mixed-content note). |
| `access-points.html` | Known access points and the three public testnet nodes (1 Linux bootnode + 2 Windows full nodes). |
| `nodes.html` | **Live node directory** — fetches `/seeds.json` client-side and renders the network's own directory (seeds + live providers, multiaddrs, peer IDs, endpoints), auto-refreshing every 60 s. Canonical URL is `/nodes`. |
| `404.html` | Static not-found page (Cloudflare Pages serves it automatically per directory; root copy). |
| `docs/` | **Generated** static docs pages — see below. |
| `assets/style.css` | Shared styles. Dark-first, light-friendly (theme toggle), responsive, print-friendly. |
| `assets/docs.css` | Prose styles for the docs pages. |
| `assets/app.js` | Theme toggle, mobile nav, footer year, copy buttons, and the AI “Try it” widget. |
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
- **Downloads are private today.** `download.html` links to the rolling GitHub releases but
  warns that the repository requires access.

## Deploy

Cloudflare Pages: connect the repo, set **root directory** to the repository root and
**build output directory** to `website`, with **no build command** (the site is already
static). Nothing else is required; `_headers`/`_redirects` are not used.
