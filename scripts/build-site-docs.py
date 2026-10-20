#!/usr/bin/env python3
"""Render docs/*.md into static HTML under website/docs/.

The site itself has NO build step at serve time: this script is run by a human
(or CI) when docs change, and the generated HTML is committed. Cloudflare Pages
serves website/ as-is.

Usage:  /usr/bin/python3 scripts/build-site-docs.py
"""
import datetime
import pathlib
import re

import markdown

ROOT = pathlib.Path(__file__).resolve().parent.parent
DOCS = ROOT / "docs"
OUT = ROOT / "website" / "docs"
SITE = "https://gpncoin.ai"
TODAY = datetime.date.today().isoformat()

# slug -> (source markdown, nav title, description, notice)
PAGES = {
    "run-a-node": (
        "run-a-node.md",
        "Run a node",
        "One command runs a GPN node: chain node/miner plus the AI serving stack. "
        "Covers install, config, ports, dialability (port-forward / UPnP / CGNAT) and troubleshooting.",
        None,
    ),
    "whitepaper": (
        "GPN-WHITEPAPER.md",
        "Whitepaper",
        "GPN whitepaper v0.9 (draft): a GPU-native network for mining and AI compute.",
        "Whitepaper v0.9 is a draft. Section 3 still describes the earlier "
        "mined-or-served three-mode model; today a node is always miner + AI provider. "
        "See <a href='run-a-node.html'>Run a node</a> for current behavior.",
    ),
    "technical-overview": (
        "GPN-TECHNICAL-OVERVIEW.md",
        "Technical overview",
        "A short technical summary of GPN: node model, mining, AI marketplace, escrow and settlement.",
        None,
    ),
    "one-pager": (
        "GPN-ONE-PAGER.md",
        "One-pager",
        "GPN in one page: the problem, the fix, how it works, and the token.",
        None,
    ),
    "wallet": (
        "wallet.md",
        "Wallet",
        "gpn_wallet: a node-free sr25519 wallet for holding and moving GPN.",
        None,
    ),
    "fast-sync": (
        "fast-sync.md",
        "Fast sync",
        "Snapshots, checkpoints, and why a fresh GPN node re-verifies Ethash seals.",
        None,
    ),
    "peer-discovery": (
        "peer-discovery.md",
        "Peer discovery",
        "How a GPN node finds peers: static bootnodes, seed lists, DNS seeds and the security model.",
        None,
    ),
    "downloads": (
        "downloads.md",
        "Downloads",
        "Rolling GitHub releases for Windows, Linux and macOS.",
        None,
    ),
    "mainnet-launch-runbook": (
        "mainnet-launch-runbook.md",
        "Mainnet launch runbook",
        "Operational checklist for the mainnet launch: genesis, keys, bootstrap and go/no-go.",
        "Mainnet has not launched. This is the internal runbook, published for transparency; it is not a launch announcement.",
    ),
    "design-assume-valid-checkpoint": (
        "design/design-assume-valid-checkpoint.md",
        "Design: assume-valid checkpoint",
        "Design note on the fast-sync checkpoint mechanism.",
        None,
    ),
}

# markdown source path -> generated slug (for links that don't map 1:1)
LINK_MAP = {
    "design/design-assume-valid-checkpoint.md": "design-assume-valid-checkpoint.html",
}

NAV = [
    ("../index.html", "Home", "index"),
    ("../download.html", "Download", "download"),
    ("../deploy.html", "Deploy", "deploy"),
    ("../ai.html", "AI", "ai"),
    ("../access-points.html", "Access Points", "access-points"),
    ("../nodes.html", "Nodes", "nodes"),
    ("../buy.html", "Buy GPN", "buy"),
    ("index.html", "Docs", "docs"),
]

HEAD = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1" />
<title>{title} — GPN docs</title>
<meta name="description" content="{desc}" />
<meta name="theme-color" content="#07090f" />
<link rel="canonical" href="{site}/docs/{slug}.html" />
<meta property="og:site_name" content="GPN" />
<meta property="og:type" content="article" />
<meta property="og:url" content="{site}/docs/{slug}.html" />
<meta property="og:title" content="{title} — GPN docs" />
<meta property="og:description" content="{desc}" />
<meta name="twitter:card" content="summary" />
<meta name="twitter:title" content="{title} — GPN docs" />
<meta name="twitter:description" content="{desc}" />
<link rel="icon" href="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'%3E%3Crect width='32' height='32' rx='7' fill='%237c6cff'/%3E%3Ctext x='16' y='22' font-size='16' font-family='sans-serif' font-weight='700' text-anchor='middle' fill='%2307090f'%3EG%3C/text%3E%3C/svg%3E" />
<link rel="stylesheet" href="../assets/style.css" />
<link rel="stylesheet" href="../assets/docs.css" />
</head>
<body>
<a class="skip-link" href="#main">Skip to content</a>

<header class="site-header">
  <div class="wrap nav">
    <a class="brand" href="../index.html" aria-label="GPN home"><span class="dot" aria-hidden="true">G</span><span>GPN<br><small>GPU-NATIVE NETWORK</small></span></a>
    <button class="nav-toggle" id="nav-toggle" aria-expanded="false" aria-controls="nav-links" aria-label="Toggle navigation">☰</button>
    <nav class="nav-links" id="nav-links" aria-label="Primary">
      {nav}
    </nav>
    <div class="nav-actions"><button class="theme-toggle" id="theme-toggle" aria-pressed="false" title="Switch theme">☾</button></div>
  </div>
</header>
"""

FOOT = """
<footer class="site-footer">
  <div class="wrap">
    <div class="foot-grid">
      <div><a class="brand" href="../index.html" style="margin-bottom:10px"><span class="dot" aria-hidden="true">G</span> <span>GPN</span></a><p class="muted" style="max-width:320px">A GPU-native network for mining and AI compute. <em>Get paid for the GPU time you're wasting.</em></p></div>
      <div><h4>Product</h4><a href="../download.html">Download</a><a href="../deploy.html">Deploy</a><a href="../ai.html">Access the AI</a><a href="../access-points.html">Access Points</a><a href="../nodes.html">Live nodes</a></div>
      <div><h4>Learn</h4><a href="index.html">Docs</a><a href="whitepaper.html">Whitepaper</a><a href="one-pager.html">One-pager</a><a href="technical-overview.html">Technical overview</a></div>
      <div><h4>Project</h4><a href="https://github.com/jeremyzguo/gpn_coin" rel="noopener">Source on GitHub</a><a href="run-a-node.html">Run a node</a><a href="wallet.html">Wallet</a><a href="../buy.html">Token &amp; purchasing</a></div>
    </div>
    <div class="foot-bottom">
      <p class="small muted">© <span data-year>2026</span> GPN · Public testnet. Not investment advice.</p>
      <p class="small muted">Testnet only — tokens here have no monetary value.</p>
    </div>
  </div>
</footer>

<script src="../assets/app.js"></script>
</body>
</html>
"""


def nav_html(active):
    out = []
    for href, label, key in NAV:
        cur = ' aria-current="page"' if key == active else ""
        out.append(f'<a href="{href}"{cur}>{label}</a>')
    return "\n      ".join(out)


def dedent_list_fences(text):
    """Fenced blocks indented 1-3 spaces inside list items are not recognised by
    python-markdown. Dedent them to column 0 so they render as real code blocks."""
    lines = text.splitlines()
    out = []
    in_fence = False
    indent = 0
    for line in lines:
        m = re.match(r"^(\s{1,3})(```.*)$", line)
        if m and not in_fence:
            in_fence = True
            indent = len(m.group(1))
            out.append(m.group(2))
            continue
        if in_fence:
            out.append(line[indent:] if line.startswith(" " * indent) else line)
            if line.strip().startswith("```"):
                in_fence = False
            continue
        out.append(line)
    return "\n".join(out)


def md_to_html(text):
    md = markdown.Markdown(
        extensions=["toc", "tables", "fenced_code", "sane_lists", "attr_list"],
        extension_configs={"toc": {"permalink": False, "toc_depth": "2-3"}},
    )
    body = md.convert(dedent_list_fences(text))
    toc = md.toc
    return body, toc


def rewrite_links(html):
    """Point intra-docs .md links at their generated .html pages."""
    def repl(m):
        target = m.group(1)
        if re.match(r"^[a-z]+:", target) or target.startswith("#"):
            return m.group(0)
        if target in LINK_MAP:
            return f'href="{LINK_MAP[target]}"'
        if target.endswith(".md") or ".md#" in target:
            target = target.replace(".md#", ".html#").replace(".md", ".html")
        return f'href="{target}"'
    return re.sub(r'href="([^"]+)"', repl, html)


def split_title(text, fallback):
    lines = text.splitlines()
    for i, line in enumerate(lines):
        if line.startswith("# "):
            title = line[2:].strip()
            rest = lines[:i] + lines[i + 1:]
            return title, "\n".join(rest)
    return fallback, text


def write_page(slug, src, title, desc, notice):
    text = (DOCS / src).read_text(encoding="utf-8")
    page_title, body_md = split_title(text, title)
    body, toc = md_to_html(body_md)
    body = rewrite_links(body)
    if toc:
        toc = rewrite_links(toc)

    notice_html = ""
    if notice:
        notice_html = f'<div class="doc-notice" role="note">{notice}</div>\n'

    html = HEAD.format(title=title, desc=desc, site=SITE, slug=slug, nav=nav_html("docs"))
    html += f"""
<main id="main" class="doc-main">
  <div class="wrap doc-wrap">
    <nav class="crumbs" aria-label="Breadcrumb"><a href="../index.html">Home</a> / <a href="index.html">Docs</a> / <span>{title}</span></nav>
    {notice_html}<article class="prose">
<h1>{page_title}</h1>
{body}
    </article>
    <p class="doc-updated small muted">Last generated {TODAY} from the project documentation set.</p>
  </div>
</main>
"""
    html += FOOT
    (OUT / f"{slug}.html").write_text(html, encoding="utf-8")
    print(f"wrote website/docs/{slug}.html ({len(html)} bytes)")


def write_index():
    cards = []
    meta = {
        "run-a-node": ("Start here", "Install and run a node, make it dialable, fix common failures."),
        "downloads": ("Downloads", "Rolling Windows / Linux / macOS release builds."),
        "technical-overview": ("Technical overview", "Node model, mining, marketplace, escrow in a few pages."),
        "whitepaper": ("Whitepaper", "The full design paper (v0.9 draft)."),
        "one-pager": ("One-pager", "The whole thesis on one page."),
        "wallet": ("Wallet", "Hold and move GPN without running a node."),
        "fast-sync": ("Fast sync", "Snapshots, checkpoints and sync times."),
        "peer-discovery": ("Peer discovery", "Bootnodes, seed lists and DNS seeds."),
        "mainnet-launch-runbook": ("Mainnet launch runbook", "Operational go/no-go checklist (internal, published for transparency)."),
    }
    for slug, (label, blurb) in meta.items():
        _src, _title, _desc, _notice = PAGES[slug]
        cards.append(
            f'<a class="doc-card" href="{slug}.html"><b>{label}</b><span>{blurb}</span></a>'
        )

    html = HEAD.format(
        title="Documentation",
        desc="GPN documentation: run a node, downloads, technical overview, whitepaper, wallet and peer discovery.",
        site=SITE,
        slug="index",
        nav=nav_html("docs"),
    )
    html = html.replace(f"{SITE}/docs/index.html", f"{SITE}/docs/")
    html += f"""
<main id="main" class="doc-main">
  <div class="wrap doc-wrap">
    <span class="kicker">Docs</span>
    <h1>GPN documentation</h1>
    <p class="lede">Guides for operators and users. These pages are generated from the project's Markdown
      docs and served statically here on <b>gpncoin.ai</b>.</p>

    <div class="doc-notice" role="note"><b>Status:</b> the docs are being migrated to this domain. Pages
      listed below are published; guides not listed here are still only in the source repository, which is
      private today (access on request). If a page looks stale, the repository is the source of truth.</div>

    <div class="doc-grid">
      {''.join(cards)}
    </div>

    <h2>Quick links</h2>
    <ul>
      <li><a href="run-a-node.html#make-your-node-dialable">Make your node dialable</a> — port-forwarding, UPnP, CGNAT.</li>
      <li><a href="../download.html">Download &amp; install</a> — one-command installers and release artifacts.</li>
      <li><a href="../deploy.html">Deploy</a> — node, access point, Windows path, config and ports.</li>
      <li><a href="../ai.html">Use the AI marketplace</a> — OpenAI-compatible chat and the job API.</li>
    </ul>

    <p class="doc-updated small muted">Last generated {TODAY}.</p>
  </div>
</main>
"""
    html += FOOT
    (OUT / "index.html").write_text(html, encoding="utf-8")
    print(f"wrote website/docs/index.html ({len(html)} bytes)")


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for slug, (src, title, desc, notice) in PAGES.items():
        write_page(slug, src, title, desc, notice)
    write_index()


if __name__ == "__main__":
    main()
