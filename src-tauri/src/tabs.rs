use std::{
  fs,
  path::{Path, PathBuf},
  sync::Mutex,
};

use tauri::{
  webview::{DownloadEvent, NewWindowResponse, PageLoadEvent},
  AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, WebviewBuilder, WebviewUrl, Wry,
};

use crate::profile::{hostname_of, now_ms, save_profile};
use crate::state::{AppState, Tab, TabView, ZOOM_STEPS};
use crate::SHELL_LABEL;

pub const APP_ORIGIN: &str = "http://tauri.localhost";

/// Opaque per-tab secret, embedded in that tab's init script. Hex only, so it is
/// always safe to interpolate into a JS string literal.
pub fn new_token() -> String {
  use std::sync::atomic::{AtomicU64, Ordering};
  static SEQ: AtomicU64 = AtomicU64::new(0);
  let n = SEQ.fetch_add(1, Ordering::Relaxed);
  let t = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos() as u64)
    .unwrap_or(0);
  let mut x = t ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
  x ^= x << 13;
  x ^= x >> 7;
  x ^= x << 17;
  format!("{:016x}{:08x}", x, (x >> 32) as u32)
}

/// Resolve which tab really sent a page event: the token is only known to that
/// tab's init script, so a page cannot impersonate a different tab.
pub fn tab_id_by_token(s: &AppState, tok: &str) -> Option<u32> {
  if tok.is_empty() {
    return None;
  }
  s.tabs.iter().find(|t| t.token == tok).map(|t| t.id)
}

pub fn internal_page_of(url: &str) -> Option<&'static str> {
  match url.trim_end_matches('/') {
    "nano://newtab" => Some("ntp.html"),
    "nano://history" => Some("history.html"),
    "nano://bookmarks" => Some("bookmarks.html"),
    "nano://downloads" => Some("downloads.html"),
    "nano://settings" => Some("settings.html"),
    "nano://about" => Some("about.html"),
    _ => None,
  }
}

fn nano_url_for(page: &str) -> &'static str {
  match page {
    "history.html" => "nano://history",
    "bookmarks.html" => "nano://bookmarks",
    "downloads.html" => "nano://downloads",
    "settings.html" => "nano://settings",
    "about.html" => "nano://about",
    _ => "nano://newtab",
  }
}

pub fn nano_title(page: &str) -> &'static str {
  match page {
    "ntp.html" => "新标签页",
    "history.html" => "历史记录",
    "bookmarks.html" => "书签管理",
    "downloads.html" => "下载内容",
    "settings.html" => "设置",
    "about.html" => "关于 NanoBrowser",
    _ => "新标签页",
  }
}

fn is_app_origin(url: &Url) -> bool {
  url.host_str().map(|h| h == "tauri.localhost").unwrap_or(false)
}

fn is_real_page(url: &Url) -> bool {
  url.scheme().starts_with("http") && !is_app_origin(url)
}

/// Parse omnibox input into a navigation target URL.
pub fn parse_input(state: &AppState, input: &str) -> String {
  let s = input.trim();
  if s.is_empty() {
    return "nano://newtab".into();
  }
  if internal_page_of(s).is_some() {
    return s.to_string();
  }
  if let Some(u) = looks_like_url(s) {
    return u;
  }
  state.search_url(s)
}

pub fn looks_like_url(s: &str) -> Option<String> {
  if s.starts_with("http://") || s.starts_with("https://") {
    return if Url::parse(s).is_ok() { Some(s.to_string()) } else { None };
  }
  if s.contains(' ') || s.is_empty() {
    return None;
  }
  let candidate = format!("https://{}", s);
  if let Ok(u) = Url::parse(&candidate) {
    let host = u.host_str()?;
    let valid = host == "localhost"
      || host.parse::<std::net::IpAddr>().is_ok()
      || host.split('.').count() >= 2;
    if valid && s.chars().next().map(|c| c.is_ascii_alphanumeric()).unwrap_or(false) {
      return Some(candidate);
    }
  }
  None
}

/// Chrome-style collision handling: `report.pdf` -> `report (1).pdf` -> `report (2).pdf`.
/// The name is claimed with an exclusive create, so two downloads that run at the
/// same time cannot both decide they own the same path.
fn unique_path(path: &Path) -> PathBuf {
  if reserve(path) {
    return path.to_path_buf();
  }
  let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
  let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
  let ext = path.extension().map(|s| s.to_string_lossy().to_string());
  for n in 1..1000u32 {
    let name = match &ext {
      Some(e) => format!("{} ({}).{}", stem, n, e),
      None => format!("{} ({})", stem, n),
    };
    let cand = dir.join(name);
    if reserve(&cand) {
      return cand;
    }
  }
  path.to_path_buf()
}

/// Claim `path` for this download; WebView2 fills the (empty) file in afterwards.
fn reserve(path: &Path) -> bool {
  fs::OpenOptions::new().write(true).create_new(true).open(path).is_ok()
}

pub fn favicon_for(url: &str) -> String {
  if let Ok(u) = Url::parse(url) {
    if let Some(h) = u.host_str() {
      // the port belongs in the guess: dropping it would point a site served on
      // a non-default port at a different origin's icon
      let port = u.port().map(|p| format!(":{}", p)).unwrap_or_default();
      return format!("{}://{}{}/favicon.ico", u.scheme(), h, port);
    }
  }
  String::new()
}

/* ---------------- emit helpers ---------------- */

pub fn emit_tabs_state(app: &AppHandle<Wry>, state: &AppState) {
  let tabs: Vec<TabView> = state.tabs.iter().map(Into::into).collect();
  let _ = app.emit("tabs-state", serde_json::json!({ "tabs": tabs, "active": state.active }));
}

pub fn emit_omnibox(app: &AppHandle<Wry>, state: &AppState) {
  let payload = match state.active_tab() {
    Some(t) => serde_json::json!({
      "url": t.url, "title": t.title, "loading": t.loading, "zoom": t.zoom(),
      "starred": state.profile.bookmarks.iter().any(|b| b.url == t.url),
      "canBack": t.can_back, "canFwd": t.can_fwd, "id": t.id
    }),
    None => serde_json::json!({
      "url": "", "title": "", "loading": false, "zoom": 1.0,
      "starred": false, "canBack": false, "canFwd": false, "id": 0
    }),
  };
  let _ = app.emit("omnibox-state", payload);
}

fn emit_all(app: &AppHandle<Wry>, state: &AppState) {
  emit_tabs_state(app, state);
  emit_omnibox(app, state);
}

pub fn update_window_title(app: &AppHandle<Wry>, state: &AppState) {
  let title = match state.active_tab() {
    Some(t) if !t.title.is_empty() => format!("{} - NanoBrowser", t.title),
    _ => "NanoBrowser".to_string(),
  };
  if let Some(w) = app.get_window("main") {
    let _ = w.set_title(&title);
  }
}

pub fn persist_session(state: &mut AppState) {
  state.profile.session.tabs = state.tabs.iter().map(|t| t.url.clone()).collect();
  state.profile.session.active = state.active_index().unwrap_or(0);
  save_profile(&state.profile);
}

/* ---------------- geometry ---------------- */

/// Window inner size in logical pixels.
fn win_size(app: &AppHandle<Wry>) -> Option<(f64, f64)> {
  let win = app.get_window("main")?;
  let size = win.inner_size().ok()?;
  let scale = win.scale_factor().unwrap_or(1.0);
  Some((size.width as f64 / scale, size.height as f64 / scale))
}

/// Lift the shell webview above the page webviews (they are sibling HWNDs, and
/// the page webview is normally created after the shell, so it sits on top).
#[cfg(windows)]
fn raise_shell(app: &AppHandle<Wry>) {
  let Some(shell) = app.get_webview(SHELL_LABEL) else { return };
  let _ = shell.with_webview(|pw| unsafe {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
      SetWindowPos, HWND_TOP, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };
    let mut hwnd = HWND::default();
    if pw.controller().ParentWindow(&mut hwnd).is_ok() {
      let _ = SetWindowPos(
        hwnd,
        Some(HWND_TOP),
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
      );
    }
  });
}

#[cfg(not(windows))]
fn raise_shell(_app: &AppHandle<Wry>) {}

/// Re-assert the shell z-order after a page webview has been created or shown.
/// Child webviews are sibling HWNDs, so a webview created after the shell would
/// otherwise be stacked above it and clip an open menu / suggestion list.
pub fn ensure_shell_top(app: &AppHandle<Wry>) {
  let overlay = app.state::<Mutex<AppState>>().lock().unwrap().overlay;
  if overlay {
    raise_shell(app);
  }
}

/// Grow/shrink the shell webview so that menus and omnibox suggestions (which
/// overflow the chrome strip) are not clipped by the webview bounds.
pub fn set_overlay(app: &AppHandle<Wry>, active: bool) {
  {
    let state = app.state::<Mutex<AppState>>();
    let mut s = state.lock().unwrap();
    s.overlay = active;
  }
  let (shell_h, overlay) = {
    let state = app.state::<Mutex<AppState>>();
    let s = state.lock().unwrap();
    (s.shell_h, s.overlay)
  };
  let Some((w, h)) = win_size(app) else { return };
  let Some(shell) = app.get_webview(SHELL_LABEL) else { return };
  let height = if overlay { h } else { shell_h };
  let _ = shell.set_size(LogicalSize::new(w, height));
  if overlay {
    raise_shell(app);
  }
}

pub fn layout(app: &AppHandle<Wry>) {
  let state = app.state::<Mutex<AppState>>();
  let (shell_h, overlay, labels) = {
    let s = state.lock().unwrap();
    (
      s.shell_h,
      s.overlay,
      s.tabs.iter().map(|t| t.label.clone()).collect::<Vec<_>>(),
    )
  };
  let Some((w, h)) = win_size(app) else { return };
  if w <= 0.0 || h <= 0.0 {
    return;
  }
  if let Some(shell) = app.get_webview(SHELL_LABEL) {
    let shell_size_h = if overlay { h } else { shell_h };
    let _ = shell.set_size(LogicalSize::new(w, shell_size_h));
    if overlay {
      raise_shell(app);
    }
  }
  let content_h = h - shell_h;
  if content_h < 1.0 {
    return;
  }
  // the docked DevTools panel takes an edge of the content area; the page keeps
  // the rest (see `devtools::split`)
  let ((page_w, page_h), panel) = crate::devtools::split(app, w, content_h, shell_h);
  for label in labels {
    if let Some(wv) = app.get_webview(&label) {
      let _ = wv.set_position(LogicalPosition::new(0.0, shell_h));
      let _ = wv.set_size(LogicalSize::new(page_w, page_h));
    }
  }
  if let Some((px, py, pw, ph)) = panel {
    if let Some(p) = app.get_webview(crate::devtools::PANEL_LABEL) {
      let _ = p.set_position(LogicalPosition::new(px, py));
      let _ = p.set_size(LogicalSize::new(pw, ph));
      let _ = p.show();
    }
  }
}

/* ---------------- per-tab webview callbacks ---------------- */

fn on_navigation(app: &AppHandle<Wry>, id: u32, url: &Url) -> bool {
  let state = app.state::<Mutex<AppState>>();
  let mut s = state.lock().unwrap();

  // anti-spoof: external pages may not navigate into the app origin
  if is_app_origin(url) {
    let allowed = s.tab(id).map(|t| t.expect_internal).unwrap_or(false);
    if let Some(t) = s.tab_mut(id) {
      t.expect_internal = false;
    }
    if !allowed {
      return false;
    }
  }

  let Some(t) = s.tab_mut(id) else { return true };

  if is_app_origin(url) {
    let page = url.path().trim_start_matches('/');
    t.url = nano_url_for(page).to_string();
    t.internal = true;
    t.icon = String::new();
  } else {
    t.url = url.to_string();
    t.internal = false;
    t.icon = favicon_for(&t.url);
    match t.pending_nav {
      1 | 2 => t.pending_nav = 0, // back/forward: position already adjusted
      _ => {
        t.nav_total = (t.nav_pos + 1).max(1);
        t.nav_pos = t.nav_total;
      }
    }
    t.can_back = t.nav_pos > 1;
    t.can_fwd = t.nav_pos < t.nav_total;
    t.title = hostname_of(&t.url);
  }
  t.loading = true;
  emit_all(app, &s);
  persist_session(&mut s);
  true
}

fn on_title(app: &AppHandle<Wry>, id: u32, title: String) {
  let state = app.state::<Mutex<AppState>>();
  let mut s = state.lock().unwrap();
  let (url, internal) = {
    let Some(t) = s.tab_mut(id) else { return };
    t.title = title.clone();
    (t.url.clone(), t.internal)
  };
  if !internal {
    if let Some(h) = s.profile.history.iter_mut().find(|h| h.url == url) {
      h.title = title;
    }
  }
  emit_all(app, &s);
  update_window_title(app, &s);
  if s.active == Some(id) {
    save_profile(&s.profile);
  }
}

fn on_page_load(app: &AppHandle<Wry>, id: u32, event: PageLoadEvent, url: &Url) {
  let state = app.state::<Mutex<AppState>>();
  let mut s = state.lock().unwrap();
  let mut history_added = false;
  // extract what we need from the tab first, ending its borrow, then touch history
  let visited = if let Some(t) = s.tab_mut(id) {
    match event {
      PageLoadEvent::Started => {
        t.loading = true;
        None
      }
      PageLoadEvent::Finished => {
        t.loading = false;
        if is_real_page(url) {
          // the page usually reports its icon before this fires, and the tab
          // already carries it — seeding it here means a brand new history
          // entry shows in the new-tab tiles right away (the icon event may
          // also arrive after, which heals the entry just as well)
          Some((url.to_string(), t.title.clone(), t.icon.clone()))
        } else {
          None
        }
      }
    }
  } else {
    None
  };
  if let Some((u, title, icon)) = visited {
    if s.profile.history.first().map(|h| h.url.as_str()) != Some(u.as_str()) {
      s.profile.history.insert(0, crate::state::HistoryItem {
        url: u,
        title,
        time: now_ms(),
        icon,
      });
      s.profile.history.truncate(500);
      history_added = true;
    }
  }
  emit_all(app, &s);
  if history_added {
    save_profile(&s.profile);
  }
}

fn on_download(app: &AppHandle<Wry>, event: DownloadEvent) -> bool {
  let state = app.state::<Mutex<AppState>>();
  let mut s = state.lock().unwrap();
  match event {
    DownloadEvent::Requested { url, destination } => {
      // Redirect every download into the configured directory (default:
      // <app dir>\Downloads) so nothing is written to the system drive.
      let dir = crate::profile::resolve_downloads_dir(&s.profile.settings);
      let _ = fs::create_dir_all(&dir);
      let name = destination
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".to_string());
      let target = unique_path(&dir.join(&name));
      *destination = target.clone();
      s.profile.downloads.insert(0, crate::state::DownloadItem {
        url: url.to_string(),
        name,
        path: target.to_string_lossy().to_string(),
        time: now_ms(),
        status: "downloading".into(),
      });
      s.profile.downloads.truncate(100);
      save_profile(&s.profile);
      let _ = app.emit("downloads-changed", ());
    }
    DownloadEvent::Finished { url, path, success } => {
      // two downloads of the same URL can be in flight at once, so pair the event
      // with the oldest entry that is still marked downloading
      let url = url.to_string();
      let idx = {
        let ds = &s.profile.downloads;
        ds.iter()
          .position(|d| d.url == url && d.status == "downloading")
          .or_else(|| ds.iter().rposition(|d| d.url == url))
      };
      if let Some(i) = idx {
        let it = &mut s.profile.downloads[i];
        if let Some(p) = &path {
          it.path = p.to_string_lossy().to_string();
        }
        it.status = if success { "done".into() } else { "failed".into() };
        save_profile(&s.profile);
        let _ = app.emit("downloads-changed", ());
      }
    }
    _ => {}
  }
  true
}

fn build_tab_webview(app: &AppHandle<Wry>, id: u32, target: &str, token: &str) -> WebviewBuilder<Wry> {
  let label = format!("tab-{}", id);
  let dark = app.state::<Mutex<AppState>>().lock().unwrap().resolved_dark();
  let url = if let Some(page) = internal_page_of(target) {
    WebviewUrl::App(page.into())
  } else if let Ok(u) = Url::parse(target) {
    WebviewUrl::External(u)
  } else {
    WebviewUrl::App("ntp.html".into())
  };

  let a_nav = app.clone();
  let a_title = app.clone();
  let a_load = app.clone();
  let a_nw = app.clone();
  let a_dl = app.clone();

  WebviewBuilder::new(label, url)
    .data_directory(crate::profile::webview_data_dir())
    // F12 must dock our own panel, never pop WebView2's separate DevTools
    // window; the page is still reachable over CDP without this
    .devtools(false)
    .initialization_script(crate::inject::init_script(id, token))
    .background_color(if dark {
      tauri::webview::Color(32, 33, 36, 255)
    } else {
      tauri::webview::Color(255, 255, 255, 255)
    })
    .on_navigation(move |url| on_navigation(&a_nav, id, url))
    .on_document_title_changed(move |_wv, title| on_title(&a_title, id, title))
    .on_page_load(move |_wv, payload| {
      on_page_load(&a_load, id, payload.event(), payload.url())
    })
    .on_new_window(move |url, _features| {
      let a = a_nw.clone();
      let u = url.to_string();
      tauri::async_runtime::spawn(async move {
        create_tab(&a, Some(u), false);
      });
      NewWindowResponse::Deny
    })
    .on_download(move |_wv, event| on_download(&a_dl, event))
}

/* ---------------- tab mutations (must run off the main event handler) ---------------- */

pub fn create_tab(app: &AppHandle<Wry>, target: Option<String>, background: bool) -> u32 {
  let state = app.state::<Mutex<AppState>>();
  let target = target.unwrap_or_else(|| "nano://newtab".into());
  let internal = internal_page_of(&target).is_some();
  let token = new_token();
  let (id, shell_h, others) = {
    let mut s = state.lock().unwrap();
    let id = s.next_id;
    s.next_id += 1;
    let t = Tab {
      id,
      label: format!("tab-{}", id),
      url: target.clone(),
      title: if internal {
        nano_title(internal_page_of(&target).unwrap()).to_string()
      } else {
        hostname_of(&target)
      },
      loading: true,
      pinned: false,
      icon: if internal { String::new() } else { favicon_for(&target) },
      internal,
      live: true,
      zoom_idx: 7,
      can_back: false,
      can_fwd: false,
      expect_internal: internal,
      nav_pos: 0,
      nav_total: 0,
      pending_nav: 0,
      token: token.clone(),
    };
    s.tabs.push(t);
    let others: Vec<String> = s.tabs.iter().filter(|t| t.id != id).map(|t| t.label.clone()).collect();
    (id, s.shell_h, others)
  };

  let builder = build_tab_webview(app, id, &target, &token);
  let spawned = spawn_child(app, builder, shell_h);
  if spawned {
    let label = format!("tab-{}", id);
    if background {
      if let Some(wv) = app.get_webview(&label) {
        let _ = wv.hide();
      }
    } else {
      for l in others {
        if let Some(wv) = app.get_webview(&l) {
          let _ = wv.hide();
        }
      }
      state.lock().unwrap().active = Some(id);
      if let Some(wv) = app.get_webview(&label) {
        let _ = wv.show();
        let _ = wv.set_focus();
      }
    }
  }

  let s = state.lock().unwrap();
  emit_all(app, &s);
  update_window_title(app, &s);
  drop(s);
  persist_session(&mut state.lock().unwrap());
  ensure_shell_top(app);
  if !background {
    // a new tab takes the panel with it, exactly like switching to one
    crate::devtools::retarget(app, id);
  }
  id
}

fn spawn_child(app: &AppHandle<Wry>, builder: WebviewBuilder<Wry>, shell_h: f64) -> bool {
  let Some(win) = app.get_window("main") else { return false };
  let Ok(scale) = win.scale_factor() else { return false };
  let (w, h) = match win.inner_size() {
    Ok(size) => (size.width as f64 / scale, size.height as f64 / scale),
    Err(_) => (1200.0, 800.0),
  };
  let content_h = (h - shell_h).max(1.0);
  // a panel that is already open keeps its size: the new tab must not be laid
  // out over it while it is being created
  let ((page_w, page_h), _) = crate::devtools::split(app, w, content_h, shell_h);
  win.add_child(builder, LogicalPosition::new(0.0, shell_h), LogicalSize::new(page_w, page_h)).is_ok()
}

pub fn activate_tab(app: &AppHandle<Wry>, id: u32) {
  let state = app.state::<Mutex<AppState>>();
  let (live, url, token) = {
    let s = state.lock().unwrap();
    match s.tab(id) {
      Some(t) => (t.live, t.url.clone(), t.token.clone()),
      None => return,
    }
  };
  let others = {
    let s = state.lock().unwrap();
    s.tabs.iter().filter(|t| t.id != id).map(|t| t.label.clone()).collect::<Vec<_>>()
  };

  for l in others {
    if let Some(wv) = app.get_webview(&l) {
      let _ = wv.hide();
    }
  }

  if !live {
    // phantom (lazy session restore): materialize webview at tab url
    let builder = build_tab_webview(app, id, &url, &token);
    let shell_h = state.lock().unwrap().shell_h;
    if spawn_child(app, builder, shell_h) {
      state.lock().unwrap().tab_mut(id).map(|t| t.live = true);
    }
  }

  let label = format!("tab-{}", id);
  if let Some(wv) = app.get_webview(&label) {
    let _ = wv.show();
    let _ = wv.set_focus();
  }

  let mut s = state.lock().unwrap();
  s.active = Some(id);
  emit_all(app, &s);
  update_window_title(app, &s);
  drop(s);
  persist_session(&mut state.lock().unwrap());
  ensure_shell_top(app);
  // the docked panel is per-tab: if it is open, it now watches the tab that owns
  // the window (the frontend reloads, like Chrome starting fresh for a new tab)
  crate::devtools::retarget(app, id);
}

pub fn close_tab(app: &AppHandle<Wry>, id: u32, from_user: bool) {
  let state = app.state::<Mutex<AppState>>();
  let (label, url, title) = {
    let s = state.lock().unwrap();
    let Some(pos) = s.tabs.iter().position(|t| t.id == id) else { return };
    let t = &s.tabs[pos];
    (t.label.clone(), t.url.clone(), t.title.clone())
  };

  if let Some(wv) = app.get_webview(&label) {
    let _ = wv.close();
  }
  // the webview is gone: its protocol receivers and cached session go with it
  crate::devtools::forget(app, id);

  let next_id = {
    let mut s = state.lock().unwrap();
    // re-find the tab: the lock was released while the webview was closing, so
    // a stale index could remove a neighbouring tab
    let Some(pos) = s.tabs.iter().position(|t| t.id == id) else { return };
    let was_active = s.active == Some(id);
    let pinned_now = s.tabs[pos].pinned;
    s.tabs.remove(pos);
    if from_user && !pinned_now && url != "nano://newtab" {
      s.closed.push((url, title));
      s.closed.truncate(16);
    }
    if was_active && !s.tabs.is_empty() {
      Some(s.tabs[pos.min(s.tabs.len() - 1)].id)
    } else {
      None
    }
  };

  let s = state.lock().unwrap();
  if s.tabs.is_empty() {
    save_profile(&s.profile);
    drop(s);
    if let Some(w) = app.get_window("main") {
      let _ = w.close();
    }
    return;
  }
  drop(s);

  if let Some(nid) = next_id {
    activate_tab(app, nid);
  }
  let s = state.lock().unwrap();
  emit_all(app, &s);
  drop(s);
  persist_session(&mut state.lock().unwrap());
  ensure_shell_top(app);
}

pub fn navigate_tab(app: &AppHandle<Wry>, id: u32, input: &str) {
  let state = app.state::<Mutex<AppState>>();
  let target = {
    let s = state.lock().unwrap();
    parse_input(&s, input)
  };
  let internal = internal_page_of(&target).is_some();

  {
    let mut s = state.lock().unwrap();
    let Some(t) = s.tab_mut(id) else { return };
    if t.live {
      t.expect_internal = internal;
      t.loading = true;
      if let Some(page) = internal_page_of(&target) {
        t.url = nano_url_for(page).to_string();
        t.internal = true;
        t.title = nano_title(page).to_string();
        t.icon = String::new();
      } else {
        t.url = target.clone();
        t.internal = false;
        t.title = hostname_of(&target);
        t.icon = favicon_for(&target);
        t.nav_total = (t.nav_pos + 1).max(1);
        t.nav_pos = t.nav_total;
        t.can_back = t.nav_pos > 1;
        t.can_fwd = t.nav_pos < t.nav_total;
      }
      emit_all(app, &s);
    } else {
      // phantom: store target, then lazy-spawn via activate_tab
      t.url = target.clone();
      t.expect_internal = internal;
      t.internal = internal;
      t.title = if let Some(page) = internal_page_of(&target) {
        nano_title(page).to_string()
      } else {
        hostname_of(&target)
      };
      t.icon = if internal { String::new() } else { favicon_for(&target) };
    }
  }

  let live = state.lock().unwrap().tab(id).map(|t| t.live).unwrap_or(false);
  if !live {
    activate_tab(app, id);
  } else if let Some(wv) = app.get_webview(&format!("tab-{}", id)) {
    if internal {
      let page = internal_page_of(&target).unwrap();
      if let Ok(u) = Url::parse(&format!("{}/{}", APP_ORIGIN, page)) {
        let _ = wv.navigate(u);
      }
    } else if let Ok(u) = Url::parse(&target) {
      let _ = wv.navigate(u);
    }
  }

  persist_session(&mut state.lock().unwrap());
}

pub fn go_history(app: &AppHandle<Wry>, id: u32, back: bool) {
  let state = app.state::<Mutex<AppState>>();
  let label = {
    let mut s = state.lock().unwrap();
    let Some(t) = s.tab_mut(id) else { return };
    if (back && !t.can_back) || (!back && !t.can_fwd) {
      return;
    }
    t.pending_nav = if back { 1 } else { 2 };
    t.loading = true;
    if back {
      t.nav_pos = t.nav_pos.saturating_sub(1);
      t.can_back = t.nav_pos > 1;
      t.can_fwd = true;
    } else {
      t.nav_pos = (t.nav_pos + 1).min(t.nav_total);
      t.can_fwd = t.nav_pos < t.nav_total;
      t.can_back = t.nav_pos > 1;
    }
    let label = t.label.clone();
    emit_all(app, &s);
    label
  };
  if let Some(wv) = app.get_webview(&label) {
    let _ = wv.eval(if back { "history.back();" } else { "history.forward();" });
  }
}

pub fn reload_tab(app: &AppHandle<Wry>, id: u32) {
  let state = app.state::<Mutex<AppState>>();
  let label = {
    let mut s = state.lock().unwrap();
    let Some(t) = s.tab_mut(id) else { return };
    t.loading = true;
    let label = t.label.clone();
    emit_all(app, &s);
    label
  };
  if let Some(wv) = app.get_webview(&label) {
    let _ = wv.reload();
  }
}

/// Chrome's hard reload: reload with `ignoreCache`, so the engine serves the
/// document *and* the subresources it pulls in straight from the network instead
/// of its HTTP cache. With `clear_cache` the profile's whole disk cache is
/// dropped first — that is Chrome's "empty cache and hard reload".
///
/// WebView2's own `Reload()` only revalidates. The one-shot CDP call below goes
/// through `ICoreWebView2::CallDevToolsProtocolMethod`, i.e. in-process; no
/// debugging port is opened.
pub fn hard_reload_tab(app: &AppHandle<Wry>, id: u32, clear_cache: bool) {
  let state = app.state::<Mutex<AppState>>();
  let label = {
    let mut s = state.lock().unwrap();
    let Some(t) = s.tab_mut(id) else { return };
    t.loading = true;
    let label = t.label.clone();
    emit_all(app, &s);
    label
  };
  let a = app.clone();
  // `Cdp::call` parks its caller until the reply lands on the UI thread, so it
  // must not run on one of the async runtime's workers.
  std::thread::spawn(move || {
    let timeout = std::time::Duration::from_millis(2500);
    let cdp = std::sync::Arc::new(nanobrowser::cdp::Cdp::new(a.clone(), label.clone()));
    if clear_cache {
      // tolerated if this WebView2 build refuses the method: the reload below
      // still bypasses the cache, which is the part the user sees
      let _ = cdp.call_within("Network.clearBrowserCache", "{}", timeout);
    }
    if cdp
      .call_within("Page.reload", r#"{"ignoreCache":true}"#, timeout)
      .is_err()
    {
      // a plain reload beats a button that silently does nothing
      if let Some(wv) = a.get_webview(&label) {
        let _ = wv.reload();
      }
    }
  });
}

pub fn stop_tab(app: &AppHandle<Wry>, id: u32) {
  let state = app.state::<Mutex<AppState>>();
  let label = {
    let mut s = state.lock().unwrap();
    let Some(t) = s.tab_mut(id) else { return };
    t.loading = false;
    let label = t.label.clone();
    emit_all(app, &s);
    label
  };
  if let Some(wv) = app.get_webview(&label) {
    let _ = wv.eval("window.stop();");
  }
}

/// Toggle the docked DevTools panel. Returns the resulting open state, so the
/// caller can keep a menu checkbox in sync.
pub fn toggle_devtools(app: &AppHandle<Wry>, id: u32) -> bool {
  crate::devtools::toggle_for(app, id)
}

/// Returns the resulting zoom factor so the shell can update the menu without
/// waiting for the next state round trip.
pub fn set_zoom(app: &AppHandle<Wry>, id: u32, delta: i32) -> Option<f64> {
  let state = app.state::<Mutex<AppState>>();
  let (label, zoom) = {
    let mut s = state.lock().unwrap();
    let t = s.tab_mut(id)?;
    t.zoom_idx = match delta {
      0 => 7,
      d if d > 0 => (t.zoom_idx + 1).min(ZOOM_STEPS.len() - 1),
      _ => t.zoom_idx.saturating_sub(1),
    };
    let out = (t.label.clone(), t.zoom());
    emit_all(app, &s);
    out
  };
  if let Some(wv) = app.get_webview(&label) {
    let _ = wv.set_zoom(zoom);
  }
  Some(zoom)
}

pub fn find_in_page(app: &AppHandle<Wry>, id: u32, q: &str, move_delta: Option<i32>) {
  let state = app.state::<Mutex<AppState>>();
  let label = {
    let s = state.lock().unwrap();
    let Some(t) = s.tab(id) else { return };
    t.label.clone()
  };
  if let Some(wv) = app.get_webview(&label) {
    let js = match move_delta {
      None => format!(
        "window.__nanoFind && window.__nanoFind({{query:{},index:0}});",
        serde_json::to_string(q).unwrap_or_else(|_| "\"\"".into())
      ),
      Some(d) => format!("window.__nanoFindMove && window.__nanoFindMove({});", d),
    };
    let _ = wv.eval(&js);
  }
}

pub fn find_clear(app: &AppHandle<Wry>, id: u32) {
  let state = app.state::<Mutex<AppState>>();
  let label = {
    let s = state.lock().unwrap();
    let Some(t) = s.tab(id) else { return };
    t.label.clone()
  };
  if let Some(wv) = app.get_webview(&label) {
    let _ = wv.eval("window.__nanoFindClear && window.__nanoFindClear();");
  }
}

/* ---------------- shortcuts (from page keydown relays) ---------------- */

pub fn handle_shortcut(app: &AppHandle<Wry>, key: &str) {
  let state = app.state::<Mutex<AppState>>();
  let active = state.lock().unwrap().active;
  match key {
    "ctrl-t" => {
      let a = app.clone();
      tauri::async_runtime::spawn(async move {
        create_tab(&a, None, false);
      });
    }
    "ctrl-w" => {
      if let Some(id) = active {
        let a = app.clone();
        tauri::async_runtime::spawn(async move {
          close_tab(&a, id, true);
        });
      }
    }
    "ctrl-shift-t" => {
      // pop under a single lock, otherwise two rapid presses can reopen the same url
      let last = state.lock().unwrap().closed.pop();
      if let Some((url, _)) = last {
        let a = app.clone();
        tauri::async_runtime::spawn(async move {
          create_tab(&a, Some(url), false);
        });
      }
    }
    "ctrl-l" => {
      if let Some(wv) = app.get_webview(SHELL_LABEL) {
        let _ = wv.set_focus();
      }
      let _ = app.emit("focus-omnibox", ());
    }
    "ctrl-f" => {
      if let Some(wv) = app.get_webview(SHELL_LABEL) {
        let _ = wv.set_focus();
      }
      let _ = app.emit("open-findbar", ());
    }
    "ctrl-d" => {
      if let Some(id) = active {
        let mut s = state.lock().unwrap();
        if let Some(t) = s.tab(id) {
          let (url, title) = (t.url.clone(), t.title.clone());
          let icon = if t.icon.is_empty() { favicon_for(&url) } else { t.icon.clone() };
          if !url.is_empty() && !url.starts_with("nano://") && !s.profile.bookmarks.iter().any(|b| b.url == url) {
            s.profile.bookmarks.insert(0, crate::state::Bookmark { url, title, added: now_ms(), icon });
            save_profile(&s.profile);
            let _ = app.emit("bookmarks-changed", ());
            emit_omnibox(app, &s);
          }
        }
      }
    }
    "ctrl-h" | "ctrl-shift-o" | "ctrl-j" => {
      let page = match key {
        "ctrl-h" => "nano://history",
        "ctrl-shift-o" => "nano://bookmarks",
        _ => "nano://downloads",
      };
      if let Some(id) = active {
        navigate_tab(app, id, page);
      } else {
        let a = app.clone();
        let p = page.to_string();
        tauri::async_runtime::spawn(async move {
          create_tab(&a, Some(p), false);
        });
      }
    }
    "ctrl-tab" => {
      let s = state.lock().unwrap();
      let len = s.tabs.len();
      if len > 0 {
        let cur = s.active_index().unwrap_or(0);
        let id = s.tabs[(cur + 1) % len].id;
        drop(s);
        activate_tab(app, id);
      }
    }
    "ctrl-shift-tab" => {
      let s = state.lock().unwrap();
      let len = s.tabs.len();
      if len > 0 {
        let cur = s.active_index().unwrap_or(0);
        let id = s.tabs[(cur + len - 1) % len].id;
        drop(s);
        activate_tab(app, id);
      }
    }
    "ctrl-1" | "ctrl-2" | "ctrl-3" | "ctrl-4" | "ctrl-5" | "ctrl-6" | "ctrl-7" | "ctrl-8" => {
      let n: usize = key.trim_start_matches("ctrl-").parse().unwrap_or(1) - 1;
      let s = state.lock().unwrap();
      if n < s.tabs.len() {
        let id = s.tabs[n].id;
        drop(s);
        activate_tab(app, id);
      }
    }
    "ctrl-9" => {
      let s = state.lock().unwrap();
      if let Some(t) = s.tabs.last() {
        let id = t.id;
        drop(s);
        activate_tab(app, id);
      }
    }
    "hard-reload" => {
      if let Some(id) = active {
        hard_reload_tab(app, id, false);
      }
    }
    "alt-left" => {
      if let Some(id) = active {
        go_history(app, id, true);
      }
    }
    "alt-right" => {
      if let Some(id) = active {
        go_history(app, id, false);
      }
    }
    "ctrl-zoom-in" => {
      if let Some(id) = active {
        set_zoom(app, id, 1);
      }
    }
    "ctrl-zoom-out" => {
      if let Some(id) = active {
        set_zoom(app, id, -1);
      }
    }
    "ctrl-zoom-reset" => {
      if let Some(id) = active {
        set_zoom(app, id, 0);
      }
    }
    "f11" => {
      if let Some(w) = app.get_window("main") {
        let fs = w.is_fullscreen().unwrap_or(false);
        let _ = w.set_fullscreen(!fs);
      }
    }
    "f12" | "ctrl-shift-i" => {
      if let Some(id) = active {
        crate::devtools::toggle_for(app, id);
      }
    }
    _ => {}
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::state::sample_tab;

  fn state_with(bookmarks_bar: bool, engine: &str) -> AppState {
    let mut p = crate::state::Profile::default();
    p.settings.bookmarks_bar = bookmarks_bar;
    p.settings.search_engine = engine.into();
    AppState::new(p, false)
  }

  #[test]
  fn the_nano_pages_map_both_ways() {
    for (url, page) in [
      ("nano://newtab", "ntp.html"),
      ("nano://history", "history.html"),
      ("nano://bookmarks", "bookmarks.html"),
      ("nano://downloads", "downloads.html"),
      ("nano://settings", "settings.html"),
    ] {
      assert_eq!(internal_page_of(url), Some(page), "{}", url);
      assert_eq!(nano_url_for(page), url, "{}", page);
      // the round trip is what the session restore and the omnibox both rely on
      assert_eq!(internal_page_of(nano_url_for(page)), Some(page));
    }
  }

  #[test]
  fn the_nano_pages_tolerate_a_trailing_slash_but_not_a_stale_name() {
    assert_eq!(internal_page_of("nano://newtab/"), Some("ntp.html"));
    assert_eq!(internal_page_of("nano://gone"), None);
    assert_eq!(internal_page_of("https://nano://newtab"), None);
    assert_eq!(internal_page_of(""), None);
    assert_eq!(nano_url_for("unknown.html"), "nano://newtab");
  }

  #[test]
  fn internal_pages_have_titles_and_the_rest_do_not() {
    assert_eq!(nano_title("ntp.html"), "新标签页");
    assert_eq!(nano_title("history.html"), "历史记录");
    assert_eq!(nano_title("bookmarks.html"), "书签管理");
    assert_eq!(nano_title("downloads.html"), "下载内容");
    assert_eq!(nano_title("settings.html"), "设置");
    assert_eq!(nano_title("about.html"), "关于 NanoBrowser");
    // anything unrecognised still gets a title, so no tab can end up blank
    assert_eq!(nano_title("index.html"), "新标签页");
  }

  #[test]
  fn only_web_addresses_are_treated_as_addresses() {
    assert_eq!(looks_like_url("https://example.com/x").as_deref(), Some("https://example.com/x"));
    assert_eq!(looks_like_url("example.com").as_deref(), Some("https://example.com"));
    assert_eq!(looks_like_url("www.example.com/ab").as_deref(), Some("https://www.example.com/ab"));
    assert_eq!(looks_like_url("localhost:8080/status").as_deref(), Some("https://localhost:8080/status"));
    assert_eq!(looks_like_url("127.0.0.1:3000").as_deref(), Some("https://127.0.0.1:3000"));
    assert_eq!(looks_like_url("example.co.uk").as_deref(), Some("https://example.co.uk"));

    // a phrase, a single word, a broken scheme and a leading dash are all searches
    assert_eq!(looks_like_url("hello world"), None);
    assert_eq!(looks_like_url("hello"), None);
    assert_eq!(looks_like_url("http://"), None);
    assert_eq!(looks_like_url("-example.com"), None);
    assert_eq!(looks_like_url(""), None);
  }

  #[test]
  fn omnibox_input_becomes_a_url_a_search_or_a_known_page() {
    let s = state_with(true, "bing");
    assert_eq!(parse_input(&s, ""), "nano://newtab");
    assert_eq!(parse_input(&s, "   "), "nano://newtab");
    assert_eq!(parse_input(&s, "nano://settings"), "nano://settings");
    assert_eq!(parse_input(&s, "  nano://history  "), "nano://history");
    assert_eq!(parse_input(&s, "example.com"), "https://example.com");
    // everything else is searched for with the configured engine
    assert_eq!(parse_input(&s, "rust windows"), "https://www.bing.com/search?q=rust+windows");
    assert_eq!(parse_input(&state_with(true, "google"), "rust windows"), "https://www.google.com/search?q=rust+windows");
  }

  #[test]
  fn page_events_are_only_trusted_from_the_tab_that_holds_the_token() {
    let mut s = state_with(true, "bing");
    s.tabs.push(sample_tab(4, "https://a"));
    s.tabs.push(sample_tab(9, "https://b"));
    assert_eq!(tab_id_by_token(&s, "tok9"), Some(9));
    assert_eq!(tab_id_by_token(&s, "tok4"), Some(4));
    // a page cannot claim to be another tab, and an empty token is nobody
    assert_eq!(tab_id_by_token(&s, "tok"), None);
    assert_eq!(tab_id_by_token(&s, "tok999"), None);
    assert_eq!(tab_id_by_token(&s, ""), None);
    assert_eq!(tab_id_by_token(&s, "TOK9"), None, "tokens are case sensitive");
  }

  #[test]
  fn fresh_tokens_are_unique_long_and_script_safe() {
    let a = new_token();
    let b = new_token();
    assert_ne!(a, b);
    assert_eq!(a.len(), 24, "16 hex digits of state plus 8 of mixing");
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "safe inside a JS string literal: {}", a);
    let many: std::collections::HashSet<String> = (0..2000).map(|_| new_token()).collect();
    assert_eq!(many.len(), 2000, "no collisions over a burst of tabs");
  }

  #[test]
  fn the_guessed_favicon_keeps_the_scheme_and_the_host() {
    assert_eq!(favicon_for("https://www.example.com/a/b?c=1"), "https://www.example.com/favicon.ico");
    assert_eq!(favicon_for("http://localhost:8080/x"), "http://localhost:8080/favicon.ico");
    assert_eq!(favicon_for("https://example.com"), "https://example.com/favicon.ico");
    // no host to guess from: the UI falls back to its globe glyph
    assert_eq!(favicon_for("about:blank"), "");
    assert_eq!(favicon_for("not a url"), "");
    // a nano:// page does carry a host, so the guess comes out unusable — which is
    // why callers only ever ask for one on a real page (`if internal { .. }`)
    assert_eq!(favicon_for("nano://newtab"), "nano://newtab/favicon.ico");
  }

  #[test]
  fn a_page_is_only_a_page_when_it_is_http_and_not_our_own_ui() {
    let real = tauri::Url::parse("https://example.com/").unwrap();
    assert!(is_real_page(&real));
    assert!(!is_app_origin(&real));
    // the shell UI is served from tauri.localhost and must never be reloaded,
    // bookmarked or reported as a visited page
    let ui = tauri::Url::parse("http://tauri.localhost/index.html").unwrap();
    assert!(is_app_origin(&ui));
    assert!(!is_real_page(&ui));
    let internal = tauri::Url::parse("nano://newtab").unwrap();
    assert!(!is_app_origin(&internal));
    assert!(!is_real_page(&internal));
  }

  /// A scratch directory beside the test binary, so test files never land off
  /// the drive the project lives on and never touch the app's own data folder.
  fn scratch(name: &str) -> PathBuf {
    let dir = std::env::current_exe()
      .unwrap_or_default()
      .parent()
      .unwrap_or(Path::new("."))
      .join("_testscratch")
      .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch dir");
    dir
  }

  #[test]
  fn a_second_download_of_the_same_name_gets_a_numbered_copy() {
    let dir = scratch("unique_path");
    let first = unique_path(&dir.join("report.pdf"));
    assert_eq!(first.file_name().unwrap(), "report.pdf");
    assert!(first.exists(), "the name is claimed by creating the file");
    let second = unique_path(&dir.join("report.pdf"));
    assert_eq!(second.file_name().unwrap(), "report (1).pdf");
    let third = unique_path(&dir.join("report.pdf"));
    assert_eq!(third.file_name().unwrap(), "report (2).pdf");
    // no extension, and an extension-looking name that is not one
    let bare = unique_path(&dir.join("notes"));
    assert_eq!(bare.file_name().unwrap(), "notes");
    let bare2 = unique_path(&dir.join("notes"));
    assert_eq!(bare2.file_name().unwrap(), "notes (1)");
    // a claimed path is reserved even before anything is written into it
    assert_eq!(std::fs::metadata(&first).unwrap().len(), 0);
    let _ = fs::remove_dir_all(&dir);
  }

  #[test]
  fn a_download_into_a_missing_directory_falls_back_to_the_name_it_was_given() {
    // the reserved directory is gone: the caller still gets a usable path rather
    // than an error or a panic
    let dir = scratch("unique_missing").join("nope");
    let p = unique_path(&dir.join("a.txt"));
    assert_eq!(p.file_name().unwrap(), "a.txt");
    let _ = fs::remove_dir_all(dir.parent().unwrap());
  }
}

