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

  /* ---------- OS tabs (home quickstart) ---------- */
  function wireTabs() {
    var lists = document.querySelectorAll('[role="tablist"]');
    if (!lists.length) return;
    Array.prototype.forEach.call(lists, function (list) {
      var tabs = Array.prototype.slice.call(list.querySelectorAll('[role="tab"]'));
      if (!tabs.length) return;
      var key = "gpn-tabs:" + (list.getAttribute("aria-label") || "tabs");

      function panelOf(tab) {
        var id = tab.getAttribute("aria-controls");
        return id ? document.getElementById(id) : null;
      }

      function select(tab, focus) {
        tabs.forEach(function (t) {
          var on = t === tab;
          t.setAttribute("aria-selected", String(on));
          t.tabIndex = on ? 0 : -1;
          t.classList.toggle("is-selected", on);
          var p = panelOf(t);
          if (p) p.hidden = !on;
        });
        if (focus) tab.focus();
        try { sessionStorage.setItem(key, tab.getAttribute("data-os") || tab.id); } catch (e) {}
      }

      /* Restore the visitor's last choice, else guess the OS once. */
      var wanted = null;
      try { wanted = sessionStorage.getItem(key); } catch (e) {}
      if (!wanted) {
        var ua = (navigator.userAgent || "") + " " + (navigator.platform || "");
        if (/Win/i.test(ua)) wanted = "windows";
        else if (/Mac|iPhone|iPad/i.test(ua)) wanted = "macos";
        else wanted = "linux";
      }
      var initial = tabs.filter(function (t) { return t.getAttribute("data-os") === wanted; })[0];
      select(initial || tabs[0], false);

      list.addEventListener("click", function (e) {
        var tab = e.target.closest ? e.target.closest('[role="tab"]') : null;
        if (tab && list.contains(tab)) select(tab, false);
      });

      list.addEventListener("keydown", function (e) {
        var i = tabs.indexOf(e.target);
        if (i < 0) return;
        var next = null;
        if (e.key === "ArrowRight" || e.key === "ArrowDown") next = tabs[(i + 1) % tabs.length];
        else if (e.key === "ArrowLeft" || e.key === "ArrowUp") next = tabs[(i - 1 + tabs.length) % tabs.length];
        else if (e.key === "Home") next = tabs[0];
        else if (e.key === "End") next = tabs[tabs.length - 1];
        if (next) { e.preventDefault(); select(next, true); }
      });
    });
  }

  /* ---------- Join-the-team application form ---------- */
  function wireJoinForm() {
    var form = document.getElementById("join-form");
    if (!form) return;
    var out = document.getElementById("join-result");
    var submit = document.getElementById("join-submit");
    var emailRe = /^[^\s@]+@[^\s@]+\.[^\s@]{2,}$/;

    function render(cls, html) {
      if (!out) return;
      out.className = "result" + (cls ? " " + cls : "");
      out.innerHTML = html;
    }
    function val(name) {
      var el = form.elements[name];
      return el && typeof el.value === "string" ? el.value.trim() : "";
    }
    function fail(msg) { render("", '<span class="err">Check that field.</span>\n' + escapeHtml(msg)); }

    form.addEventListener("submit", function (e) {
      e.preventDefault();

      /* Honeypot: a real person never sees this input. Stay silent, look successful. */
      if (val("website")) { render("", '<span class="ok">Thanks \u2014 application received.</span>'); form.reset(); return; }

      var name = val("name"), email = val("email"), role = val("role"), message = val("message");
      if (name.length < 2) return fail("Please tell us your name (2\u201380 characters).");
      if (name.length > 80) return fail("That name is too long \u2014 80 characters max.");
      if (!emailRe.test(email) || email.length > 200) return fail("That email address doesn't look right.");
      if (message.length > 4000) return fail("Please keep the message under 4000 characters.");

      var body = { name: name, email: email, role: role || "unspecified", message: message };
      if (submit) submit.disabled = true;
      render("empty", '<span class="spinner"></span> &nbsp;Sending&hellip;');

      fetch("/api/join", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body)
      }).then(function (res) {
        return res.text().then(function (text) {
          var data;
          try { data = JSON.parse(text); } catch (err) { data = null; }
          if (!res.ok || !data) throw new Error("HTTP " + res.status);
          return data;
        });
      }).then(function (data) {
        if (!data.ok) {
          render("", '<span class="err">Not sent.</span>\n' + escapeHtml(data.error || "Something went wrong."));
          return;
        }
        var ref = data.reference ? '<span class="meter">reference ' + escapeHtml(data.reference) + '</span>' : "";
        if (data.stored === false) {
          var mail = "mailto:hello@gpncoin.ai?subject=" + encodeURIComponent("Join GPN") +
            "&body=" + encodeURIComponent(name + " <" + email + ">\n" + (role || "") + "\n\n" + message);
          render("", '<span class="ok">Almost \u2014 one more step.</span>\n' +
            "Our form couldn't save your application automatically. Please send the same details by email " +
            '(<a href="' + escapeHtml(mail) + '">open the draft</a>) and we\'ll reply.' + ref);
          return;
        }
        render("", '<span class="ok">Application received \u2014 thank you.</span>\n' +
          "We read everything and reply from hello@gpncoin.ai. If you have a repo or benchmark, " +
          "replying to that address with a link is the fastest way to get our attention." + ref);
        form.reset();
      }).catch(function (err) {
        var mail = "mailto:hello@gpncoin.ai?subject=" + encodeURIComponent("Join GPN") +
          "&body=" + encodeURIComponent(name + " <" + email + ">\n" + (role || "") + "\n\n" + message);
        render("", '<span class="err">Could not reach the server.</span>\n' +
          escapeHtml(String(err.message || err)) + "\n" +
          'Please <a href="' + escapeHtml(mail) + '">send it by email instead</a>.');
      }).finally(function () { if (submit) submit.disabled = false; });
    });
  }

  function init() { wireTheme(); wireNav(); wireYear(); wireTryIt(); wireCopy(); wireTabs(); wireJoinForm(); }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
