(() => {
  const { invoke, esc, tabId, host } = window.Nano;
  const list = document.getElementById("list");
  const search = document.getElementById("search");

  const X = `<svg viewBox="0 0 24 24"><path stroke="currentColor" stroke-width="1.6" stroke-linecap="round" d="M6.5 6.5l11 11M17.5 6.5l-11 11"/></svg>`;
  const E = `<svg viewBox="0 0 24 24"><path fill="currentColor" d="M14.06 9.02l.92.92L5.92 19H5v-.92l9.06-9.06M17.66 4c-.2 0-.41.08-.56.23l-1.87 1.87 3.67 3.67 1.87-1.87c.31-.31.31-.82 0-1.14l-2.53-2.53c-.15-.15-.36-.23-.58-.23zM14.06 7.44L3 18.5V22h3.5L17.73 10.77l-3.67-3.33z"/></svg>`;

  let all = [];

  // the stored favicon is the page-reported one; a failed or unknown image
  // degrades to a globe glyph so a row never renders an empty slot
  const GLOBE = `<svg viewBox="0 0 24 24"><circle cx="12" cy="12" r="9" fill="none" stroke="currentColor" stroke-width="1.7"/><ellipse cx="12" cy="12" rx="4" ry="9" fill="none" stroke="currentColor" stroke-width="1.7"/><path stroke="currentColor" stroke-width="1.7" d="M3 12h18"/></svg>`;
  const GLOBE_ATTR = GLOBE.replace(/"/g, "&quot;");
  const bmIcon = (b) =>
    /^https?:\/\//i.test(b.icon || "")
      ? `<img src="${esc(b.icon)}" onerror="this.outerHTML='${GLOBE_ATTR}'">`
      : GLOBE;

  function row(b) {
    const r = document.createElement("div");
    r.className = "list-row";
    r.innerHTML = `
      ${bmIcon(b)}
      <span class="lr-title"><a href="${esc(b.url)}">${esc(b.title || host(b.url))}</a></span>
      <span class="lr-url">${esc(b.url)}</span>
      <span class="lr-actions">
        <button class="iconbtn edit" title="编辑">${E}</button>
        <button class="iconbtn del" title="删除">${X}</button>
      </span>`;
    r.querySelector("a").addEventListener("click", (e) => {
      e.preventDefault();
      invoke("internal_nav", { id: tabId(), input: b.url });
    });
    r.querySelector(".del").addEventListener("click", (e) => {
      const btn = e.currentTarget;
      if (btn.dataset.confirm) {
        invoke("remove_bookmark", { url: b.url }).then(load);
        return;
      }
      btn.dataset.confirm = "1";
      btn.title = "确定删除这条书签吗？";
      btn.style.color = "var(--danger)";
      setTimeout(() => { delete btn.dataset.confirm; btn.title = "删除"; btn.style.color = ""; }, 2500);
    });
    r.querySelector(".edit").addEventListener("click", () => {
      editRow(r, b);
    });
    return r;
  }

  function editRow(r, b) {
    const box = document.createElement("div");
    box.className = "bm-edit";
    box.innerHTML = `
      ${bmIcon(b)}
      <div class="fields">
        <input class="f-title" value="${esc(b.title)}" placeholder="标题" spellcheck="false">
        <input class="f-url" value="${esc(b.url)}" placeholder="网址" spellcheck="false">
      </div>
      <div class="btns">
        <button class="ok">保存</button>
        <button class="cancel">取消</button>
      </div>`;
    r.replaceWith(box);
    const tIn = box.querySelector(".f-title");
    const uIn = box.querySelector(".f-url");
    tIn.focus();
    tIn.select();

    async function save() {
      const url = uIn.value.trim();
      const title = tIn.value.trim() || host(url);
      if (!url || !/^https?:\/\//i.test(url)) {
        uIn.focus();
        uIn.style.borderColor = "var(--danger)";
        setTimeout(() => (uIn.style.borderColor = ""), 1200);
        return;
      }
      await invoke("update_bookmark", { oldUrl: b.url, url, title });
      load();
    }
    box.querySelector(".ok").addEventListener("click", save);
    box.querySelector(".cancel").addEventListener("click", load);
    box.addEventListener("keydown", (e) => {
      if (e.key === "Enter") { e.preventDefault(); save(); }
      if (e.key === "Escape") load();
    });
  }

  function render() {
    const q = (search.value || "").toLowerCase();
    const items = q
      ? all.filter((b) => b.title.toLowerCase().includes(q) || b.url.toLowerCase().includes(q))
      : all;
    list.innerHTML = "";
    if (!items.length) {
      list.innerHTML = `<div class="empty-hint">${q ? "没有匹配的书签" : "还没有书签<br>点击地址栏右侧的星号即可收藏网页"}</div>`;
      return;
    }
    for (const b of items) list.appendChild(row(b));
  }

  async function load() {
    all = await invoke("get_bookmarks");
    render();
  }

  search.addEventListener("input", render);
  window.Nano.listen("bookmarks-changed", load);
  window.Nano.boot().then(load);
})();
