import markdown, pathlib, re, datetime

base = pathlib.Path("/home/jeremyg/shared_workspace/artifacts/project7")
md = (base / "GPN-WHITEPAPER.md").read_text()

# Strip the leading H1 + the metadata line; we render our own title page.
lines = md.splitlines()
# find the first '##' (Abstract) to split title block from body
body_start = next(i for i, l in enumerate(lines) if l.startswith("## "))
body_md = "\n".join(lines[body_start:])

html_body = markdown.markdown(
    body_md,
    extensions=["toc", "tables", "fenced_code", "sane_lists", "attr_list"],
)

template = """<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>GPN — A GPU-Native Network for Mining and AI Compute</title>
<style>
  :root { --ink:#111014; --mut:#5b5b66; --accent:#6d4bff; --line:#e6e4ee; --bg:#ffffff; --panel:#f7f6fb; }
  * { box-sizing: border-box; }
  html { -webkit-text-size-adjust: 100%; }
  body { margin:0; color:var(--ink); background:var(--bg);
         font: 16px/1.65 ui-serif, Georgia, "Times New Roman", serif; }
  .wrap { max-width: 820px; margin: 0 auto; padding: 0 28px 96px; }
  /* Title page */
  .title { min-height: 92vh; display:flex; flex-direction:column; justify-content:center;
           border-bottom: 1px solid var(--line); }
  .title .kicker { font: 600 13px/1 ui-sans-serif, system-ui, sans-serif; letter-spacing:.18em;
                   text-transform:uppercase; color:var(--accent); }
  .title h1 { font-size: 44px; line-height:1.1; margin:18px 0 10px; letter-spacing:-.01em; }
  .title .sub { font-size: 20px; color:var(--mut); margin:0 0 30px; }
  .title .meta { font: 14px/1.7 ui-sans-serif, system-ui, sans-serif; color:var(--mut); }
  .title .rule { width:64px; height:4px; background:var(--accent); border-radius:2px; margin:26px 0; }
  /* Body */
  h1,h2,h3 { font-family: ui-sans-serif, system-ui, -apple-system, Segoe UI, Roboto, sans-serif;
             letter-spacing:-.01em; line-height:1.25; }
  h2 { font-size: 26px; margin: 44px 0 12px; padding-top: 14px; border-top:1px solid var(--line); }
  h3 { font-size: 19px; margin: 28px 0 8px; color:#2a2833; }
  p, li { color:#1c1b22; }
  a { color: var(--accent); text-decoration: none; }
  a:hover { text-decoration: underline; }
  code { font: 13.5px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace; background:var(--panel);
         padding:1px 6px; border-radius:6px; }
  blockquote { margin: 18px 0; padding: 10px 18px; border-left: 4px solid var(--accent);
               background: var(--panel); color:#333; border-radius: 0 8px 8px 0; }
  table { border-collapse: collapse; width:100%; margin: 18px 0; font-size: 14.5px; }
  th, td { border:1px solid var(--line); padding:8px 12px; text-align:left; vertical-align:top; }
  th { background: var(--panel); font-family: ui-sans-serif, system-ui, sans-serif; }
  hr { border:0; border-top:1px solid var(--line); margin: 34px 0; }
  /* ToC */
  .toc { background: var(--panel); border:1px solid var(--line); border-radius:12px; padding:18px 22px; margin: 30px 0 10px; }
  .toc::before { content:"Contents"; display:block; font: 700 13px/1 ui-sans-serif, system-ui, sans-serif;
                 letter-spacing:.16em; text-transform:uppercase; color:var(--mut); margin-bottom:10px; }
  .toc ul { margin:0; padding-left: 18px; }
  .toc li { margin: 3px 0; }
  @media print {
    .title { min-height: auto; padding: 40px 0; }
    h2 { page-break-after: avoid; }
    body { font-size: 12pt; }
  }
</style></head>
<body><div class="wrap">
  <section class="title">
    <div class="kicker">GPN Network · Whitepaper</div>
    <h1>A GPU-Native Network for Mining&nbsp;and&nbsp;AI&nbsp;Compute</h1>
    <div class="rule"></div>
    <p class="sub">One GPU that mines <em>or</em> serves AI — auto-switching to whichever pays more.</p>
    <div class="meta">Version 0.9 (draft) &nbsp;·&nbsp; 2026-10-09<br>Fair-launch, proof-of-work, GPU-only, trust-minimal escrow.</div>
  </section>
  <div class="toc">{toc}</div>
  {body}
</div></body></html>
"""

# Build ToC from the H2 headings in the rendered HTML
import html as _html
heads = re.findall(r'<h2[^>]*>(.*?)</h2>', html_body)
toc_items = "".join(f"<li>{h}</li>" for h in heads)

out = template.replace("{toc}", f"<ul>{toc_items}</ul>").replace("{body}", html_body)
(base / "GPN-WHITEPAPER.html").write_text(out)
print("wrote", base / "GPN-WHITEPAPER.html", len(out), "bytes;", len(heads), "sections")
