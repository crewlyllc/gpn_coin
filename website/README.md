# GPN — website

A **no-build static site** for GPN (Project 7). Hand-written HTML/CSS/JS, no bundler, no
external CDNs, no build step. Open `index.html` directly, or serve the folder with any
static server.

## Pages

| File | What it covers |
|---|---|
| `index.html` | Home — the reasoning: the problem, GPN's answer, the 3-mode diagram, how it works, positioning vs. other networks, features, token summary. |
| `download.html` | One-command installers (`install.sh`, `install-accesspoint.sh`), release artifacts, prerequisites, repo links. |
| `deploy.html` | Step-by-step: run a miner/node, a provider (LLM/diffusion/fine-tune), and an access point. Windows provider path, config/env, ports. |
| `buy.html` | Honest status: testnet today (faucet / mining / job earnings), the mainnet listing plan, fair launch + treasury/vesting + halving, disclaimer. |
| `ai.html` | The AI marketplace: OpenAI-compatible chat + job API, models, pricing, and a working **“Try it”** widget against a live testnet access point. |
| `access-points.html` | Reference list of known access points (seeded with the current testnet entry point) and how to run your own. |
| `assets/style.css` | Shared styles. Dark-first, light-friendly (theme toggle), responsive, print-friendly. |
| `assets/app.js` | Theme toggle, mobile nav, footer year, copy buttons, and the AI “Try it” widget. |

## Preview locally

Open the file directly:

```bash
xdg-open index.html        # or: open index.html on macOS
```

Or serve it (recommended — the AI widget needs `http(s)://`, not `file://`, to make
same-origin calls):

```bash
python3 -m http.server 8080 --directory .
# then visit http://localhost:8080/
```

The existing `dapp`/fleet tooling also serves a static dir on **:8080**; you can point that
at this folder instead if you prefer. Any static host works (nginx, Caddy, GitHub Pages,
S3, etc.) — copy the folder as-is.

## Notes for editors

- **Keep it self-contained.** No external `<script>`/`<link>` to CDNs; inline SVG/data-URI art
  only. This keeps it offline-friendly and CSP-friendly.
- **Nav + footer are repeated** in each page (there is no templating layer). If you change a
  link, update all six pages — search for `foot-grid` and `nav-links`.
- **No secrets.** Never place tokens, seeds, or wallet material in these files. The AI widget
  has an optional token field on purpose — it is typed by the visitor at runtime and is never
  stored or hardcoded.
- **The whitepaper links** are relative (`../GPN-WHITEPAPER.html`, `../GPN-WHITEPAPER.md`) so
  the site works when the `website/` folder sits alongside those files, as it does in the
  project artifacts directory.
- **CORS / mixed content:** the “Try it” widget calls `http://gpn-testnet.duckdns.org:8092`
  from the browser. If the access point doesn't send permissive CORS headers, the browser
  blocks the response — the page falls back to a clear message and a copy-paste `curl`
  example. A page served over `https://` cannot call a plain `http://` host.
- **Testnet framing** is intentional and repeated (banner on `buy.html`/`access-points.html`,
  a footer note everywhere). Keep it that way.

## Deploy to a host

The site is just files. To serve on a box:

```bash
# example: serve this folder on :8080
cd website && python3 -m http.server 8080
```

For a permanent host, put the folder behind nginx/Caddy and set the MIME types normally
(they're all standard: `.html`, `.css`, `.js`, `.svg`).
