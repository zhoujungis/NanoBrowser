/* shared helpers for nano:// internal pages */
window.Nano = (() => {
  const invoke = window.__TAURI__.core.invoke;
  const listen = window.__TAURI__.event.listen;
  const THEME_KEY = "nano.theme";

  // 应用主题（同步），并缓存到 localStorage 供下一个内部页先行绘制，避免闪白
  function apply(dark) {
    document.documentElement.dataset.theme = dark ? "dark" : "light";
    try { localStorage.setItem(THEME_KEY, dark ? "dark" : "light"); } catch (e) { /* storage unavailable */ }
  }

  // 脚本一开始就用缓存值同步上主题；随后 boot() 拿到真实设置再修正
  try {
    const cached = localStorage.getItem(THEME_KEY);
    if (cached) apply(cached === "dark");
  } catch (e) { /* ignore */ }

  function esc(s) {
    return String(s ?? "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
  }

  async function boot() {
    const st = await invoke("get_initial_state");
    apply(st.resolvedDark);
    return st;
  }

  listen("theme-changed", (e) => {
    if (e.payload && typeof e.payload.dark !== "undefined") apply(e.payload.dark);
  });

  return {
    invoke, listen, esc, boot,
    tabId: () => window.__NANO_TAB_ID__ || 0,
    favicon: (u) => { try { return new URL(u).origin + "/favicon.ico"; } catch (e) { return ""; } },
    host: (u) => { try { return new URL(u).hostname.replace(/^www\./, ""); } catch (e) { return u; } },
  };
})();
