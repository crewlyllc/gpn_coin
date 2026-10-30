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

  /* ---------- "Try it" widget (ai.html) ----------
     Talks only to the same-origin HTTPS proxy (/api/*), which forwards to the
     access point. No mixed content, no CORS, no token in the page. */
  function wireTryIt() {
    var form = document.getElementById("try-form");
    if (!form) return;

    var modelEl = document.getElementById("ai-model");
    var promptEl = document.getElementById("ai-prompt");
    var maxEl = document.getElementById("ai-max");
    var out = document.getElementById("ai-result");
    var statusEl = document.getElementById("ai-status");
    var submit = document.getElementById("ai-submit");
    var badge = document.getElementById("net-status");

    function render(cls, html) {
      out.className = "result" + (cls ? " " + cls : "");
      out.innerHTML = html;
    }
    function setStatus(text) {
      if (statusEl) statusEl.textContent = text || "";
    }

    function setOptions(ids) {
      if (!modelEl || !ids.length) return;
      var wanted = modelEl.value;
      modelEl.innerHTML = "";
      ids.forEach(function (id) {
        var o = document.createElement("option");
        o.value = id;
        o.textContent = id;
        modelEl.appendChild(o);
      });
      if (ids.indexOf(wanted) >= 0) modelEl.value = wanted;
      else if (ids.indexOf("llama3.2:1b") >= 0) modelEl.value = "llama3.2:1b";
    }

    /* Hero badge + model picker: both from the live proxy. */
    fetch("/api/v1/models", { headers: { accept: "application/json" } })
      .then(function (r) { return r.ok ? r.json() : Promise.reject(r.status); })
      .then(function (doc) {
        var ids = ((doc && doc.data) || []).map(function (m) { return m.id; }).filter(Boolean);
        setOptions(ids);
        if (badge) {
          badge.textContent = ids.length
            ? ids.length + " model" + (ids.length === 1 ? "" : "s") + " online"
            : "testnet live";
        }
      })
      .catch(function () {
        if (badge) badge.textContent = "testnet live";
      });

    fetch("/api/health", { headers: { accept: "application/json" } })
      .then(function (r) { return r.ok ? r.json() : Promise.reject(r.status); })
      .then(function (h) {
        if (badge && h && Number(h.providers) > 0) {
          var n = Number(h.providers);
          badge.textContent = n + " provider" + (n === 1 ? "" : "s") + " online";
        }
      })
      .catch(function () {});

    form.addEventListener("submit", function (e) {
      e.preventDefault();
      var prompt = (promptEl && promptEl.value || "").trim();
      var model = (modelEl && modelEl.value || "llama3.2:1b").trim() || "llama3.2:1b";
      if (!prompt) {
        render("", '<span class="err">Type a prompt first.</span>');
        return;
      }

      var body = {
        model: model,
        messages: [{ role: "user", content: prompt }],
        stream: false
      };
      var maxTokens = parseInt((maxEl && maxEl.value || "").trim(), 10);
      if (maxTokens > 0) body.max_tokens = maxTokens;

      if (submit) submit.disabled = true;
      render("empty", '<span class="spinner"></span> &nbsp;Waiting for a GPU&hellip;');
      var t0 = Date.now();
      var tick = setInterval(function () {
        setStatus("Waiting for a GPU… " + Math.round((Date.now() - t0) / 1000) + " s. " +
          "A cold testnet job needs block inclusion plus model load.");
      }, 1000);
      setStatus("Waiting for a GPU… 0 s.");

      var controller = typeof AbortController !== "undefined" ? new AbortController() : null;
      var timer = setTimeout(function () { if (controller) controller.abort(); }, 150000);

      fetch("/api/v1/chat/completions", {
        method: "POST",
        headers: { "Content-Type": "application/json", accept: "application/json" },
        body: JSON.stringify(body),
        signal: controller ? controller.signal : undefined
      })
        .then(function (res) {
          return res.text().then(function (text) {
            var data;
            try { data = JSON.parse(text); } catch (err) { data = null; }
            if (!res.ok) {
              var msg = (data && data.error && (data.error.message || data.error)) ||
                        (typeof data === "string" ? data : text);
              var e = new Error(String(msg || ("HTTP " + res.status)));
              e.status = res.status;
              throw e;
            }
            if (!data) throw new Error("The access point returned a non-JSON body.");
            return data;
          });
        })
        .then(function (data) {
          var choice = data.choices && data.choices[0];
          var answer = (choice && choice.message && choice.message.content) ||
                       (choice && choice.text) || data.response || "";
          var secs = ((Date.now() - t0) / 1000).toFixed(1);
          var usage = data.usage || {};
          var meta = secs + " s · " + (data.model || model) + " via gpncoin.ai";
          if (usage.total_tokens) meta += " · " + usage.total_tokens + " tokens";
          render("", '<span class="ok">Answer</span>\n' + escapeHtml(String(answer).trim()) +
                     '<span class="meter">' + escapeHtml(meta) + '</span>');
          setStatus("");
        })
        .catch(function (err) {
          var status = err && err.status;
          var hint = "";
          if (err && err.name === "AbortError") {
            hint = "No answer within 150 s. The testnet may be busy — try again, or pick a smaller model.";
          } else if (status === 429) {
            hint = "Rate limit reached. The free endpoint allows 12 chats per 10 minutes per IP.";
          } else if (status === 502) {
            hint = "The proxy could not complete the call (access point unreachable, or its token is not configured). Check /api/health, then retry.";
          } else if (status === 400) {
            hint = "The request was rejected — check the model name against /api/v1/models.";
          } else {
            hint = "Check your connection and /api/health, then retry.";
          }
          render("", '<span class="err">Request failed.</span>\n' +
            escapeHtml(String(err && err.message ? err.message : err)) +
            '\n\n<p class="small muted mb-0">' + escapeHtml(hint) + '</p>');
          setStatus("");
        })
        .finally(function () {
          clearInterval(tick);
          clearTimeout(timer);
          if (submit) submit.disabled = false;
        });
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

  /* ---------- Referral links (`?ref=CODE`) ----------
     A download URL cannot carry a query param into a zip, so the page bakes
     the code into the copy-paste command instead: every `<span
     data-ref-arg></span>` placeholder becomes " --ref CODE". The code is
     validated and written as TEXT (assets/referral.js); with no code the
     placeholders stay empty and the page is byte-identical to before.

     Persisted in sessionStorage so it survives clicking around the site. */
  function wireReferral() {
    var R = window.GPN && window.GPN.referral;
    if (!R) return;
    var KEY = "gpn-ref";
    var DISMISSED = "gpn-ref-note-dismissed";

    var code = R.fromSearch(window.location.search);
    if (code) {
      try { sessionStorage.setItem(KEY, code); } catch (e) { /* storage off */ }
    } else {
      try { code = R.validate(sessionStorage.getItem(KEY) || ""); } catch (e) { code = null; }
    }
    if (!code) return;

    var targets = document.querySelectorAll("[data-ref-arg]");
    if (!targets.length) return;
    Array.prototype.forEach.call(targets, function (el) { R.renderInto(el, code); });

    var dismissed = false;
    try { dismissed = sessionStorage.getItem(DISMISSED) === code; } catch (e) {}
    if (dismissed) return;

    var note = document.createElement("p");
    note.className = "ref-note small muted";
    note.setAttribute("role", "status");

    var text = document.createElement("span");
    /* textContent, not markup assignment: `code` is URL input. */
    text.textContent = "Referral: " + code + " \u2014 recorded once on first run";
    note.appendChild(text);

    var btn = document.createElement("button");
    btn.type = "button";
    btn.className = "btn small ghost no-print";
    btn.textContent = "Dismiss";
    btn.addEventListener("click", function () {
      note.parentNode && note.parentNode.removeChild(note);
      try { sessionStorage.setItem(DISMISSED, code); } catch (e) {}
    });
    note.appendChild(document.createTextNode(" "));
    note.appendChild(btn);

    /* Place the note next to the first command it applies to. */
    var anchor = targets[0].closest ? targets[0].closest(".field, .codeblock") : null;
    var host = anchor || targets[0].parentNode;
    if (host && host.parentNode) host.parentNode.insertBefore(note, host.nextSibling);
  }

  function init() { wireTheme(); wireNav(); wireYear(); wireTryIt(); wireCopy(); wireTabs(); wireReferral(); wireJoinForm(); }
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init);
  else init();
})();
