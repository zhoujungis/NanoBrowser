(() => {
  const { invoke, esc, tabId, favicon, host } = window.Nano;
  const list = document.getElementById("list");
  const search = document.getElementById("search");
  const clearBtn = document.getElementById("clear");

  const X = `<svg viewBox="0 0 24 24"><path stroke="currentColor" stroke-width="1.6" stroke-linecap="round" d="M6.5 6.5l11 11M17.5 6.5l-11 11"/></svg>`;

  function fmtTime(ms) {
    const d = new Date(ms);
    return `${d.getHours()}`.padStart(2, "0") + ":" + `${d.getMinutes()}`.padStart(2, "0");
  }
  function dayKey(ms) {
    const d = new Date(ms);
    const today = new Date();
    const yest = new Date(Date.now() - 86400000);
    if (d.toDateString() === today.toDateString()) return "今天";
    if (d.toDateString() === yest.toDateString()) return "昨天";
    return `${d.getMonth() + 1}月${d.getDate()}日`;
  }

  let all = [];
  function render(items) {
    list.innerHTML = "";
    if (!items.length) {
      list.innerHTML = `<div class="empty-hint">没有找到历史记录<br>访问过的网页会出现在这里</div>`;
      return;
    }
    let lastDay = "";
    for (const h of items) {
      const day = dayKey(h.time);
      if (day !== lastDay) {
        lastDay = day;
        const g = document.createElement("div");
        g.className = "group-title";
        g.textContent = day;
        list.appendChild(g);
      }
      const r = document.createElement("div");
      r.className = "list-row";
      r.innerHTML = `
        <img src="${favicon(h.url)}" onerror="this.style.visibility='hidden'">
        <span class="lr-title"><a href="${esc(h.url)}">${esc(h.title || host(h.url))}</a></span>
        <span class="lr-url">${esc(h.url)}</span>
        <span class="lr-time">${fmtTime(h.time)}</span>
        <span class="lr-actions"><button class="iconbtn" title="删除">${X}</button></span>`;
      r.querySelector("a").addEventListener("click", (e) => {
        e.preventDefault();
        invoke("internal_nav", { id: tabId(), input: h.url });
      });
      r.querySelector(".iconbtn").addEventListener("click", (e) => {
        const btn = e.currentTarget;
        if (btn.dataset.confirm) {
          invoke("delete_history", { url: h.url, time: h.time }).then(load);
          return;
        }
        btn.dataset.confirm = "1";
        btn.title = "确定删除这条历史记录吗？";
        btn.style.color = "var(--danger)";
        setTimeout(() => { delete btn.dataset.confirm; btn.title = "删除"; btn.style.color = ""; }, 2500);
      });
      list.appendChild(r);
    }
  }

  async function load() {
    all = await invoke("get_history", { query: search.value || null });
    render(all);
  }

  let t = null;
  search.addEventListener("input", () => {
    clearTimeout(t);
    t = setTimeout(load, 150);
  });

  clearBtn.addEventListener("click", async () => {
    if (clearBtn.dataset.confirm) {
      await invoke("clear_history");
      clearBtn.textContent = "清除浏览记录";
      delete clearBtn.dataset.confirm;
      load();
    } else {
      clearBtn.textContent = "确认清除？";
      clearBtn.dataset.confirm = "1";
      setTimeout(() => { clearBtn.textContent = "清除浏览记录"; delete clearBtn.dataset.confirm; }, 2500);
    }
  });

  window.Nano.boot().then(load);
})();
