/* ============================================================
   GPN referral links — pure, testable helpers.

   Loaded by download.html / index.html before assets/app.js and used by
   `wireReferral()` there. Also require()-able from Node so the validation
   rule can be unit-tested without a browser (website/tests/referral.test.js).

   SECURITY: a referral code arrives from the URL, so it is untrusted input.
   It is validated against a strict allowlist and only ever written with
   `textContent` (never innerHTML, never attribute interpolation). A value
   like `"><script>` is simply not a valid code and is dropped.
   ============================================================ */
(function (root, factory) {
  if (typeof module === "object" && module.exports) {
    module.exports = factory();
  } else {
    root.GPN = root.GPN || {};
    root.GPN.referral = factory();
  }
})(typeof globalThis !== "undefined" ? globalThis : this, function () {
  "use strict";

  var MIN = 4;
  var MAX = 32;
  /* The one and only validation rule: 4-32 chars of [A-Za-z0-9_-], whole
     string. Same rule as gpn-agent/src/referral.rs — except that a URL value
     is never trimmed: leading/trailing space means "not a code", and is
     dropped rather than guessed at. */
  var RE = /^[A-Za-z0-9_-]{4,32}$/;

  /* Return the code, or null. No trimming games: a code with spaces is not a
     code (the Rust side trims, but a URL link should never carry them). */
  function validate(value) {
    if (typeof value !== "string") return null;
    return RE.test(value) ? value : null;
  }

  /* `?ref=CODE` or `?r=CODE`. Anything invalid (wrong length, bad char,
     percent-encoded payload, `<script>`) yields null and is ignored. */
  function fromSearch(search) {
    var params;
    try {
      params = new URLSearchParams(search || "");
    } catch (e) {
      return null;
    }
    var raw = params.get("ref");
    if (raw === null) raw = params.get("r");
    return validate(raw);
  }

  /* The exact text appended to an install command. Empty for no code, so a
     page with no referral renders byte-for-byte the command it always had. */
  function argFor(code) {
    var ok = validate(code);
    return ok ? " --ref " + ok : "";
  }

  /* Write the argument into an element as TEXT. Deliberately the only DOM
     write this module performs, and deliberately textContent: the code can
     never become markup. */
  function renderInto(el, code) {
    if (!el) return "";
    var arg = argFor(code);
    el.textContent = arg;
    return arg;
  }

  return {
    MIN: MIN,
    MAX: MAX,
    RE: RE,
    validate: validate,
    fromSearch: fromSearch,
    argFor: argFor,
    renderInto: renderInto
  };
});
