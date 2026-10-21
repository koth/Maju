// The page-side half of the panel driver's eval protocol.
//
// The Rust side (`browser_panel/driver.rs`) replaces the two placeholders and
// injects the result into a panel page:
//
// - the script placeholder ← the script SOURCE to run (an expression; awaited)
// - the call id placeholder ← a JSON string literal naming the waiting call
//
// The outcome travels back through the Tauri IPC bridge every webview carries,
// as `{ ok: true, value }` or `{ ok: false, error }`. A page that never runs
// this (closed, navigated mid-flight) simply never answers; the caller's
// timeout covers that.
//
// This file is real, parseable JavaScript on its own — the UI tests run it,
// so a syntax error here fails a test instead of silently timing out in the
// app. It deliberately embeds the script directly rather than eval()ing a
// string: evaluateJavaScript is privileged, but an eval() inside it would be
// subject to the page's CSP, and strict sites forbid eval.
(function () {
  var __respond = function (payload) {
    try {
      window.__TAURI_INTERNALS__.invoke("browser_panel_eval_result", {
        callId: __KODEX_CALL_ID__,
        payload: payload,
      });
    } catch (e) {
      // IPC 桥不在(理论上不可能:每个 webview 都注入了):只能让调用方超时。
    }
  };
  (async function () {
    try {
      var __value = await (__KODEX_SCRIPT__);
      __respond({ ok: true, value: __value === undefined ? null : __value });
    } catch (e) {
      __respond({ ok: false, error: String(e && (e.message || e)) });
    }
  })();
})();
