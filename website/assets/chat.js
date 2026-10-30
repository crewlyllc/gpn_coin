/* ============================================================
   GPN chat — a small, dependency-free ChatGPT-style front end for
   POST /api/v1/chat/completions on gpncoin.ai.

   The pure half (escape/markdown/SSE/pruning/error copy) is exported so it
   can be unit-tested in Node without a DOM — see website/tests/chat.test.js.
   The UI half only runs in a browser.

   SECURITY MODEL — read before changing anything that touches text:

     * Model output and user input are UNTRUSTED. They reach the document only
       through renderMarkdown(), which escapes EVERY character first (& < > "
       ') and then re-introduces a whitelist of tags it emits itself. There is
       no raw `innerHTML = modelText` anywhere.
     * Links must be http(s). `javascript:` and `data:` URLs never become <a>.
     * No eval, no new Function, no dynamic <script>, no third-party CDN.
   ============================================================ */
(function (root, factory) {
  if (typeof module === "object" && module.exports) {
    module.exports = factory();
  } else {
    root.GPN = root.GPN || {};
    root.GPN.chat = factory();
  }
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  /* ============================================================
     1. Escaping
     ============================================================ */

  function escapeHtml(value) {
    return String(value == null ? "" : value)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;")
      .replace(/'/g, "&#39;");
  }

  /* Only absolute http(s) URLs survive. A value like `javascript:alert(1)` or
     `data:text/html,...` returns null and is rendered as literal text. */
  function safeUrl(url) {
    var value = String(url == null ? "" : url).trim();
    if (!/^https?:\/\//i.test(value)) return null;
    if (/[\s<>"']/.test(value)) return null;
    return value;
  }

  /* ============================================================
     2. Inline markdown (operates on ALREADY-ESCAPED text)
     ============================================================ */

  function isWordChar(ch) {
    return /[A-Za-z0-9_]/.test(ch);
  }

  function renderInline(esc) {
    var out = "";
    var i = 0;
    var n = esc.length;

    while (i < n) {
      var ch = esc.charAt(i);

      /* inline code: `x` — no newlines, closing backtick required */
      if (ch === "`") {
        var tick = esc.indexOf("`", i + 1);
        if (tick > i + 1) {
          var code = esc.slice(i + 1, tick);
          if (code.indexOf("\n") === -1) {
            out += "<code>" + code + "</code>";
            i = tick + 1;
            continue;
          }
        }
        out += ch;
        i++;
        continue;
      }

      /* [label](url) — http(s) only */
      if (ch === "[") {
        var lm = /^\[([^\]\n]{0,500})\]\(([^()\s]{1,500})\)/.exec(esc.slice(i, i + 1100));
        if (lm) {
          var href = safeUrl(lm[2]);
          if (href) {
            out += '<a href="' + href + '" rel="noopener noreferrer" target="_blank">' +
              (renderInline(lm[1]) || lm[1]) + "</a>";
            i += lm[0].length;
            continue;
          }
        }
        out += ch;
        i++;
        continue;
      }

      /* **bold** */
      if (esc.substr(i, 2) === "**") {
        var boldEnd = esc.indexOf("**", i + 2);
        if (boldEnd > i + 2) {
          out += "<strong>" + renderInline(esc.slice(i + 2, boldEnd)) + "</strong>";
          i = boldEnd + 2;
          continue;
        }
      }

      /* *italic* (single asterisk, not inside a word) */
      if (ch === "*" && esc.charAt(i + 1) !== "*" && !isWordChar(esc.charAt(i - 1))) {
        var emEnd = esc.indexOf("*", i + 1);
        if (emEnd > i + 1) {
          var body = esc.slice(i + 1, emEnd);
          if (body.charAt(0) !== " " && body.charAt(body.length - 1) !== " " &&
              body.indexOf("\n") === -1) {
            out += "<em>" + renderInline(body) + "</em>";
            i = emEnd + 1;
            continue;
          }
        }
      }

      /* _italic_ — only at a word boundary, so snake_case survives */
      if (ch === "_" && !isWordChar(esc.charAt(i - 1))) {
        var usEnd = esc.indexOf("_", i + 1);
        if (usEnd > i + 1) {
          var ubody = esc.slice(i + 1, usEnd);
          if (ubody.charAt(0) !== " " && ubody.charAt(ubody.length - 1) !== " " &&
              ubody.indexOf("\n") === -1 && ubody.indexOf(" ") !== -1) {
            out += "<em>" + renderInline(ubody) + "</em>";
            i = usEnd + 1;
            continue;
          }
        }
      }

      out += ch;
      i++;
    }
    return out;
  }

  /* ============================================================
     3. Block markdown
     ============================================================ */

  var FENCE_OPEN = /^(`{3,}|~{3,})\s*([^`]*)$/;
  var HEADING = /^(#{1,6})\s+(.*)$/;
  var UL_ITEM = /^\s{0,3}[-*+]\s+(.*)$/;
  var OL_ITEM = /^\s{0,3}(\d{1,3})[.)]\s+(.*)$/;
  var QUOTE = /^\s{0,3}>\s?(.*)$/;
  var RULE = /^\s{0,3}([-*_])(?:\s*\1){2,}\s*$/;

  /* Returns HTML built exclusively from escaped text plus tags this function
     writes itself. Never feed the result of this to anything but innerHTML. */
  function renderMarkdown(source) {
    if (source == null) return "";
    var text = String(source).replace(/\r\n?/g, "\n");
    var lines = text.split("\n");
    var html = [];
    var i = 0;

    while (i < lines.length) {
      var line = lines[i];

      if (line.trim() === "") { i++; continue; }

      /* fenced code block: 3+ backticks or tildes; the closing fence must be
         at least as long, so ```` wrapping ``` is one block, not two */
      var fence = FENCE_OPEN.exec(line);
      if (fence) {
        var marker = fence[1];
        var lang = (fence[2] || "").trim().slice(0, 24);
        var closeRe = new RegExp("^" + marker.charAt(0) + "{" + marker.length + ",}\\s*$");
        var buf = [];
        i++;
        while (i < lines.length) {
          if (closeRe.test(lines[i])) { i++; break; }
          buf.push(lines[i]);
          i++;
        }
        var cls = lang ? ' class="lang-' + lang.replace(/[^A-Za-z0-9_+-]/g, "") + '"' : "";
        html.push("<pre><code" + cls + ">" + escapeHtml(buf.join("\n")) + "</code></pre>");
        continue;
      }

      if (RULE.test(line)) { html.push("<hr>"); i++; continue; }

      var heading = HEADING.exec(line);
      if (heading) {
        var level = heading[1].length;
        html.push("<h" + level + ">" + renderInline(escapeHtml(heading[2].trim())) + "</h" + level + ">");
        i++;
        continue;
      }

      if (QUOTE.test(line)) {
        var qbuf = [];
        while (i < lines.length && QUOTE.test(lines[i])) {
          qbuf.push(renderInline(escapeHtml(QUOTE.exec(lines[i])[1])));
          i++;
        }
        html.push("<blockquote>" + qbuf.join("<br>") + "</blockquote>");
        continue;
      }

      if (UL_ITEM.test(line)) {
        var items = [];
        while (i < lines.length && UL_ITEM.test(lines[i])) {
          items.push("<li>" + renderInline(escapeHtml(UL_ITEM.exec(lines[i])[1])) + "</li>");
          i++;
        }
        html.push("<ul>" + items.join("") + "</ul>");
        continue;
      }

      if (OL_ITEM.test(line)) {
        var oitems = [];
        while (i < lines.length && OL_ITEM.test(lines[i])) {
          oitems.push("<li>" + renderInline(escapeHtml(OL_ITEM.exec(lines[i])[2])) + "</li>");
          i++;
        }
        html.push("<ol>" + oitems.join("") + "</ol>");
        continue;
      }

      /* paragraph: consecutive plain lines, single newline = <br> */
      var para = [];
      while (i < lines.length) {
        var l = lines[i];
        if (l.trim() === "") break;
        if (FENCE_OPEN.test(l) || HEADING.test(l) || UL_ITEM.test(l) ||
            OL_ITEM.test(l) || QUOTE.test(l) || RULE.test(l)) break;
        para.push(renderInline(escapeHtml(l)));
        i++;
      }
      if (para.length) html.push("<p>" + para.join("<br>") + "</p>");
      else i++; /* defensive: never loop forever on an unmatched line */
    }

    return html.join("");
  }

  /* ============================================================
     4. SSE stream parser
     ============================================================ */

  /* Feed decoded text chunks with push(); it returns complete events.
     Events: {type:"data", data}, {type:"done"}, {type:"error", raw}.
     Handles CRLF, multi-line `data:` fields, comments, and JSON split across
     arbitrary chunk boundaries. */
  function SSEParser() {
    this.buf = "";
    this.data = [];
  }

  SSEParser.prototype._dispatch = function () {
    if (!this.data.length) return null;
    var payload = this.data.join("\n");
    this.data = [];
    if (payload === "[DONE]") return { type: "done" };
    var obj;
    try {
      obj = JSON.parse(payload);
    } catch (e) {
      return { type: "error", raw: payload };
    }
    if (!obj || typeof obj !== "object") return { type: "error", raw: payload };
    return { type: "data", data: obj };
  };

  SSEParser.prototype._line = function (line) {
    if (line === "") return this._dispatch();
    if (line.charAt(0) === ":") return null;          /* comment / heartbeat */
    var m = /^data:(.*)$/.exec(line);
    if (m) {
      var value = m[1];
      if (value.charAt(0) === " ") value = value.slice(1);
      this.data.push(value);
    }
    /* event:, id:, retry: are ignored — OpenAI-shaped streams do not need them */
    return null;
  };

  SSEParser.prototype.push = function (chunk) {
    if (chunk == null) return [];
    this.buf += String(chunk);
    var out = [];
    var idx;
    while ((idx = this.buf.indexOf("\n")) !== -1) {
      var line = this.buf.slice(0, idx);
      this.buf = this.buf.slice(idx + 1);
      if (line.charAt(line.length - 1) === "\r") line = line.slice(0, -1);
      var ev = this._line(line);
      if (ev) out.push(ev);
    }
    return out;
  };

  /* End of transport: flush a trailing unterminated line and any pending data. */
  SSEParser.prototype.flush = function () {
    var out = [];
    if (this.buf) {
      var ev = this._line(this.buf);
      this.buf = "";
      if (ev) out.push(ev);
    }
    var last = this._dispatch();
    if (last) out.push(last);
    return out;
  };

  /* Text carried by one chat.completion.chunk (or a whole non-stream reply). */
  function chunkText(obj) {
    if (!obj || !obj.choices || !obj.choices[0]) return "";
    var choice = obj.choices[0];
    if (choice.delta && typeof choice.delta.content === "string") return choice.delta.content;
    if (choice.message && typeof choice.message.content === "string") return choice.message.content;
    if (typeof choice.text === "string") return choice.text;
    return "";
  }

  /* ============================================================
     5. Conversations: titles, caps, export
     ============================================================ */

  var MAX_CONVERSATIONS = 30;
  var MAX_MESSAGES = 200;

  function deriveTitle(text, max) {
    var limit = max || 48;
    var flat = String(text == null ? "" : text).replace(/\s+/g, " ").trim();
    if (!flat) return "New chat";
    if (flat.length <= limit) return flat;
    return flat.slice(0, limit - 1).trimEnd() + "\u2026";
  }

  /* Newest first, at most `maxConversations`, at most `maxMessages` each.
     Pure: returns a new array, never mutates the input. */
  function pruneConversations(list, options) {
    var maxConvos = (options && options.maxConversations) || MAX_CONVERSATIONS;
    var maxMsgs = (options && options.maxMessages) || MAX_MESSAGES;
    if (!Array.isArray(list)) return [];

    var copy = [];
    for (var i = 0; i < list.length; i++) {
      var conv = list[i];
      if (!conv || typeof conv !== "object") continue;
      var out = {};
      for (var key in conv) {
        if (Object.prototype.hasOwnProperty.call(conv, key)) out[key] = conv[key];
      }
      out.messages = Array.isArray(conv.messages) ? conv.messages.slice(-maxMsgs) : [];
      copy.push(out);
    }
    copy.sort(function (a, b) { return (b.updatedAt || 0) - (a.updatedAt || 0); });
    return copy.slice(0, maxConvos);
  }

  function conversationToMarkdown(conv) {
    if (!conv) return "";
    var lines = [];
    lines.push("# " + (conv.title || "GPN chat"));
    lines.push("");
    lines.push("_Model: " + (conv.model || "unknown") + " · exported " + new Date().toISOString() + "_");
    lines.push("");
    (conv.messages || []).forEach(function (m) {
      lines.push(m.role === "user" ? "## You" : "## Assistant");
      lines.push("");
      lines.push(String(m.content == null ? "" : m.content));
      lines.push("");
    });
    return lines.join("\n");
  }

  /* ============================================================
     6. Error copy
     ============================================================ */

  function serverMessage(bodyText) {
    var body = String(bodyText == null ? "" : bodyText);
    try {
      var doc = JSON.parse(body);
      if (doc && doc.error) {
        if (typeof doc.error === "string") return doc.error;
        if (doc.error.message) return String(doc.error.message);
      }
      if (doc && typeof doc.message === "string") return doc.message;
    } catch (e) { /* not JSON — fall through */ }
    var flat = body.replace(/<[^>]*>/g, " ").replace(/\s+/g, " ").trim();
    if (/^<!doctype|^<html/i.test(flat)) return "";
    return flat ? flat.slice(0, 200) : "";
  }

  /* Map the failures this endpoint actually produces to plain language. */
  function errorMessage(status, bodyText) {
    var detail = serverMessage(bodyText);
    switch (Number(status)) {
      case 429:
        return "Rate limit: 12 chats per 10 minutes per visitor. Try again shortly.";
      case 400:
        return "The API rejected the request" + (detail ? ": " + detail : ". Check the model name and message length.");
      case 413:
        return "That conversation is too large for the endpoint (64 KB request limit). Start a new chat.";
      case 502:
        return "The gpncoin.ai Worker could not reach the access point — usually a brief network blip. Try again.";
      case 503:
        return "No provider is online right now (the registry may still be warming up). Try again in a minute.";
      case 504:
        return "The request timed out at the edge. A cold on-chain job can take 30\u201390 s — try again.";
      default:
        if (status >= 500) {
          return "The network answered HTTP " + status + (detail ? ": " + detail : ".") + " Try again shortly.";
        }
        return detail || ("Request failed (HTTP " + status + ").");
    }
  }

  function timeoutMessage() {
    return "Still generating after 120 s — the network may be busy. Your partial answer is kept; try again.";
  }

  /* ============================================================
     7. Browser UI — only runs when there is a document
     ============================================================ */

  /* Label a model for the picker. The page exists to show the pipeline works,
     so the smallest model is the default and says so; larger models stay
     selectable with an honest latency note. Nothing here ever switches the
     model on the user's behalf — no auto-upgrade, no retry with a bigger one. */
  function modelHint(id) {
    var name = String(id == null ? "" : id).toLowerCase();
    var m = /(\d+(?:\.\d+)?)\s*b\b/.exec(name);
    if (!m) return "";
    var params = parseFloat(m[1]);
    if (!isFinite(params)) return "";
    if (params <= 0.6) return "fastest, best for demos";
    if (params <= 2) return "quick";
    return "slower to first token";
  }

  function modelLabel(id) {
    var hint = modelHint(id);
    return hint ? String(id) + " \u2014 " + hint : String(id);
  }

  var STORE_KEY = "gpn-chat-v1";
  var API_URL = "/api/v1/chat/completions";
  var MODELS_URL = "/api/v1/models";
  var FALLBACK_MODELS = ["qwen2.5:0.5b", "llama3.2:1b", "qwen2.5:7b", "deepseek-r1:7b"];  var CLIENT_TIMEOUT_MS = 120000;

  function boot() {
    var doc = document;

    var el = {
      shell: doc.getElementById("chat-shell"),
      sidebar: doc.getElementById("chat-sidebar"),
      backdrop: doc.getElementById("chat-backdrop"),
      scroll: doc.getElementById("chat-scroll"),
      sidebarToggle: doc.getElementById("chat-menu"),
      newChat: doc.getElementById("new-chat"),
      list: doc.getElementById("conv-list"),
      listEmpty: doc.getElementById("conv-empty"),
      clearAll: doc.getElementById("clear-all"),
      exportBtn: doc.getElementById("export-btn"),
      modelSelect: doc.getElementById("model-select"),
      modelNote: doc.getElementById("model-note"),
      messages: doc.getElementById("messages"),
      empty: doc.getElementById("empty-state"),
      starters: doc.getElementById("starters"),
      composer: doc.getElementById("composer"),
      input: doc.getElementById("composer-input"),
      send: doc.getElementById("send-btn"),
      stop: doc.getElementById("stop-btn"),
      note: doc.getElementById("composer-note"),
      status: doc.getElementById("chat-status")
    };
    if (!el.messages || !el.composer) return;

    /* ---------- state ---------- */

    var state = loadState();
    var busy = false;
    var abortCtrl = null;
    var stickToBottom = true;
    var activeMsgNode = null;

    /* ---------- storage ---------- */

    function loadState() {
      var fallback = { model: FALLBACK_MODELS[0], activeId: null, conversations: [] };
      var raw;
      try { raw = root.localStorage.getItem(STORE_KEY); } catch (e) { return fallback; }
      if (!raw) return fallback;
      try {
        var parsed = JSON.parse(raw);
        if (!parsed || typeof parsed !== "object") return fallback;
        return {
          model: typeof parsed.model === "string" && parsed.model ? parsed.model : fallback.model,
          activeId: typeof parsed.activeId === "string" ? parsed.activeId : null,
          conversations: pruneConversations(parsed.conversations)
        };
      } catch (e) {
        return fallback;
      }
    }

    function saveState() {
      state.conversations = pruneConversations(state.conversations);
      var payload = {
        model: state.model,
        activeId: state.activeId,
        conversations: state.conversations
      };
      try {
        root.localStorage.setItem(STORE_KEY, JSON.stringify(payload));
      } catch (err) {
        /* Quota: keep the ten newest conversations and retry once. */
        try {
          state.conversations = pruneConversations(state.conversations, { maxConversations: 10 });
          root.localStorage.setItem(STORE_KEY, JSON.stringify({
            model: state.model, activeId: state.activeId, conversations: state.conversations
          }));
        } catch (err2) { /* storage unavailable — the page still works for this session */ }
      }
    }

    /* ---------- helpers ---------- */

    function uid() {
      return "c" + Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
    }

    function activeConversation() {
      if (!state.activeId) return null;
      for (var i = 0; i < state.conversations.length; i++) {
        if (state.conversations[i].id === state.activeId) return state.conversations[i];
      }
      return null;
    }

    function newConversation() {
      var conv = {
        id: uid(),
        title: "New chat",
        model: state.model,
        messages: [],
        createdAt: Date.now(),
        updatedAt: Date.now()
      };
      state.conversations.unshift(conv);
      state.activeId = conv.id;
      return conv;
    }

    function touch(conv) {
      conv.updatedAt = Date.now();
      var firstUser = null;
      for (var i = 0; i < conv.messages.length; i++) {
        if (conv.messages[i].role === "user") { firstUser = conv.messages[i]; break; }
      }
      conv.title = firstUser ? deriveTitle(firstUser.content) : "New chat";
    }

    function nearBottom() {
      var box = el.scroll || el.messages;
      return box.scrollHeight - box.scrollTop - box.clientHeight < 90;
    }

    function scrollToBottom(force) {
      if (!force && !stickToBottom) return;
      var box = el.scroll || el.messages;
      box.scrollTop = box.scrollHeight;
    }

    function setNote(text, kind) {
      el.note.textContent = text || "";
      el.note.className = "composer-note" + (kind ? " " + kind : "");
    }

    function setBusy(on, reason) {
      busy = on;
      el.input.disabled = on;
      el.send.disabled = on;
      el.stop.hidden = !on;
      el.stop.disabled = !on;
      if (el.modelSelect) el.modelSelect.disabled = on;
      el.composer.classList.toggle("is-busy", on);
      if (on) setNote(reason || "Waiting for a GPU\u2026", "busy");
      else setNote("");
    }

    function fmtTime(ts) {
      if (!ts) return "";
      try {
        return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
      } catch (e) { return ""; }
    }

    function fmtLatency(ms) {
      if (!ms && ms !== 0) return "";
      return ms < 1000 ? Math.round(ms) + " ms" : (ms / 1000).toFixed(1) + " s";
    }

    /* ---------- sidebar ---------- */

    function renderConversations() {
      el.list.textContent = "";
      var convs = state.conversations.slice().sort(function (a, b) {
        return (b.updatedAt || 0) - (a.updatedAt || 0);
      });
      convs.forEach(function (conv) {
        var row = doc.createElement("div");
        row.className = "conv-row" + (conv.id === state.activeId ? " active" : "");

        var open = doc.createElement("button");
        open.type = "button";
        open.className = "conv-open";
        open.setAttribute("data-id", conv.id);
        var title = doc.createElement("span");
        title.className = "conv-title";
        title.textContent = conv.title || "New chat";
        var meta = doc.createElement("span");
        meta.className = "conv-meta";
        meta.textContent = conv.messages.length + " msg" + (conv.messages.length === 1 ? "" : "s");
        open.appendChild(title);
        open.appendChild(meta);
        open.addEventListener("click", function () {
          if (busy) return;              /* don't detach a streaming node */
          state.activeId = conv.id;
          saveState();
          render();
          closeSidebar();
          el.input.focus();
        });

        var del = doc.createElement("button");
        del.type = "button";
        del.className = "conv-del";
        del.setAttribute("aria-label", "Delete conversation: " + (conv.title || "New chat"));
        del.title = "Delete";
        del.textContent = "\u00d7";
        del.addEventListener("click", function (ev) {
          ev.stopPropagation();
          state.conversations = state.conversations.filter(function (c) { return c.id !== conv.id; });
          if (state.activeId === conv.id) state.activeId = null;
          saveState();
          render();
        });

        row.appendChild(open);
        row.appendChild(del);
        el.list.appendChild(row);
      });
      if (el.listEmpty) el.listEmpty.hidden = convs.length > 0;
    }

    function renderModels() {
      var ids = (renderModels.ids && renderModels.ids.length) ? renderModels.ids : FALLBACK_MODELS;
      el.modelSelect.textContent = "";
      ids.forEach(function (id) {
        var option = doc.createElement("option");
        option.value = id;
        option.textContent = modelLabel(id);
        el.modelSelect.appendChild(option);
      });
      if (ids.indexOf(state.model) === -1) state.model = pickDefault(ids);
      el.modelSelect.value = state.model;
      if (el.modelNote) el.modelNote.textContent = state.model;
    }

    /* smallest/fastest first so the first reply feels snappy */
    function pickDefault(ids) {
      var prefs = ["0.5b", "0.5", "1b", "1.5b", "3b"];
      for (var p = 0; p < prefs.length; p++) {
        for (var i = 0; i < ids.length; i++) {
          if (String(ids[i]).toLowerCase().indexOf(prefs[p]) !== -1) return ids[i];
        }
      }
      return ids[0];
    }

    function loadModels() {
      fetch(MODELS_URL, { headers: { accept: "application/json" } })
        .then(function (res) { return res.ok ? res.json() : Promise.reject(res.status); })
        .then(function (doc) {
          var ids = ((doc && doc.data) || []).map(function (m) { return m && m.id; }).filter(Boolean);
          if (!ids.length) throw new Error("empty model list");
          renderModels.ids = ids;
          renderModels();
        })
        .catch(function () {
          renderModels.ids = FALLBACK_MODELS.slice();
          renderModels();
        });
    }

    /* ---------- message rendering ---------- */

    function metaLine(msg) {
      var bits = [];
      if (msg.model) bits.push(msg.model);
      if (msg.latencyMs != null) bits.push(fmtLatency(msg.latencyMs));
      if (msg.usage && msg.usage.total_tokens) {
        bits.push(msg.usage.total_tokens + " tokens");
        if (msg.usage.completion_tokens) bits.push(msg.usage.completion_tokens + " out");
      }
      if (msg.stopped) bits.push("stopped");
      if (msg.ts) bits.push(fmtTime(msg.ts));
      return bits.join(" \u00b7 ");
    }

    function copyText(text, button) {
      function done(ok) {
        var old = button.textContent;
        button.textContent = ok ? "Copied" : "Copy failed";
        setTimeout(function () { button.textContent = old; }, 1200);
      }
      if (root.navigator && root.navigator.clipboard && root.navigator.clipboard.writeText) {
        root.navigator.clipboard.writeText(text).then(function () { done(true); }, function () { done(false); });
        return;
      }
      try {
        var ta = doc.createElement("textarea");
        ta.value = text;
        ta.setAttribute("readonly", "");
        ta.style.position = "fixed";
        ta.style.left = "-9999px";
        doc.body.appendChild(ta);
        ta.select();
        var ok = doc.execCommand("copy");
        doc.body.removeChild(ta);
        done(ok);
      } catch (e) { done(false); }
    }

    function messageNode(msg, index) {
      var article = doc.createElement("article");
      article.className = "msg msg-" + msg.role;
      article.setAttribute("data-index", String(index));

      if (msg.role === "assistant") {
        var avatar = doc.createElement("div");
        avatar.className = "msg-avatar";
        avatar.setAttribute("aria-hidden", "true");
        avatar.textContent = "G";
        article.appendChild(avatar);
      }

      var body = doc.createElement("div");
      body.className = "msg-body";

      var bubble = doc.createElement("div");
      bubble.className = "msg-bubble";

      if (msg.role === "assistant") {
        var content = doc.createElement("div");
        content.className = "md";
        /* renderMarkdown escapes every character of `msg.content` before it
           emits any tag — no raw model text is ever parsed as HTML. */
        content.innerHTML = msg.content ? renderMarkdown(msg.content) : "";
        if (!msg.content) {
          var dots = doc.createElement("span");
          dots.className = "typing";
          dots.innerHTML = "<i></i><i></i><i></i>";
          content.appendChild(dots);
        }
        bubble.appendChild(content);
      } else {
        var p = doc.createElement("div");
        p.className = "msg-text";
        p.textContent = msg.content;           /* user text: TEXT node only */
        bubble.appendChild(p);
      }

      if (msg.error) {
        var err = doc.createElement("p");
        err.className = "msg-error";
        err.textContent = msg.error;
        bubble.appendChild(err);
      }

      body.appendChild(bubble);

      var meta = metaLine(msg);
      if (meta) {
        var metaEl = doc.createElement("p");
        metaEl.className = "msg-meta";
        metaEl.textContent = meta;
        body.appendChild(metaEl);
      }

      var actions = doc.createElement("div");
      actions.className = "msg-actions";

      var copyBtn = doc.createElement("button");
      copyBtn.type = "button";
      copyBtn.className = "msg-action";
      copyBtn.textContent = "Copy";
      copyBtn.disabled = !msg.content;
      copyBtn.addEventListener("click", function () { copyText(msg.content || "", copyBtn); });
      actions.appendChild(copyBtn);

      if (msg.role === "assistant") {
        var regen = doc.createElement("button");
        regen.type = "button";
        regen.className = "msg-action";
        regen.textContent = "Regenerate";
        regen.disabled = busy;
        regen.addEventListener("click", function () { regenerate(index); });
        actions.appendChild(regen);
      }

      body.appendChild(actions);
      article.appendChild(body);
      return { article: article, contentEl: bubble.querySelector(".md") };
    }

    function renderMessages(conv) {
      el.messages.textContent = "";
      activeMsgNode = null;
      if (!conv || !conv.messages.length) {
        el.empty.hidden = false;
        el.messages.hidden = true;
        return;
      }
      el.empty.hidden = true;
      el.messages.hidden = false;
      conv.messages.forEach(function (msg, index) {
        var built = messageNode(msg, index);
        el.messages.appendChild(built.article);
        if (index === conv.messages.length - 1 && msg.role === "assistant") {
          activeMsgNode = built.contentEl;
        }
      });
      scrollToBottom(true);
    }

    function render() {
      renderConversations();
      renderModels();
      renderMessages(activeConversation());
      if (el.exportBtn) el.exportBtn.disabled = !activeConversation();
    }

    /* ---------- transport ---------- */

    function payloadFor(conv, stream) {
      var history = conv.messages
        .filter(function (m) { return !m.error && String(m.content || "").trim() !== ""; })
        .map(function (m) { return { role: m.role, content: m.content }; });
      return {
        model: conv.model || state.model,
        messages: history,
        stream: !!stream,
        max_tokens: 1024
      };
    }

    function ApiFailure(status, body) {
      this.name = "ApiFailure";
      this.status = status;
      this.body = body;
      this.message = errorMessage(status, body);
    }
    ApiFailure.prototype = Object.create(Error.prototype);
    ApiFailure.prototype.constructor = ApiFailure;

    async function requestCompletion(conv, stream, signal) {
      var res = await fetch(API_URL, {
        method: "POST",
        headers: { "Content-Type": "application/json", accept: stream ? "text/event-stream" : "application/json" },
        body: JSON.stringify(payloadFor(conv, stream)),
        signal: signal
      });
      if (!res.ok) {
        var text = "";
        try { text = await res.text(); } catch (e) { text = ""; }
        throw new ApiFailure(res.status, text);
      }
      return res;
    }

    /* Read an SSE body, calling onDelta for each token. Returns chunk count. */
    async function readStream(res, onDelta) {
      var reader = res.body.getReader();
      var decoder = new TextDecoder();
      var parser = new SSEParser();
      var count = 0;

      function handle(events) {
        for (var i = 0; i < events.length; i++) {
          var ev = events[i];
          if (ev.type === "data") {
            var piece = chunkText(ev.data);
            if (piece) { count++; onDelta(piece); }
            if (ev.data && ev.data.usage) onDelta(null, ev.data);
          } else if (ev.type === "done") {
            onDelta(null, null, true);
          }
        }
      }

      for (;;) {
        var step = await reader.read();
        if (step.done) break;
        handle(parser.push(decoder.decode(step.value, { stream: true })));
      }
      handle(parser.flush());
      return count;
    }

    /* ---------- one assistant turn ---------- */

    async function runTurn(conv, assistantMsg) {
      var started = Date.now();
      abortCtrl = new AbortController();
      var signal = abortCtrl.signal;
      var timeoutHit = false;

      var timeoutId = setTimeout(function () {
        timeoutHit = true;
        try { abortCtrl.abort(); } catch (e) {}
      }, CLIENT_TIMEOUT_MS);

      setBusy(true, "Waiting for a GPU\u2026 0 s");

      var tick = setInterval(function () {
        var secs = Math.round((Date.now() - started) / 1000);
        if (secs >= 20) {
          setNote("Still generating\u2026 " + secs + " s. An on-chain job can take 30\u201390 s; " +
            "Stop keeps whatever has arrived.", "busy");
        } else {
          setNote("Waiting for a GPU\u2026 " + secs + " s. A cold testnet job needs block inclusion " +
            "plus model load.", "busy");
        }
      }, 1000);

      var usage = null;
      var usedModel = conv.model || state.model;

      function onDelta(piece, data, done) {
        if (data && data.usage) usage = data.usage;
        if (data && data.model) usedModel = data.model;
        if (!piece) return;
        assistantMsg.content += piece;
        if (activeMsgNode) {
          activeMsgNode.innerHTML = renderMarkdown(assistantMsg.content);
        }
        scrollToBottom(false);
      }

      try {
        var res = await requestCompletion(conv, true, signal);
        var ctype = (res.headers.get("content-type") || "").toLowerCase();
        var chunks = 0;

        if (ctype.indexOf("json") !== -1) {
          /* Proxy ignored `stream` — accept the full body. */
          var doc = await res.json();
          var text = chunkText(doc);
          if (doc && doc.usage) usage = doc.usage;
          if (doc && doc.model) usedModel = doc.model;
          if (text) { assistantMsg.content = text; chunks = 1; }
        } else if (res.body && typeof res.body.getReader === "function") {
          chunks = await readStream(res, onDelta);
        }

        /* No tokens at all: retry once without streaming. */
        if (!chunks && !assistantMsg.content) {
          var plain = await requestCompletion(conv, false, signal);
          var full = await plain.json();
          var fullText = chunkText(full);
          if (full && full.usage) usage = full.usage;
          if (full && full.model) usedModel = full.model;
          if (fullText) assistantMsg.content = fullText;
        }

        if (!assistantMsg.content) {
          assistantMsg.error = "The provider returned an empty answer. Try again with a different model.";
        }
      } catch (err) {
        var aborted = err && (err.name === "AbortError" || err.code === 20);
        if (aborted) {
          if (timeoutHit) {
            assistantMsg.error = timeoutMessage();
          } else {
            assistantMsg.stopped = true;
            if (!assistantMsg.content) assistantMsg.error = "Stopped before the model answered.";
          }
        } else if (err instanceof ApiFailure) {
          assistantMsg.error = err.message;
        } else {
          assistantMsg.error = "Could not reach gpncoin.ai/api. Check your connection and try again.";
        }
      } finally {
        clearTimeout(timeoutId);
        clearInterval(tick);
        abortCtrl = null;
        setBusy(false);
      }

      assistantMsg.ts = assistantMsg.ts || Date.now();
      assistantMsg.latencyMs = Date.now() - started;
      assistantMsg.model = usedModel;
      if (usage) assistantMsg.usage = usage;
      if (assistantMsg.stopped && assistantMsg.content) {
        assistantMsg.content += "\n\n_(stopped)_";
      }

      touch(conv);
      saveState();
      render();
      el.input.focus();
    }

    async function sendMessage(text) {
      if (busy) return;
      var content = String(text == null ? "" : text).trim();
      if (!content) return;

      var conv = activeConversation();
      if (!conv) conv = newConversation();
      conv.model = conv.model || state.model;

      conv.messages.push({ role: "user", content: content, ts: Date.now() });
      var assistantMsg = { role: "assistant", content: "", ts: Date.now(), model: conv.model };
      conv.messages.push(assistantMsg);
      touch(conv);
      saveState();

      el.input.value = "";
      autoGrow();
      stickToBottom = true;
      renderMessages(conv);
      activeMsgNode = el.messages.lastElementChild
        ? el.messages.lastElementChild.querySelector(".md")
        : null;

      await runTurn(conv, assistantMsg);
    }

    async function regenerate(index) {
      if (busy) return;
      var conv = activeConversation();
      if (!conv) return;
      var msg = conv.messages[index];
      if (!msg || msg.role !== "assistant") return;

      /* find the user turn this answer belongs to */
      var userIndex = -1;
      for (var i = index - 1; i >= 0; i--) {
        if (conv.messages[i].role === "user") { userIndex = i; break; }
      }
      if (userIndex === -1) return;

      conv.messages = conv.messages.slice(0, userIndex + 1);
      var assistantMsg = {
        role: "assistant",
        content: "",
        ts: Date.now(),
        model: conv.model || state.model
      };
      conv.messages.push(assistantMsg);
      touch(conv);
      saveState();
      stickToBottom = true;
      renderMessages(conv);
      activeMsgNode = el.messages.lastElementChild
        ? el.messages.lastElementChild.querySelector(".md")
        : null;
      await runTurn(conv, assistantMsg);
    }

    /* ---------- composer ---------- */

    function autoGrow() {
      var ta = el.input;
      ta.style.height = "auto";
      var next = Math.min(ta.scrollHeight, 220);
      ta.style.height = next + "px";
    }

    el.input.addEventListener("input", autoGrow);

    el.input.addEventListener("keydown", function (ev) {
      if (ev.key === "Enter" && !ev.shiftKey && !ev.isComposing) {
        ev.preventDefault();
        if (!busy) sendMessage(el.input.value);
      }
    });

    el.composer.addEventListener("submit", function (ev) {
      ev.preventDefault();
      if (!busy) sendMessage(el.input.value);
    });

    el.stop.addEventListener("click", function () {
      if (!abortCtrl) return;
      try { abortCtrl.abort(); } catch (e) {}
      setNote("Stopping\u2026", "busy");
    });

    el.newChat.addEventListener("click", function () {
      if (busy) return;
      state.activeId = null;
      saveState();
      render();
      closeSidebar();
      el.input.focus();
    });

    el.modelSelect.addEventListener("change", function () {
      state.model = el.modelSelect.value;
      var conv = activeConversation();
      if (conv && !conv.messages.length) conv.model = state.model;
      saveState();
      renderModels();
      render();
    });

    el.exportBtn.addEventListener("click", function () {
      var conv = activeConversation();
      if (!conv) return;
      var markdown = conversationToMarkdown(conv);
      var blob = new Blob([markdown], { type: "text/markdown;charset=utf-8" });
      var url = URL.createObjectURL(blob);
      var a = doc.createElement("a");
      a.href = url;
      a.download = "gpn-chat-" + new Date().toISOString().slice(0, 10) + ".md";
      doc.body.appendChild(a);
      a.click();
      doc.body.removeChild(a);
      setTimeout(function () { URL.revokeObjectURL(url); }, 2000);
    });

    el.clearAll.addEventListener("click", function () {
      if (busy) return;
      var ok = root.confirm("Delete every conversation saved in this browser? This cannot be undone.");
      if (!ok) return;
      state.conversations = [];
      state.activeId = null;
      saveState();
      render();
    });

    /* ---------- mobile sidebar ---------- */

    function openSidebar() {
      el.sidebar.classList.add("open");
      el.backdrop.hidden = false;
      el.sidebarToggle.setAttribute("aria-expanded", "true");
    }
    function closeSidebar() {
      el.sidebar.classList.remove("open");
      el.backdrop.hidden = true;
      el.sidebarToggle.setAttribute("aria-expanded", "false");
    }
    el.sidebarToggle.addEventListener("click", function () {
      if (el.sidebar.classList.contains("open")) closeSidebar(); else openSidebar();
    });
    el.backdrop.addEventListener("click", closeSidebar);
    doc.addEventListener("keydown", function (ev) {
      if (ev.key === "Escape") closeSidebar();
    });

    /* ---------- scrolling ---------- */

    (el.scroll || el.messages).addEventListener("scroll", function () {
      stickToBottom = nearBottom();
    });

    /* ---------- starters ---------- */

    var STARTERS = [
      "Explain what a GPU-native compute network is, in two sentences.",
      "Write a haiku about idle GPUs earning their keep.",
      "Show me a curl command that calls this API."
    ];

    STARTERS.forEach(function (prompt) {
      var b = doc.createElement("button");
      b.type = "button";
      b.className = "starter";
      b.textContent = prompt;
      b.addEventListener("click", function () {
        el.input.value = prompt;
        autoGrow();
        sendMessage(prompt);
      });
      el.starters.appendChild(b);
    });

    /* ---------- first paint ---------- */

    render();
    loadModels();
    autoGrow();
    if (el.status) {
      el.status.textContent = "Testnet \u00b7 free \u00b7 open models";
    }
  }

  if (typeof document !== "undefined") {
    if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", boot);
    } else {
      boot();
    }
  }

  return {
    escapeHtml: escapeHtml,
    safeUrl: safeUrl,
    renderInline: renderInline,
    renderMarkdown: renderMarkdown,
    SSEParser: SSEParser,
    chunkText: chunkText,
    deriveTitle: deriveTitle,
    pruneConversations: pruneConversations,
    conversationToMarkdown: conversationToMarkdown,
    errorMessage: errorMessage,
    timeoutMessage: timeoutMessage,
    MAX_CONVERSATIONS: MAX_CONVERSATIONS,
    MAX_MESSAGES: MAX_MESSAGES,
    STORE_KEY: STORE_KEY,
    modelLabel: modelLabel,
    modelHint: modelHint
  };
});
