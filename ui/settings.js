(() => {
  const { invoke } = window.Nano;

  let st = null; // current settings object

  async function save() {
    await invoke("set_settings", { settings: st });
  }

  function initSeg(id, key) {
    const seg = document.getElementById(id);
    seg.addEventListener("click", (e) => {
      const btn = e.target.closest("button[data-v]");
      if (!btn) return;
      if (st[key] === btn.dataset.v) return;
      st[key] = btn.dataset.v;
      paint();
      save();
    });
  }

  function initSwitch(id, key) {
    const el = document.getElementById(id);
    el.addEventListener("click", () => {
      st[key] = !st[key];
      paint();
      save();
    });
  }

  function paint() {
    for (const [id, key] of [["seg-engine", "searchEngine"], ["seg-theme", "theme"], ["seg-startup", "startup"]]) {
      const seg = document.getElementById(id);
      for (const b of seg.querySelectorAll("button[data-v]")) {
        b.classList.toggle("on", b.dataset.v === st[key]);
      }
    }
    document.getElementById("sw-bookmarks").classList.toggle("on", st.bookmarksBar);
  }

  function initHomepage() {
    const input = document.getElementById("in-homepage");
    input.addEventListener("change", () => {
      let v = input.value.trim();
      if (v && !/^(https?:\/\/|nano:\/\/)/i.test(v)) {
        if (/^[\w.-]+\.[a-z]{2,}/i.test(v)) v = "https://" + v;
        else v = "";
      }
      if (!v) v = "nano://newtab";
      input.value = v === "nano://newtab" ? "" : v;
      st.homepage = v;
      save();
    });
  }

  function initDownloadsDir() {
    const input = document.getElementById("in-downloads");
    input.addEventListener("change", () => {
      const v = input.value.trim();
      if (v && !/^[a-zA-Z]:[\\/]/.test(v) && !/^\\\\/.test(v)) {
        // only absolute Windows paths (drive letter or UNC) are accepted
        input.value = st.downloadsDir || "";
        return;
      }
      input.value = v;
      st.downloadsDir = v;
      save();
    });
  }

  function initClear() {
    const btn = document.getElementById("clear-history");
    btn.addEventListener("click", async () => {
      if (btn.dataset.confirm) {
        await invoke("clear_history");
        btn.textContent = "清除浏览记录";
        delete btn.dataset.confirm;
      } else {
        btn.textContent = "确认清除？";
        btn.dataset.confirm = "1";
        setTimeout(() => { btn.textContent = "清除浏览记录"; delete btn.dataset.confirm; }, 2500);
      }
    });
  }

  initSeg("seg-engine", "searchEngine");
  initSeg("seg-theme", "theme");
  initSeg("seg-startup", "startup");
  initSwitch("sw-bookmarks", "bookmarksBar");
  initHomepage();
  initDownloadsDir();
  initClear();
  document.getElementById("open-downloads").addEventListener("click", () => invoke("open_downloads_folder"));

  window.Nano.boot().then((state) => {
    st = state.settings;
    const input = document.getElementById("in-homepage");
    input.value = st.homepage && st.homepage !== "nano://newtab" ? st.homepage : "";
    document.getElementById("dl-default").textContent = state.defaultDownloadsDir || "应用目录\\Downloads";
    document.getElementById("in-downloads").value = st.downloadsDir || "";
    paint();
  });
})();
