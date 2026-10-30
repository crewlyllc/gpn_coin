/* Tests for the browser chat client — the pure half of assets/chat.js.
 *
 * Run with:  node website/tests/chat.test.js
 * (no dependencies, no build — plain node:assert, same style as referral.test.js)
 *
 * What is under test is the security boundary and the wire format:
 *   - every character of untrusted text is escaped BEFORE any tag is emitted
 *   - only http(s) links become anchors (`javascript:` / `data:` never do)
 *   - the SSE parser survives arbitrary chunk boundaries and junk lines
 *   - conversations are capped so localStorage cannot grow without bound
 *   - API failures become the plain-language copy the page actually shows
 */
"use strict";

const assert = require("assert");
const fs = require("fs");
const path = require("path");

const C = require("../assets/chat.js");
const HERE = __dirname;
const SITE = path.join(HERE, "..");

let passed = 0;
function test(name, fn) {
  fn();
  passed++;
  console.log("ok - " + name);
}

/* ============================================================
   Escaping
   ============================================================ */

test("escapeHtml escapes every HTML-significant character", () => {
  assert.strictEqual(C.escapeHtml("<script>"), "&lt;script&gt;");
  assert.strictEqual(C.escapeHtml('a "b" & \'c\''), "a &quot;b&quot; &amp; &#39;c&#39;");
  assert.strictEqual(C.escapeHtml(null), "");
  assert.strictEqual(C.escapeHtml(undefined), "");
  assert.strictEqual(C.escapeHtml(42), "42");
});

/* ============================================================
   Markdown renderer
   ============================================================ */

test("a <script> tag survives as text, never as an element", () => {
  const html = C.renderMarkdown("<script>alert(1)</script>");
  assert.ok(!/<script/i.test(html), "must not emit a script element: " + html);
  assert.ok(!/<img/i.test(html));
  assert.ok(html.includes("&lt;script&gt;alert(1)&lt;/script&gt;"));
});

test("event-handler payloads are inert", () => {
  const html = C.renderMarkdown('<img src=x onerror="alert(1)"> and <b onclick="x">bold</b>');
  assert.ok(!/<img/i.test(html));
  assert.ok(!/<b\s/i.test(html));
  assert.ok(html.includes("&lt;img"));
  assert.ok(html.includes("&quot;"));
});

test("javascript: links are rejected and rendered literally", () => {
  const html = C.renderMarkdown("[click me](javascript:alert(1))");
  assert.ok(!/<a /i.test(html), "no anchor may be emitted: " + html);
  assert.ok(html.includes("javascript:alert(1)"));
});

test("data: links are rejected", () => {
  const html = C.renderMarkdown("[x](data:text/html;base64,PHNjcmlwdD4=)");
  assert.ok(!/<a /i.test(html), html);
});

test("http(s) links become anchors with rel=noopener noreferrer", () => {
  const html = C.renderMarkdown("see [the docs](https://gpncoin.ai/ai) and [http](http://example.com/x?y=1)");
  assert.strictEqual((html.match(/<a /g) || []).length, 2, html);
  assert.ok(html.includes('href="https://gpncoin.ai/ai"'));
  assert.ok(html.includes('rel="noopener noreferrer"'));
  assert.ok(html.includes('target="_blank"'));
});

test("a quote in a link URL cannot break out of the attribute", () => {
  const html = C.renderMarkdown('[x](https://e.com/"onmouseover="alert(1))');
  assert.ok(!/onmouseover="/.test(html), html);
});

test("nested code fences stay inside one code block", () => {
  const src = "````\n```\ninner fence\n```\n````";
  const html = C.renderMarkdown(src);
  assert.strictEqual((html.match(/<pre>/g) || []).length, 1, html);
  assert.ok(html.includes("```\ninner fence\n```"), html);
});

test("an unterminated fence still renders (no infinite loop)", () => {
  const html = C.renderMarkdown("```js\nconst a = 1;");
  assert.strictEqual((html.match(/<pre>/g) || []).length, 1);
  assert.ok(html.includes("const a = 1;"));
  assert.ok(html.includes('class="lang-js"'));
});

test("code blocks are escaped, not parsed", () => {
  const html = C.renderMarkdown("```html\n<b>hi</b> & *x*\n```");
  assert.ok(html.includes("&lt;b&gt;hi&lt;/b&gt; &amp; *x*"), html);
  assert.ok(!/<b>/.test(html));
});

test("the fence language class is sanitised", () => {
  const html = C.renderMarkdown('```js" onload="x\ncode\n```');
  assert.ok(!/onload=/.test(html), html);
});

test("inline markdown: code, bold, italic, links", () => {
  /* renderInline() takes ALREADY-ESCAPED text (renderMarkdown escapes first) */
  assert.strictEqual(C.renderInline(C.escapeHtml("use `x < y` here")), "use <code>x &lt; y</code> here");
  assert.strictEqual(C.renderInline("**bold**"), "<strong>bold</strong>");
  assert.strictEqual(C.renderInline("*italic*"), "<em>italic</em>");
  assert.strictEqual(C.renderInline("snake_case_name"), "snake_case_name");
  assert.strictEqual(C.renderInline("2 * 3 * 4"), "2 * 3 * 4");
});

test("headings, lists and rules", () => {
  assert.strictEqual(C.renderMarkdown("## Hi"), "<h2>Hi</h2>");
  assert.ok(C.renderMarkdown("- one\n- two").startsWith("<ul><li>one</li><li>two</li></ul>"));
  assert.ok(C.renderMarkdown("1. one\n2. two").startsWith("<ol><li>one</li><li>two</li></ol>"));
  assert.strictEqual(C.renderMarkdown("---"), "<hr>");
});

test("a single newline becomes <br>, a blank line becomes a new paragraph", () => {
  const one = C.renderMarkdown("a\nb");
  assert.strictEqual(one, "<p>a<br>b</p>");
  const two = C.renderMarkdown("a\n\nb");
  assert.strictEqual((two.match(/<p>/g) || []).length, 2, two);
});

test("renderMarkdown never returns null or throws on odd input", () => {
  assert.strictEqual(C.renderMarkdown(""), "");
  assert.strictEqual(C.renderMarkdown(null), "");
  assert.strictEqual(C.renderMarkdown(undefined), "");
  C.renderMarkdown("````\nunterminated");
  C.renderMarkdown("[](  )");
  C.renderMarkdown("[[[[(");
});

/* ============================================================
   SSE parser
   ============================================================ */

function collect(parser, chunks) {
  const out = [];
  chunks.forEach((chunk) => { out.push(...parser.push(chunk)); });
  out.push(...parser.flush());
  return out;
}

test("parses one event split across an arbitrary boundary", () => {
  const p = new C.SSEParser();
  const events = collect(p, ['data: {"choices":[{"delta":{"content":"He', 'llo"}}]}\n\n']);
  assert.strictEqual(events.length, 1);
  assert.strictEqual(events[0].type, "data");
  assert.strictEqual(C.chunkText(events[0].data), "Hello");
});

test("parses several events in one chunk", () => {
  const p = new C.SSEParser();
  const body = 'data: {"choices":[{"delta":{"content":"a"}}]}\n\n' +
               'data: {"choices":[{"delta":{"content":"b"}}]}\n\n';
  const events = collect(p, [body]);
  assert.strictEqual(events.length, 2);
  assert.strictEqual(C.chunkText(events[0].data) + C.chunkText(events[1].data), "ab");
});

test("[DONE] terminates the stream and is not fed to JSON.parse", () => {
  const p = new C.SSEParser();
  const events = collect(p, ['data: {"choices":[{"delta":{"content":"x"}}]}\n\n', "data: [DONE]\n\n"]);
  assert.deepStrictEqual(events.map((e) => e.type), ["data", "done"]);
});

test("malformed JSON is reported, never thrown", () => {
  const p = new C.SSEParser();
  const events = collect(p, ["data: not-json\n\n", "data: {\"ok\":1}\n\n"]);
  assert.strictEqual(events[0].type, "error");
  assert.strictEqual(events[0].raw, "not-json");
  assert.strictEqual(events[1].type, "data");
});

test("CRLF line endings, comments and blank keep-alives are handled", () => {
  const p = new C.SSEParser();
  const events = collect(p, [": ping\r\n\r\n", 'data: {"choices":[]}\r\n\r\n']);
  assert.strictEqual(events.length, 1);
  assert.strictEqual(events[0].type, "data");
});

test("multi-line data fields are joined with a newline", () => {
  const p = new C.SSEParser();
  const events = collect(p, ['data: {"a":\ndata: 1}\n\n']);
  assert.strictEqual(events.length, 1);
  assert.strictEqual(events[0].type, "data");
  assert.strictEqual(events[0].data.a, 1);
});

test("flush() drains a trailing event with no blank line", () => {
  const p = new C.SSEParser();
  p.push('data: {"choices":[{"delta":{"content":"tail"}}]}');
  const events = p.flush();
  assert.strictEqual(events.length, 1);
  assert.strictEqual(C.chunkText(events[0].data), "tail");
});

test("chunkText understands delta, message and text replies", () => {
  assert.strictEqual(C.chunkText({ choices: [{ delta: { content: "d" } }] }), "d");
  assert.strictEqual(C.chunkText({ choices: [{ message: { content: "m" } }] }), "m");
  assert.strictEqual(C.chunkText({ choices: [{ text: "t" }] }), "t");
  assert.strictEqual(C.chunkText({}), "");
  assert.strictEqual(C.chunkText(null), "");
});

/* ============================================================
   Conversation caps
   ============================================================ */

test("pruneConversations keeps the 30 newest, newest first", () => {
  const list = [];
  for (let i = 0; i < 40; i++) {
    list.push({ id: "c" + i, updatedAt: i, messages: [] });
  }
  const out = C.pruneConversations(list);
  assert.strictEqual(out.length, C.MAX_CONVERSATIONS);
  assert.strictEqual(out[0].id, "c39");
  assert.strictEqual(out[out.length - 1].id, "c10");
});

test("pruneConversations caps each conversation at 200 messages, keeping the latest", () => {
  const messages = [];
  for (let i = 0; i < 250; i++) messages.push({ role: "user", content: "m" + i });
  const out = C.pruneConversations([{ id: "x", updatedAt: 1, messages }]);
  assert.strictEqual(out[0].messages.length, C.MAX_MESSAGES);
  assert.strictEqual(out[0].messages[0].content, "m50");
  assert.strictEqual(out[0].messages[199].content, "m249");
});

test("pruneConversations does not mutate its input", () => {
  const messages = new Array(250).fill(0).map((_, i) => ({ role: "user", content: "m" + i }));
  const conv = { id: "x", updatedAt: 1, messages };
  C.pruneConversations([conv]);
  assert.strictEqual(conv.messages.length, 250);
});

test("pruneConversations tolerates junk", () => {
  assert.deepStrictEqual(C.pruneConversations(null), []);
  assert.deepStrictEqual(C.pruneConversations([null, 3, "x"]), []);
  const out = C.pruneConversations([{ id: "a", updatedAt: 1 }]);
  assert.deepStrictEqual(out[0].messages, []);
});

test("deriveTitle flattens whitespace and truncates", () => {
  assert.strictEqual(C.deriveTitle("  hello\n\n world  "), "hello world");
  assert.strictEqual(C.deriveTitle(""), "New chat");
  const long = C.deriveTitle("x".repeat(100));
  assert.ok(long.length <= 48, long);
  assert.ok(long.endsWith("\u2026"));
});

test("conversationToMarkdown round-trips roles and text", () => {
  const md = C.conversationToMarkdown({
    title: "T",
    model: "m",
    messages: [
      { role: "user", content: "hi" },
      { role: "assistant", content: "hello" }
    ]
  });
  assert.ok(md.startsWith("# T"));
  assert.ok(md.includes("## You\n\nhi"));
  assert.ok(md.includes("## Assistant\n\nhello"));
});

/* ============================================================
   Error copy
   ============================================================ */

test("429 says 12 chats per 10 minutes", () => {
  const msg = C.errorMessage(429, "");
  assert.ok(msg.includes("12"), msg);
  assert.ok(/10 minutes/.test(msg), msg);
});

test("503 talks about providers/registry warming", () => {
  const msg = C.errorMessage(503, "");
  assert.ok(/provider/i.test(msg), msg);
  assert.ok(/warming/i.test(msg), msg);
});

test("502 blames the Worker-to-access-point hop", () => {
  const msg = C.errorMessage(502, "");
  assert.ok(/Worker/i.test(msg), msg);
});

test("400 surfaces the API's own message", () => {
  const body = JSON.stringify({ error: { message: "model 'nope' is not available", type: "x" } });
  const msg = C.errorMessage(400, body);
  assert.ok(msg.includes("model 'nope' is not available"), msg);
});

test("an error body of HTML never leaks markup into the message", () => {
  const msg = C.errorMessage(500, "<!doctype html><html><body>oops</body></html>");
  assert.ok(!/</.test(msg), msg);
});

test("unknown statuses still produce a readable sentence", () => {
  assert.ok(C.errorMessage(418, "").length > 10);
  assert.ok(C.errorMessage(500, "").includes("500"));
});

test("timeout copy mentions the 120 s budget and keeps the partial answer", () => {
  const msg = C.timeoutMessage();
  assert.ok(msg.includes("120"), msg);
  assert.ok(/partial/i.test(msg), msg);
});

/* ============================================================
   Model labels (the default is the smallest, and it says so)
   ============================================================ */

test("the smallest model is labelled as the demo default", () => {
  assert.ok(C.modelLabel("qwen2.5:0.5b").includes("fastest, best for demos"), C.modelLabel("qwen2.5:0.5b"));
  assert.ok(C.modelHint("qwen2.5:0.5b").includes("fastest"));
});

test("larger models carry an honest latency note, not a bigger-is-better pitch", () => {
  assert.ok(C.modelLabel("qwen2.5:7b").includes("slower to first token"), C.modelLabel("qwen2.5:7b"));
  assert.ok(C.modelLabel("llama3.2:1b").includes("quick"), C.modelLabel("llama3.2:1b"));
  assert.strictEqual(C.modelLabel("mystery"), "mystery");
});

test("the empty state explains the smallest-model default", () => {
  const html = fs.readFileSync(path.join(SITE, "chat.html"), "utf8");
  assert.ok(/Running the smallest models for speed/.test(html));
  assert.ok(/trade latency for quality/.test(html));
});

/* ============================================================
   Shipped page wiring (static)
   ============================================================ */

test("chat.html loads the scripts and styles it needs", () => {
  const html = fs.readFileSync(path.join(SITE, "chat.html"), "utf8");
  assert.ok(html.includes('src="assets/chat.js"'), "must load assets/chat.js");
  assert.ok(html.includes('href="assets/chat.css"'), "must load assets/chat.css");
  assert.ok(html.includes('src="assets/app.js"'), "must load the shared nav/theme script");
  assert.ok(html.indexOf("assets/chat.js") < html.indexOf("assets/app.js"), "chat.js first");
});

test("chat.html carries the composer, sidebar and message list elements", () => {
  const html = fs.readFileSync(path.join(SITE, "chat.html"), "utf8");
  for (const id of ["composer", "composer-input", "send-btn", "stop-btn",
                    "messages", "empty-state", "conv-list", "model-select",
                    "new-chat", "export-btn", "clear-all"]) {
    assert.ok(html.includes('id="' + id + '"'), "missing #" + id);
  }
  assert.ok(/<textarea[^>]*id="composer-input"/.test(html), "textarea must be the composer input");
});

test("chat.html never ships an inline handler or eval", () => {
  const html = fs.readFileSync(path.join(SITE, "chat.html"), "utf8");
  assert.ok(!/\son[a-z]+\s*=/i.test(html.replace(/on[a-z]+=""/gi, "")), "no inline event handlers");
  assert.ok(!/eval\s*\(/.test(html));
});

test("every top-level page links to Chat in the nav", () => {
  const pages = ["index.html", "ai.html", "download.html", "deploy.html",
                 "access-points.html", "nodes.html", "buy.html", "contact.html"];
  for (const page of pages) {
    const html = fs.readFileSync(path.join(SITE, page), "utf8");
    const nav = html.slice(html.indexOf('class="nav-links"'), html.indexOf("</nav>"));
    assert.ok(/href="chat\.html"[^>]*>Chat</.test(nav), page + " nav must link to Chat");
  }
});

test("the AI page points at the full chat", () => {
  const html = fs.readFileSync(path.join(SITE, "ai.html"), "utf8");
  assert.ok(/href="chat\.html"/.test(html));
  assert.ok(/full chat/i.test(html));
});

test("the sitemap lists /chat", () => {
  const xml = fs.readFileSync(path.join(SITE, "sitemap.xml"), "utf8");
  assert.ok(xml.includes("https://gpncoin.ai/chat"), "sitemap must list the chat page");
});

console.log("\n" + passed + " tests passed");
