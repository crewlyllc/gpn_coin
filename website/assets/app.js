/* ============================================================
   GPN website — small progressive-enhancement script.
   No dependencies, no build. Safe to open from file:// (same-origin
   API calls just won't work from file:// — see the CORS note on ai.html).
   ============================================================ */
(function () {
  "use strict";

  /* ---------- Theme (dark by default; remembers choice) ---------- */
  var root = document.documentElement;
  try {
    var saved = localStorage.getItem("gpn-theme");
    if (saved === "light") root.classList.add("light");
  } catch (e) { /* storage disabled — ignore */ }

  function wireTheme() {
    var btn = document.getElementById("theme-toggle");
    if (!btn) return;
    var light = function () { return root.classList.contains("light"); };
    var paint = function () {
      btn.setAttribute("aria-pressed", String(light()));
      btn.setAttribute("title", light() ? "Switch to dark theme" : "Switch to light theme");
      btn.innerHTML = light() ? "&#9728;&#65039;" : "&#9789;"; /* sun / moon */
    };
    paint();
    btn.addEventListener("click", function () {
      root.classList.toggle("light");
      try { localStorage.setItem("gpn-theme", light() ? "light" : "dark"); } catch (e) {}
      paint();
    });
  }

  /* ---------- Mobile nav ---------- */
  function wireNav() {
    var btn = document.getElementById("nav-toggle");
    var links = document.getElementById("nav-links");
    if (!btn || !links) return;
    btn.addEventListener("click", function () {
      var open = links.classList.toggle("open");
      btn.setAttribute("aria-expanded", String(open));
    });
    links.addEventListener("click", function (e) {
      if (e.target.tagName === "A") { links.classList.remove("open"); btn.setAttribute("aria-expanded", "false"); }
    });
  }

  /* ---------- Footer year ---------- */
  function wireYear() {
    document.querySelectorAll("[data-year]").forEach(function (el) {
      el.textContent = String(new Date().getFullYear());
    });
  }

  /* ---------- "Try it" access-node widget (ai.html) ---------- */
  function wireTryIt() {
    var form = document.getElementById("try-form");
    if (!form) return;

    var urlEl = document.getElementById("ap-url");
    var tokenEl = document.getElementById("ap-token");
    var modelEl = document.getElementById("ap-model");
    var promptEl = document.getElementById("ap-prompt");
    var out = document.getElementById("try-result");
    var submit = document.getElementById("try-submit");

    /* Optional ?ap=... / ?token=... overrides (token never persisted). */
    try {
      var q = new URLSearchParams(location.search);
      if (q.get("ap") && urlEl) urlEl.value = q.get("ap");
      if (q.get("model") && modelEl) modelEl.value = q.get("model");
    } catch (e) {}

    function render(cls, html) {
      out.className = "result" + (cls ? " " + cls : "");
      out.innerHTML = html;
    }

    function stripTrailingSlash(u) { return u.replace(/\/+$/, ""); }

    function endpoint() {
      // Accept either a bare host base ("http://host:8092") or a full path.
      var base = stripTrailingSlash((urlEl && urlEl.value || "").trim());
      if (!base) return "";
      if (/\/v1\/chat\/completions$/.test(base)) return base;      // already a full endpoint
      if (/\/v1$/.test(base)) return base + "/chat/completions";   // base + /v1
      return base + "/v1/chat/completions";
    }

    function withTimeout(promise, ms) {
      return new Promise(function (resolve, reject) {
        var t = setTimeout(function () { reject(new Error("Timed out after " + (ms / 1000) + "s")); }, ms);
        promise.then(function (v) { clearTimeout(t); resolve(v); },
                     function (e) { clearTimeout(t); reject(e); });
      });
    }

    function extractText(data) {
      if (data == null) return "";
      if (typeof data === "string") return data;
      // OpenAI-compatible
      if (data.choices && data.choices[0]) {
        var c = data.choices[0];
        if (c.message && c.message.content != null) return c.message.content;
        if (c.text != null) return c.text;
      }
      // Access-point / jobs style
      if (data.answer && data.answer.response != null) return data.answer.response;
      if (data.response != null) return data.response;
      if (data.answer != null && typeof data.answer === "string") return data.answer;
      if (data.output != null && typeof data.output === "string") return data.output;
      return JSON.stringify(data, null, 2);
    }

    form.addEventListener("submit", function (e) {
      e.preventDefault();
      var url = endpoint();
      var prompt = (promptEl && promptEl.value || "").trim();
      if (!url) { render("", '<span class="err">Enter an access-point URL first.</span>'); return; }
      if (!prompt) { render("", '<span class="err">Type a prompt first.</span>'); return; }

      var model = (modelEl && modelEl.value || "auto").trim() || "auto";
      var body = {
        model: model,
        messages: [{ role: "user", content: prompt }],
        stream: false
      };

      var headers = { "Content-Type": "application/json" };
      var tok = (tokenEl && tokenEl.value || "").trim();
      if (tok) headers["Authorization"] = "Bearer " + tok;

      if (submit) { submit.disabled = true; }
      render("empty", '<span class="spinner"></span> &nbsp;Contacting the access point&hellip;');
      var t0 = Date.now();

      withTimeout(fetch(url, { method: "POST", headers: headers, body: JSON.stringify(body) }), 120000)
        .then(function (res) {
          return res.text().then(function (text) {
            var data;
            try { data = JSON.parse(text); } catch (e) { data = text; }
            if (!res.ok) {
              var detail = typeof data === "string" ? data : JSON.stringify(data);
              throw new Error("HTTP " + res.status + " " + res.statusText + (detail ? " — " + detail : ""));
            }
            return data;
          });
        })
        .then(function (data) {
          var answer = extractText(data);
          var ms = Date.now() - t0;
          render("", '<span class="ok">Answer</span>\n' + escapeHtml(answer) +
                     '<span class="meter">' + ms + ' ms · ' + escapeHtml(model) + '</span>');
        })
        .catch(function (err) {
          render("", '<span class="err">Request failed.</span>\n' + escapeHtml(String(err.message || err)) +
            '\n\n<p class="small muted mb-0">Things to check: (1) the access point is up ' +
            '(try its <code>/mode</code> or <code>/health</code> endpoint); (2) you opened this page over ' +
            '<code>http(s)://</code>, not <code>file://</code>; (3) the access point allows your origin (CORS) ' +
            'and, for non-loopback callers, a valid token. Browser requests to a plain ' +
            '<code>http://</code> host from an <code>https://</code> page are blocked by mixed-content rules.</p>');
        })
        .finally(function () { if (submit) { submit.disabled = false; } });
    });
  }

  function escapeHtml(s) {
    return String(s == null ? "" : s)
      .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;").replace(/'/g, "&#39;");
  }

  /* ---------- Copy-to-clipboard buttons ---------- */
  function wireCopy() {
    document.querySelectorAll("[data-copy]").forEach(function (btn) {
      btn.addEventListener("click", function () {
        var sel = btn.getAttribute("data-copy");
        var target = document.querySelector(sel);
        if (!target) return;
        var text = target.innerText;
        var done = function () {
          var old = btn.textContent;
          btn.textContent = "Copied \u2713";
          setTimeout(function () { btn.textContent = old; }, 1400);
        };
        if (navigator.clipboard && navigator.clipboard.writeText) {
          navigator.clipboard.writeText(text).then(done, fallback);
        } else { fallback(); }
        function fallback() {
          var r = document.createRange(); r.selectNode(target);
          var s = window.getSelection(); s.removeAllRanges(); s.addRange(r);
          try { document.execCommand("copy"); done(); } catch (e) {}
        }
      });
    });
  }

  function init() { wireTheme(); wireNav(); wireYear(); wireTryIt(); wireCopy(); }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
