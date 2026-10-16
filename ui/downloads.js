(() => {
  const { invoke, esc, host } = window.Nano;
  const list = document.getElementById("list");

  const STATUS = {
    downloading: "下载中",
    done: "已完成",
    failed: "失败",
    cancelled: "已取消",
    interrupted: "已中断",
  };

  function fmtTime(ms) {
    const d = new Date(ms);
    const hm = `${d.getHours()}`.padStart(2, "0") + ":" + `${d.getMinutes()}`.padStart(2, "0");
    return `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()} ${hm}`;
  }

  function render(items) {
    list.innerHTML = "";
    if (!items.length) {
      list.innerHTML = `<div class="empty-hint">没有下载内容<br>下载的文件会出现在这里</div>`;
      return;
    }
    for (const d of items) {
      const r = document.createElement("div");
      r.className = "list-row";
      const sub = d.status === "failed" ? d.url : d.path || d.url;
      r.innerHTML = `
        <img src="data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24'%3E%3Cpath fill='%235f6368' d='M6 2h7l5 5v13a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2zm7 1.5V8h4.5L13 3.5zM8 12h8v1.6H8V12zm0 4h8v1.6H8V16z'/%3E%3C/svg%3E">
        <span class="lr-title">${esc(d.name || host(d.url))}</span>
        <span class="lr-url">${esc(sub)}</span>
        <span class="dl-status s-${d.status}"><span class="dot"></span>${STATUS[d.status] || d.status}</span>
        <span class="lr-time">${fmtTime(d.time)}</span>`;
      list.appendChild(r);
    }
  }

  async function load() {
    render(await invoke("get_downloads"));
  }

  document.getElementById("open-dir").addEventListener("click", () => invoke("open_downloads_folder"));
  window.Nano.listen("downloads-changed", load);
  window.Nano.boot().then(load);
})();
