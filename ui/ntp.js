(() => {
  const { invoke, esc, tabId, favicon, host } = window.Nano;
  const q = document.getElementById("q");
  const tiles = document.getElementById("tiles");

  const DEFAULTS = [
    { url: "https://www.bing.com", title: "Bing" },
    { url: "https://www.baidu.com", title: "百度" },
    { url: "https://github.com", title: "GitHub" },
    { url: "https://www.bilibili.com", title: "哔哩哔哩" },
    { url: "https://www.zhihu.com", title: "知乎" },
    { url: "https://mail.qq.com", title: "QQ邮箱" },
    { url: "https://www.taobao.com", title: "淘宝" },
    { url: "https://translate.google.com", title: "翻译" },
  ];

  function nav(input) {
    invoke("internal_nav", { id: tabId(), input });
  }

  q.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && q.value.trim()) nav(q.value.trim());
  });

  function renderTile(t) {
    const d = document.createElement("div");
    d.className = "ntp-tile";
    const ic = document.createElement("div");
    ic.className = "tile-ic";
    if (t.url) {
      const img = document.createElement("img");
      // the icon the site reported when it was visited; the guessed
      // <origin>/favicon.ico is only the fallback — it 404s on most sites
      img.src = t.icon || favicon(t.url);
      img.onerror = () => { ic.textContent = (t.title || host(t.url) || "?")[0].toUpperCase(); };
      ic.appendChild(img);
    } else {
      ic.textContent = (t.title || "?")[0].toUpperCase();
    }
    const name = document.createElement("div");
    name.className = "tile-name";
    name.textContent = t.title || host(t.url);
    d.appendChild(ic);
    d.appendChild(name);
    d.addEventListener("click", () => nav(t.url));
    tiles.appendChild(d);
  }

  window.Nano.boot().then(async (st) => {
    q.placeholder = st.settings.searchEngine === "baidu" ? "百度一下" : "搜索或输入网址";
    let sites = [];
    try {
      sites = await invoke("get_top_sites", {});
    } catch (e) { /* empty */ }
    // most-visited history first, then the built-in shortcuts to fill the grid
    const seen = new Set();
    const list = [];
    const push = (t) => {
      if (!t || !t.url) return;
      let h = t.url;
      try { h = new URL(t.url).hostname.replace(/^www\./, ""); } catch (e) { /* keep */ }
      if (seen.has(h)) return;
      seen.add(h);
      list.push(t);
    };
    (Array.isArray(sites) ? sites : []).forEach(push);
    DEFAULTS.forEach(push);
    list.slice(0, 8).forEach((s) => renderTile(s));
  });
})();
