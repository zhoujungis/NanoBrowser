/* NanoBrowser shell */
(() => {
  const T = window.__TAURI__;
  const invoke = T.core.invoke;
  const listen = T.event.listen;
  const win = T.window.getCurrentWindow();

  const $ = (id) => document.getElementById(id);
  const el = {
    tabs: $("tabs"), newtab: $("newtab"), strip: $("tabstrip"),
    winMin: $("win-min"), winMax: $("win-max"), winClose: $("win-close"),
    back: $("btn-back"), fwd: $("btn-fwd"), reload: $("btn-reload"), home: $("btn-home"),
    omnibox: $("omnibox"), sec: $("omni-sec"), input: $("omni-input"), star: $("omni-star"), drop: $("omni-drop"),
    dl: $("btn-downloads"), menuBtn: $("btn-menu"),
    bmbar: $("bookmarkbar"), findbar: $("findbar"), findInput: $("find-input"), findCount: $("find-count"),
    menuLayer: $("menu-layer"), mainMenu: $("main-menu"), reloadMenu: $("reload-menu"), tabMenu: $("tab-menu"), bmMenu: $("bm-menu"), bmOverflow: $("bm-overflow"),
  };

  /* ---------------- state ---------------- */
  const S = {
    tabs: [], active: 0, settings: null, bookmarks: [], sysDark: false,
    omniFocused: false, sug: [], sugSel: -1,
    activeUrl: "", canBack: false, canFwd: false, loading: false, starred: false,
    findOpen: false, findId: 0,
    maximized: false,
  };

  const IC = {
    newtab: "i-globe", history: "i-history", bookmarks: "i-bookmark",
    downloads: "i-downloads", settings: "i-settings", about: "i-info",
  };
  const iconFor = (url) => {
    const key = url.replace(/\/$/, "");
    return IC[key] || "i-globe";
  };
  // `cls` must be carried through: some icons are sized by a class on the <svg>
  // itself (e.g. #winctl svg.cap), and dropping it leaves the glyph unstyled.
  const svgUse = (sym, cls) => `<svg${cls ? ` class="${cls}"` : ""}><use href="#${sym}"/></svg>`;

  /* ---------------- theme ---------------- */
  function resolvedDark() {
    if (!S.settings) return S.sysDark;
    return S.settings.theme === "dark" || (S.settings.theme === "system" && S.sysDark);
  }
  function applyTheme() {
    document.documentElement.dataset.theme = resolvedDark() ? "dark" : "light";
  }

  /* ---------------- tabs ---------------- */
  function tabIconHtml(t) {
    if (t.loading) return `<span class="spinner"></span>`;
    if (t.url.startsWith("nano://")) {
      return `<span class="tab-icon nano">${svgUse(iconFor(t.url))}</span>`;
    }
    if (t.icon) {
      // escaped: the value comes from whatever the page reported.
      // the class is applied before the swap, while the parent still exists
      return `<span class="tab-icon"><img src="${esc(t.icon)}" onerror="this.parentElement.classList.add('nano');this.parentElement.innerHTML='${svgUse("i-globe").replace(/"/g, "&quot;")}'"></span>`;
    }
    return `<span class="tab-icon nano">${svgUse("i-globe")}</span>`;
  }

  function renderTabs() {
    el.tabs.innerHTML = "";
    S.tabs.forEach((t, i) => {
      if (i > 0 && !S.tabs[i - 1].pinned && !t.pinned) {
        // separator between adjacent non-pinned tabs when neither is active
        const prevActive = S.tabs[i - 1].id === S.active;
        const curActive = t.id === S.active;
        if (!prevActive && !curActive) {
          const sep = document.createElement("div");
          sep.className = "tab-sep";
          el.tabs.appendChild(sep);
        }
      }
      const d = document.createElement("div");
      d.className = "tab" + (t.id === S.active ? " active" : "") + (t.pinned ? " pinned" : "");
      d.dataset.id = t.id;
      d.innerHTML = tabIconHtml(t) + `<span class="tab-title">${esc(t.title || "")}</span>` +
        `<button class="tab-close" title="关闭标签页">${svgUse("i-close")}</button>`;
      d.addEventListener("click", (e) => {
        if (e.target.closest(".tab-close")) return;
        if (suppressClick) return;
        if (e.button === 0) invoke("activate", { id: t.id });
      });
      // middle click closes the tab, like Chrome (preventDefault also suppresses
      // the middle-click autoscroll that Chromium would otherwise start)
      d.addEventListener("mousedown", (e) => {
        if (e.button === 1) { e.preventDefault(); invoke("close_tab_cmd", { id: t.id }); }
      });
      d.addEventListener("contextmenu", (e) => {
        e.preventDefault();
        openTabMenu(e, t);
      });
      // drag reorder
      d.addEventListener("pointerdown", (e) => startTabDrag(e, d, t));
      const cb = d.querySelector(".tab-close");
      cb.addEventListener("click", (e) => { e.stopPropagation(); invoke("close_tab_cmd", { id: t.id }); });
      el.tabs.appendChild(d);
    });
  }

  function esc(s) {
    return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
  }

  /* tab drag reorder — the dragged tab follows the cursor while the siblings
     slide aside (FLIP), so the strip always shows where the tab will land */
  let dragState = null;
  let suppressClick = false;

  const tabNodes = () => [...el.tabs.children].filter((n) => n.classList.contains("tab"));

  function startTabDrag(e, node, t) {
    if (e.button !== 0) return;
    // every press on a tab clears the flag, so a stale suppression can never
    // swallow the click that belongs to a *later* interaction
    suppressClick = false;
    dragState = {
      id: t.id,
      node,
      startX: e.clientX,
      grab: e.clientX - node.getBoundingClientRect().left,
      from: S.tabs.findIndex((x) => x.id === t.id),
      dragging: false,
    };
    window.addEventListener("pointermove", onTabDragMove);
    window.addEventListener("pointerup", onTabDragEnd);
    window.addEventListener("pointercancel", onTabDragEnd);
  }

  function onTabDragMove(ev) {
    const st = dragState;
    if (!st) return;
    if (!st.dragging) {
      if (Math.abs(ev.clientX - st.startX) < 6) return;
      st.dragging = true;
      st.node.classList.add("dragging");
      document.body.classList.add("tab-dragging");
      // separators are re-derived by renderTabs() once the tab lands
      [...el.tabs.querySelectorAll(".tab-sep")].forEach((n) => n.remove());
    }
    const others = tabNodes().filter((n) => n !== st.node);
    const before = others.map((n) => n.getBoundingClientRect().left);
    // the tab belongs before the first sibling whose midpoint it has not passed
    const centre = ev.clientX - st.grab + st.node.offsetWidth / 2;
    let at = others.length;
    for (let i = 0; i < others.length; i++) {
      const r = others[i].getBoundingClientRect();
      if (centre < r.left + r.width / 2) { at = i; break; }
    }
    const anchor = others[at] || null; // null appends after the last tab
    if (st.node.parentNode !== el.tabs || st.node.nextSibling !== anchor) {
      el.tabs.insertBefore(st.node, anchor);
      // animate the displaced siblings from their previous spot (FLIP)
      others.forEach((n, i) => {
        const d = before[i] - n.getBoundingClientRect().left;
        if (Math.abs(d) < 0.5) return;
        n.style.transition = "none";
        n.style.transform = `translateX(${d}px)`;
        void n.offsetWidth; // flush, then transition back to the natural spot
        n.style.transition = "transform 150ms cubic-bezier(.2,.7,.3,1)";
        n.style.transform = "";
      });
    }
    // keep the dragged tab under the cursor (offsets are read after the reflow)
    st.node.style.transform = "none";
    const base = st.node.getBoundingClientRect().left;
    st.node.style.transform = `translateX(${ev.clientX - st.grab - base}px)`;
  }

  function onTabDragEnd() {
    window.removeEventListener("pointermove", onTabDragMove);
    window.removeEventListener("pointerup", onTabDragEnd);
    window.removeEventListener("pointercancel", onTabDragEnd);
    const st = dragState;
    dragState = null;
    if (!st || !st.dragging) return;
    // the click that trails this pointerup belongs to the drag, not to the user
    // asking for that tab. It stays suppressed until the next tab press resets it.
    suppressClick = true;
    tabNodes().forEach((n) => { n.style.transition = ""; n.style.transform = ""; });
    st.node.classList.remove("dragging");
    document.body.classList.remove("tab-dragging");
    const to = tabNodes().indexOf(st.node);
    if (to >= 0 && to !== st.from) {
      invoke("move_tab", { id: st.id, to }); // Rust re-emits tabs-state
    } else {
      renderTabs(); // restore the separators we dropped while dragging
    }
    // like Chrome, releasing a dragged tab brings it to the front
    if (S.active !== st.id) invoke("activate", { id: st.id });
  }

  /* ---------------- omnibox ---------------- */
  function setSecIcon(url) {
    let sym = "i-lock";
    if (url.startsWith("nano://")) sym = iconFor(url);
    else if (url.startsWith("http://")) sym = "i-info";
    el.sec.innerHTML = svgUse(sym);
  }

  function displayUrl(url) {
    return url === "nano://newtab" ? "" : url;
  }

  function renderOmnibox() {
    if (S.omniFocused) return;
    el.input.value = displayUrl(S.activeUrl);
    setSecIcon(S.activeUrl);
    el.back.disabled = !S.canBack;
    el.fwd.disabled = !S.canFwd;
    const showStop = S.loading;
    el.reload.innerHTML = svgUse(showStop ? "i-close" : "i-reload");
    el.reload.title = showStop ? "停止加载" : "刷新 (F5)；右键查看更多";
    el.star.classList.toggle("starred", S.starred);
    el.star.innerHTML = svgUse(S.starred ? "i-star-fill" : "i-star");
    if (el.mainMenu.classList.contains("show")) renderZoomRow();
  }

  function suggest(q) {
    invoke("omnibox_suggest", { input: q }).then((items) => {
      S.sug = items || [];
      S.sugSel = -1;
      renderSug();
    });
  }

  function renderSug() {
    if (!S.sug.length || !S.omniFocused) { el.drop.innerHTML = ""; el.omnibox.classList.remove("open"); syncOverlay(); return; }
    const sym = { url: "i-globe", search: "i-search", history: "i-history", bookmark: "i-star-fill" };
    let html = "";
    S.sug.forEach((it, i) => {
      const sel = i === S.sugSel ? " selected" : "";
      const urlPart = it.kind === "search"
        ? `<span class="sug-url">搜索引擎</span>`
        : `<span class="sug-url">${esc(shortUrl(it.url))}</span>`;
      html += `<div class="sug${sel}" data-i="${i}">${svgUse(sym[it.kind] || "i-globe")}<span class="sug-text">${esc(it.text)}</span>${urlPart}<span class="sug-kind">${it.kind === "history" ? "历史记录" : it.kind === "bookmark" ? "书签" : ""}</span></div>`;
    });
    el.drop.innerHTML = html;
    el.omnibox.classList.add("open");
    syncOverlay();
    [...el.drop.children].forEach((n, i) => {
      n.addEventListener("mousedown", (e) => { e.preventDefault(); pickSug(i, e); });
    });
  }

  function shortUrl(u) {
    try { const h = new URL(u); return h.hostname.replace(/^www\./, ""); } catch (e) { return u; }
  }

  function pickSug(i, ev) {
    const it = S.sug[i];
    const input = it ? it.url : el.input.value;
    doNavigate(input, ev && ev.shiftKey);
  }

  function doNavigate(input, background) {
    closeSug();
    el.input.blur();
    if (background) invoke("new_tab", { input, background: true });
    else if (S.active) invoke("navigate", { id: S.active, input });
  }

  function closeSug() {
    S.sug = [];
    S.sugSel = -1;
    el.drop.innerHTML = "";
    el.omnibox.classList.remove("open");
    syncOverlay();
  }

  let sugTimer = null;
  el.input.addEventListener("focus", () => {
    S.omniFocused = true;
    el.omnibox.classList.add("focused");
    el.input.select();
    suggest(el.input.value);
  });
  el.input.addEventListener("blur", () => {
    S.omniFocused = false;
    el.omnibox.classList.remove("focused");
    setTimeout(closeSug, 120);
    renderOmnibox();
  });
  el.input.addEventListener("input", () => {
    clearTimeout(sugTimer);
    sugTimer = setTimeout(() => suggest(el.input.value), 110);
  });
  el.input.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown" && S.sug.length) { e.preventDefault(); S.sugSel = (S.sugSel + 1) % S.sug.length; renderSug(); }
    else if (e.key === "ArrowUp" && S.sug.length) { e.preventDefault(); S.sugSel = (S.sugSel - 1 + S.sug.length) % S.sug.length; renderSug(); }
    else if (e.key === "Escape") { closeSug(); el.input.blur(); renderOmnibox(); }
    else if (e.key === "Enter") {
      e.preventDefault();
      const it = S.sugSel >= 0 ? S.sug[S.sugSel] : null;
      doNavigate(it ? it.url : el.input.value, e.altKey);
    }
  });

  /* ---------------- toolbar ---------------- */
  el.back.addEventListener("click", () => S.active && invoke("go_back", { id: S.active }));
  el.fwd.addEventListener("click", () => S.active && invoke("go_forward", { id: S.active }));
  // like Chrome: a plain click reloads, shift-click bypasses the HTTP cache
  el.reload.addEventListener("click", (e) => {
    if (!S.active) return;
    if (e.shiftKey && !S.loading) { invoke("hard_reload", { id: S.active }); return; }
    invoke(S.loading ? "stop" : "reload", { id: S.active });
  });
  // ...and the button's own right-click menu carries the reload variants
  el.reload.addEventListener("contextmenu", (e) => {
    e.preventDefault();
    openReloadMenu(e.clientX, e.clientY);
  });
  el.home.addEventListener("click", () => {
    if (S.active && S.settings) invoke("navigate", { id: S.active, input: S.settings.homepage });
  });
  el.star.addEventListener("click", () => {
    if (!S.active) return;
    if (S.starred) invoke("remove_bookmark", { url: S.activeUrl });
    else {
      const title = (S.tabs.find((t) => t.id === S.active)?.title) || "";
      invoke("add_bookmark", { url: S.activeUrl, title });
    }
  });
  el.dl.addEventListener("click", () => S.active && invoke("navigate", { id: S.active, input: "nano://downloads" }));
  el.newtab.addEventListener("click", () => invoke("new_tab", {}));

  el.sec.addEventListener("click", () => { el.input.focus(); el.input.select(); });

  /* ---------------- window controls ---------------- */
  el.winMin.addEventListener("click", () => win.minimize());
  el.winMax.addEventListener("click", async () => {
    await win.toggleMaximize();
    S.maximized = await win.isMaximized();
    el.winMax.innerHTML = svgUse(S.maximized ? "i-restore" : "i-max", "cap");
    el.winMax.title = S.maximized ? "还原" : "最大化";
  });
  el.winClose.addEventListener("click", () => win.close());

  // double-clicking empty strip space toggles maximize, like Chrome
  el.strip.addEventListener("dblclick", (e) => {
    if (e.target.closest(".tab") || e.target.closest("#newtab") || e.target.closest("#winctl")) return;
    el.winMax.click();
  });

  /* ---------------- menus ---------------- */
  // The shell webview only covers the chrome strip (40+44+28 px). Menus and the
  // omnibox suggestion list overflow it, so while either is open we ask Rust to
  // grow the shell webview to the full window and raise it above the page.
  let overlayOn = false;
  let overlayQueued = false;
  function syncOverlay() {
    if (overlayQueued) return;
    overlayQueued = true;
    requestAnimationFrame(() => {
      overlayQueued = false;
      const want = !el.menuLayer.hidden || el.omnibox.classList.contains("open");
      if (overlayOn === want) return;
      overlayOn = want;
      invoke("chrome_overlay", { active: want });
    });
  }

  let curMenu = null;
  let curMenuXY = null;
  let revealTimer = null;

  function placeMenu() {
    if (!curMenu) return;
    const r = curMenu.getBoundingClientRect();
    curMenu.style.left = Math.max(4, Math.min(curMenuXY[0], window.innerWidth - r.width - 8)) + "px";
    curMenu.style.top = Math.max(4, Math.min(curMenuXY[1], window.innerHeight - r.height - 8)) + "px";
  }
  // the menu is kept invisible until the shell webview has been grown, so it
  // never flashes at a position clamped by the 112px chrome strip
  function revealMenu() {
    clearTimeout(revealTimer);
    revealTimer = null;
    if (!curMenu) return;
    placeMenu();
    curMenu.style.visibility = "";
  }

  function clearMenus() {
    clearTimeout(revealTimer);
    revealTimer = null;
    curMenu = null;
    el.menuLayer.hidden = true;
    [el.mainMenu, el.reloadMenu, el.tabMenu, el.bmMenu, el.bmOverflow].forEach((m) => {
      m.hidden = true;
      m.classList.remove("show");
      m.style.visibility = "";
    });
  }
  function closeMenus() {
    clearMenus();
    syncOverlay();
  }

  function openMenu(menuEl, x, y) {
    clearMenus();
    el.menuLayer.hidden = false;
    menuEl.hidden = false;
    menuEl.classList.add("show");
    menuEl.style.visibility = "hidden";
    curMenu = menuEl;
    curMenuXY = [x, y];
    syncOverlay();
    revealTimer = setTimeout(revealMenu, 90);
  }
  el.menuLayer.addEventListener("mousedown", (e) => { if (e.target === el.menuLayer) closeMenus(); });
  el.menuLayer.addEventListener("contextmenu", (e) => { if (e.target === el.menuLayer) { e.preventDefault(); closeMenus(); } });

  const row = (icon, label, hint, onClick, extra) => {
    const d = document.createElement("div");
    d.className = "menu-row";
    d.innerHTML = `${icon ? svgUse(icon) : "<span style='width:17px'></span>"}<span class="mr-label">${label}</span>${hint ? `<span class="mr-hint">${hint}</span>` : ""}${extra || ""}`;
    d.addEventListener("click", () => { closeMenus(); onClick(); });
    return d;
  };
  const divider = () => { const d = document.createElement("div"); d.className = "menu-divider"; return d; };

  function renderZoomRow() {
    const zoomRow = $("menu-zoom-row");
    if (!zoomRow) return;
    const zr = $("menu-zoom-val");
    const z = S.tabs.find((t) => t.id === S.active)?.zoom ?? 1;
    if (zr) zr.textContent = Math.round(z * 100) + "%";
  }

  // the visible zoom value is updated from the command's return value, so the
  // menu does not show a stale percentage until the next tabs-state round trip
  function applyZoom(delta) {
    if (!S.active) return;
    invoke("zoom", { id: S.active, delta }).then((z) => {
      if (typeof z !== "number") return;
      const t = S.tabs.find((x) => x.id === S.active);
      if (t) t.zoom = z;
      renderZoomRow();
    });
  }

  el.menuBtn.addEventListener("click", () => {
    if (!el.mainMenu.hidden) { closeMenus(); return; }
    el.mainMenu.innerHTML = "";
    el.mainMenu.appendChild(row("i-plus", "新建标签页", "Ctrl+T", () => invoke("new_tab", {})));
    el.mainMenu.appendChild(divider());
    el.mainMenu.appendChild(row("i-history", "历史记录", "Ctrl+H", () => S.active && invoke("navigate", { id: S.active, input: "nano://history" })));
    el.mainMenu.appendChild(row("i-downloads", "下载内容", "Ctrl+J", () => S.active && invoke("navigate", { id: S.active, input: "nano://downloads" })));
    el.mainMenu.appendChild(row("i-bookmark", "书签管理", "Ctrl+Shift+O", () => S.active && invoke("navigate", { id: S.active, input: "nano://bookmarks" })));
    el.mainMenu.appendChild(divider());
    const zoom = document.createElement("div");
    zoom.className = "menu-zoom";
    zoom.id = "menu-zoom-row";
    zoom.innerHTML = `<button class="tbtn" id="mz-minus">${svgUse("i-minus")}</button><span class="mz-val" id="menu-zoom-val"></span><button class="tbtn" id="mz-plus">${svgUse("i-plus")}</button>`;
    el.mainMenu.appendChild(zoom);
    el.mainMenu.appendChild(divider());
    el.mainMenu.appendChild(row("i-settings", "设置", "", () => S.active && invoke("navigate", { id: S.active, input: "nano://settings" })));
    el.mainMenu.appendChild(row("i-info", "关于 NanoBrowser", "", () => S.active && invoke("navigate", { id: S.active, input: "nano://about" })));
    el.mainMenu.appendChild(divider());
    el.mainMenu.appendChild(row("i-code", "开发者工具", "F12", () => S.active && invoke("toggle_devtools", { id: S.active })));
    // the dock edge is a persisted setting, so the row carries a check for the
    // active one and switching re-docks the panel (a reload of the panel itself)
    const dockRow = (label, val) => {
      const d = row(dockSide() === val ? "i-check" : null, label, "", () => setDockSide(val));
      d.classList.add("mr-sub");
      return d;
    };
    el.mainMenu.appendChild(dockRow("停靠：底部", "bottom"));
    el.mainMenu.appendChild(dockRow("停靠：右侧", "right"));
    const r = el.menuBtn.getBoundingClientRect();
    openMenu(el.mainMenu, r.right - 300, r.bottom + 4);
    renderZoomRow();
    zoom.querySelector("#mz-minus").addEventListener("click", () => applyZoom(-1));
    zoom.querySelector("#mz-plus").addEventListener("click", () => applyZoom(1));
  });

  // Chrome's reload drop-down: the two hard variants differ only in whether the
  // profile-wide HTTP cache is dropped before the cache-bypassing reload
  function openReloadMenu(x, y) {
    el.reloadMenu.innerHTML = "";
    el.reloadMenu.appendChild(row("i-reload", "重新加载", "F5", () => invoke("reload", { id: S.active })));
    el.reloadMenu.appendChild(row(null, "强制重新加载", "Ctrl+Shift+R", () => invoke("hard_reload", { id: S.active })));
    el.reloadMenu.appendChild(row(null, "清空缓存并硬性重新加载", "", () => invoke("clear_cache_reload", { id: S.active })));
    openMenu(el.reloadMenu, x, y);
  }

  function openTabMenu(e, t) {
    el.tabMenu.innerHTML = "";
    el.tabMenu.appendChild(row("i-pin", t.pinned ? "取消固定" : "固定标签页", "", () => invoke("toggle_pin", { id: t.id })));
    el.tabMenu.appendChild(row("i-newtab", "复制标签页", "", () => invoke("duplicate_tab", { id: t.id })));
    el.tabMenu.appendChild(divider());
    el.tabMenu.appendChild(row("i-close", "关闭标签页", "Ctrl+W", () => invoke("close_tab_cmd", { id: t.id })));
    el.tabMenu.appendChild(row(null, "关闭其他标签页", "", () => invoke("close_others", { id: t.id })));
    el.tabMenu.appendChild(row(null, "关闭右侧标签页", "", () => invoke("close_to_right", { id: t.id })));
    openMenu(el.tabMenu, e.clientX, e.clientY);
  }

  /* ---------------- bookmarks bar ---------------- */
  // the stored favicon is the one the page itself reported. It is re-checked here
  // because the value ends up in an <img src>, and a failed or unknown image
  // degrades to the globe glyph instead of leaving an empty slot
  const GLOBE_ATTR = svgUse("i-globe").replace(/"/g, "&quot;");
  function bmIcon(b) {
    const raw = b.icon || "";
    if (/^https?:\/\//i.test(raw)) {
      return `<img src="${esc(raw)}" onerror="this.outerHTML='${GLOBE_ATTR}'">`;
    }
    return svgUse("i-globe");
  }
  // widths are measured, not guessed from the character count: a fixed
  // per-character estimate is far off for latin text and thin titles
  const bmTextWidth = (() => {
    const ctx = document.createElement("canvas").getContext("2d");
    return (title) => {
      ctx.font = "12px " + getComputedStyle(document.body).fontFamily;
      return ctx.measureText(title).width;
    };
  })();

  function renderBmBar() {
    el.bmbar.hidden = !(S.settings && S.settings.bookmarksBar);
    if (el.bmbar.hidden) return;
    el.bmbar.innerHTML = "";
    const barW = el.bmbar.clientWidth - 40;
    let used = 0;
    let overflowed = false;
    const overflowList = [];
    S.bookmarks.forEach((b) => {
      const d = document.createElement("div");
      d.className = "bm";
      const w = 24 + (b.title ? Math.min(bmTextWidth(b.title), 180) + 14 : 0);
      if (!overflowed && used + w < barW) {
        used += w;
        d.innerHTML = bmIcon(b) + `<span>${esc(b.title || b.url)}</span>`;
        d.addEventListener("click", () => S.active && invoke("navigate", { id: S.active, input: b.url }));
        d.addEventListener("contextmenu", (e) => { e.preventDefault(); openBmMenu(e, b); });
        el.bmbar.appendChild(d);
      } else {
        overflowed = true;
        overflowList.push(b);
      }
    });
    if (overflowList.length) {
      const btn = document.createElement("button");
      btn.id = "bm-overflow-btn";
      btn.innerHTML = svgUse("i-down2");
      btn.title = "更多书签";
      btn.addEventListener("click", (e) => {
        el.bmOverflow.innerHTML = "";
        overflowList.forEach((b) => {
          const d = document.createElement("div");
          d.className = "menu-bm";
          d.innerHTML = bmIcon(b) + `<span>${esc(b.title || b.url)}</span>`;
          d.addEventListener("click", () => { closeMenus(); S.active && invoke("navigate", { id: S.active, input: b.url }); });
          el.bmOverflow.appendChild(d);
        });
        const r = btn.getBoundingClientRect();
        openMenu(el.bmOverflow, r.right - 280, r.bottom + 2);
      });
      el.bmbar.appendChild(btn);
    }
  }
  function openBmMenu(e, b) {
    el.bmMenu.innerHTML = "";
    el.bmMenu.appendChild(row("i-newtab", "在新标签页中打开", "", () => invoke("new_tab", { input: b.url, background: false })));
    el.bmMenu.appendChild(divider());
    el.bmMenu.appendChild(row("i-trash", "删除书签", "", () => invoke("remove_bookmark", { url: b.url })));
    openMenu(el.bmMenu, e.clientX, e.clientY);
  }

  /* ---------------- find bar ---------------- */
  let findTimer = null;

  // the find bar follows the active tab: leaving it bound to the previous tab
  // would keep highlighting text in a page the user has left
  function syncFindbar() {
    if (!S.findOpen || S.findId === S.active) return;
    const prev = S.findId;
    S.findId = S.active;
    if (prev) invoke("find_close", { id: prev });
    const q = el.findInput.value.trim();
    if (S.active && q) invoke("find_query", { id: S.active, q });
    else el.findCount.textContent = "";
  }

  function openFindbar() {
    S.findOpen = true;
    S.findId = S.active;
    el.findbar.hidden = false;
    el.findInput.focus();
    el.findInput.select();
  }
  function closeFindbar() {
    if (!S.findOpen) return;
    S.findOpen = false;
    el.findbar.hidden = true;
    el.findInput.value = "";
    el.findCount.textContent = "";
    if (S.findId) invoke("find_close", { id: S.findId });
  }
  el.findInput.addEventListener("input", () => {
    clearTimeout(findTimer);
    findTimer = setTimeout(() => {
      if (S.active) invoke("find_query", { id: S.active, q: el.findInput.value });
    }, 180);
  });
  el.findInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") { e.preventDefault(); invoke("find_move", { id: S.active, delta: e.shiftKey ? -1 : 1 }); }
    else if (e.key === "Escape") { closeFindbar(); }
  });
  $("find-prev").addEventListener("click", () => invoke("find_move", { id: S.active, delta: -1 }));
  $("find-next").addEventListener("click", () => invoke("find_move", { id: S.active, delta: 1 }));
  $("find-close").addEventListener("click", closeFindbar);

  /* ---------------- shortcuts (shell has focus) ---------------- */
  // the shell webview itself has no DevTools; these toggle the active page's
  function toggleDevtools() {
    if (S.active) invoke("toggle_devtools", { id: S.active });
  }

  window.addEventListener("keydown", async (e) => {
    if (e.key === "F11") {
      e.preventDefault();
      try {
        const fs = await win.isFullscreen();
        await win.setFullscreen(!fs);
      } catch (err) { /* ignore */ }
      return;
    }
    if (e.key === "F12") {
      e.preventDefault();
      toggleDevtools();
      return;
    }
    if (e.key === "F5") {
      e.preventDefault();
      if (!S.active) return;
      if (e.shiftKey || e.ctrlKey || e.metaKey) { invoke("hard_reload", { id: S.active }); return; }
      invoke(S.loading ? "stop" : "reload", { id: S.active });
      return;
    }
    if (e.key === "Escape") {
      if (S.findOpen) { closeFindbar(); return; }
      if (!el.menuLayer.hidden) { closeMenus(); return; }
      closeSug();
      return;
    }
    const c = e.ctrlKey || e.metaKey;
    if (e.altKey && !c) {
      if (e.key === "ArrowLeft") { e.preventDefault(); S.active && invoke("go_back", { id: S.active }); }
      else if (e.key === "ArrowRight") { e.preventDefault(); S.active && invoke("go_forward", { id: S.active }); }
      return;
    }
    if (!c) return;
    const k = (e.key || "").toLowerCase();
    if (e.shiftKey && k === "t") { e.preventDefault(); invoke("reopen_tab_cmd", {}); return; }
    if (e.shiftKey && k === "i") { e.preventDefault(); toggleDevtools(); return; }
    if (e.shiftKey && k === "o") { e.preventDefault(); S.active && invoke("navigate", { id: S.active, input: "nano://bookmarks" }); return; }
    if (e.shiftKey && k === "j") { e.preventDefault(); S.active && invoke("navigate", { id: S.active, input: "nano://downloads" }); return; }
    if (e.shiftKey && k === "r") { e.preventDefault(); S.active && invoke("hard_reload", { id: S.active }); return; }
    switch (k) {
      case "t": e.preventDefault(); invoke("new_tab", {}); return;
      case "w": e.preventDefault(); S.active && invoke("close_tab_cmd", { id: S.active }); return;
      case "l": e.preventDefault(); el.input.focus(); return;
      case "f": e.preventDefault(); openFindbar(); return;
      case "d": e.preventDefault(); el.star.click(); return;
      case "h": e.preventDefault(); S.active && invoke("navigate", { id: S.active, input: "nano://history" }); return;
      case "j": e.preventDefault(); S.active && invoke("navigate", { id: S.active, input: "nano://downloads" }); return;
      case "tab": e.preventDefault(); cycleTab(!e.shiftKey); return;
      case "+": case "=": e.preventDefault(); applyZoom(1); return;
      case "-": case "_": e.preventDefault(); applyZoom(-1); return;
      case "0": e.preventDefault(); applyZoom(0); return;
    }
    if (k >= "1" && k <= "9") {
      e.preventDefault();
      const n = parseInt(k, 10);
      const idx = n === 9 ? S.tabs.length - 1 : n - 1;
      if (idx >= 0 && idx < S.tabs.length) invoke("activate", { id: S.tabs[idx].id });
    }
  });

  function cycleTab(forward) {
    if (!S.tabs.length) return;
    const cur = S.tabs.findIndex((t) => t.id === S.active);
    const next = ((forward ? cur + 1 : cur - 1) + S.tabs.length) % S.tabs.length;
    invoke("activate", { id: S.tabs[next].id });
  }

  /* ---------------- events from rust ---------------- */
  listen("tabs-state", () => {
    invoke("get_tabs").then((r) => {
      S.tabs = r.tabs || [];
      S.active = r.active || 0;
      syncFindbar();
      // while a drag is in flight the strip DOM is the user's own arrangement
      // (the dragged node is offset under the cursor) - rebuilding it would
      // destroy that node. The drop path always re-renders afterwards.
      if (dragState && dragState.dragging) return;
      renderTabs();
    });
  });
  listen("omnibox-state", (e) => {
    const d = e.payload;
    S.activeUrl = d.url; S.canBack = d.canBack; S.canFwd = d.canFwd;
    S.loading = d.loading; S.starred = d.starred;
    renderOmnibox();
  });
  listen("bookmarks-changed", () => {
    invoke("get_bookmarks").then((b) => { S.bookmarks = b || []; renderBmBar(); });
  });
  listen("theme-changed", (e) => {
    applyTheme();
    if (e.payload && typeof e.payload.dark !== "undefined") document.documentElement.dataset.theme = e.payload.dark ? "dark" : "light";
  });
  listen("sys-theme", (e) => { S.sysDark = !!e.payload.dark; applyTheme(); });
  listen("status-hover", (e) => { /* status bubble reserved */ });
  listen("find-result", (e) => {
    const d = e.payload;
    el.findCount.textContent = d && d.count ? `${d.index + 1}/${d.count}` : "";
  });
  listen("focus-omnibox", () => { el.input.focus(); el.input.select(); });
  listen("open-findbar", () => openFindbar());

  /* ---------------- docked devtools: which edge ---------------- */
  const dockSide = () => (S.settings && S.settings.devtools_side) || "bottom";
  function setDockSide(side) {
    if (!S.settings) S.settings = {};
    S.settings.devtools_side = side;
    invoke("set_devtools_side", { side });
  }

  /* ---------------- docked devtools: panel resize ---------------- */
  // The panel's grip lives inside the DevTools webview, which cannot see the
  // pointer once it leaves its own bounds. Rust grows this shell webview over the
  // whole window for the duration of the drag, so a catcher here keeps receiving
  // the moves and reports the pointer position back.
  let dtGrab = null;
  function dtDragStart() {
    if (dtGrab) return;
    dtGrab = document.createElement("div");
    dtGrab.id = "dt-grab";
    // the panel is docked to one axis only, so the catcher only reports that
    // axis' coordinate (`Rust` turns it into the panel's new size)
    dtGrab.classList.add(dockSide() === "right" ? "ew" : "ns");
    const move = (e) => invoke("set_devtools_size", { x: e.clientX, y: e.clientY });
    const end = () => {
      if (!dtGrab) return;
      dtGrab.remove();
      dtGrab = null;
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", end);
      window.removeEventListener("blur", end);
      invoke("devtools_drag_end", {});
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", end);
    window.addEventListener("blur", end);
    document.body.appendChild(dtGrab);
    // the press that started it must not also reach the page below
    dtGrab.addEventListener("mousedown", (e) => e.preventDefault());
  }
  listen("dt-drag", (e) => { if (e.payload && e.payload.active) dtDragStart(); });

  /* ---------------- boot ---------------- */
  invoke("get_initial_state").then((r) => {
    S.settings = r.settings;
    S.bookmarks = r.bookmarks || [];
    S.tabs = r.tabs || [];
    S.active = r.active || 0;
    S.sysDark = !!r.resolvedDark;
    applyTheme();
    renderTabs();
    renderBmBar();
    invoke("get_tabs").then(() => {});
    // watch system theme from shell side too
    if (window.matchMedia) {
      const mq = window.matchMedia("(prefers-color-scheme: dark)");
      mq.addEventListener?.("change", (ev) => {
        S.sysDark = ev.matches;
        invoke("sys_theme_changed", { dark: ev.matches });
        applyTheme();
      });
      if (S.settings.theme === "system") {
        S.sysDark = mq.matches;
        applyTheme();
      }
    }
    // initial omnibox state
    invoke("get_tabs").then((r2) => { S.tabs = r2.tabs || S.tabs; S.active = r2.active || S.active; renderTabs(); });
  });

  window.addEventListener("resize", () => {
    if (curMenu) revealMenu();
    renderBmBar();
    refreshMaxIcon();
  });

  async function refreshMaxIcon() {
    try {
      const m = await win.isMaximized();
      if (m === S.maximized) return;
      S.maximized = m;
      el.winMax.innerHTML = svgUse(m ? "i-restore" : "i-max", "cap");
      el.winMax.title = m ? "还原" : "最大化";
    } catch (e) { /* ignore */ }
  }
  refreshMaxIcon();
})();
