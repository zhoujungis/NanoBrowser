//! The docked DevTools panel.
//!
//! WebView2 ships the real DevTools frontend (`devtools://devtools/bundled/`)
//! but only exposes it through `OpenDevToolsWindow`, which pops out a separate
//! window. To get Chrome's docked panel instead, the frontend is loaded into a
//! second child webview at the bottom of the window, pointed at a loopback
//! websocket served from here.
//!
//! That socket is not a debugging port: it only accepts the session id that
//! this process minted for the panel's own URL, and every request is relayed to
//! a page webview through `CallDevToolsProtocolMethod`. A page that scanned
//! localhost would find a listener that answers nothing without the id, and the
//! id lives for as long as the panel does.

use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use tauri::{
  AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Url, WebviewBuilder, WebviewUrl, Wry,
};
use tauri::webview::NewWindowResponse;

use nanobrowser::ws::{take_frame, Ws};

use crate::state::AppState;

pub const PANEL_LABEL: &str = "devtools";

/// Smallest panel we allow (per axis), its first-open size, and the smallest
/// page we leave on screen.
const MIN_H: f64 = 96.0;
const DEFAULT_H: f64 = 320.0;
const MIN_PAGE_H: f64 = 96.0;
const MIN_W: f64 = 240.0;
const DEFAULT_W: f64 = 480.0;
const MIN_PAGE_W: f64 = 240.0;

/// Which edge the panel is docked to. Only two, like Chrome's bottom/right.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
  Bottom,
  Right,
}

impl Side {
  fn parse(s: &str) -> Side {
    if s == "right" {
      Side::Right
    } else {
      Side::Bottom
    }
  }

  fn as_str(self) -> &'static str {
    match self {
      Side::Bottom => "bottom",
      Side::Right => "right",
    }
  }
}

/// Split the window's content area into the page and, when it is open, the
/// panel: `((page_w, page_h), Some((panel_x, panel_y, panel_w, panel_h)))`.
/// All values are logical pixels relative to the window; the page always starts
/// at `(0, shell_h)`, the panel takes the far edge the dock side asks for.
pub fn split(
  app: &AppHandle<Wry>,
  content_w: f64,
  content_h: f64,
  shell_h: f64,
) -> ((f64, f64), Option<(f64, f64, f64, f64)>) {
  let st = app.state::<Mutex<AppState>>();
  let s = st.lock().unwrap();
  split_geometry(
    s.devtools_open,
    side_of(&s),
    s.devtools_h,
    s.devtools_w,
    content_w,
    content_h,
    shell_h,
  )
}

/// The arithmetic behind [`split`], with the panel's state passed in instead of
/// read from the app, so the docking rules can be checked without a window.
fn split_geometry(
  open: bool,
  side: Side,
  panel_h: f64,
  panel_w: f64,
  content_w: f64,
  content_h: f64,
  shell_h: f64,
) -> ((f64, f64), Option<(f64, f64, f64, f64)>) {
  if !open {
    return ((content_w.max(1.0), content_h.max(1.0)), None);
  }
  match side {
    Side::Bottom => {
      let h = clamp_axis(panel_h, content_h, MIN_H, MIN_PAGE_H);
      let page_h = (content_h - h).max(1.0);
      ((content_w.max(1.0), page_h), Some((0.0, shell_h + page_h, content_w, h)))
    }
    Side::Right => {
      let w = clamp_axis(panel_w, content_w, MIN_W, MIN_PAGE_W);
      let page_w = (content_w - w).max(1.0);
      ((page_w, content_h.max(1.0)), Some((page_w, shell_h, w, content_h)))
    }
  }
}

fn side_of(s: &AppState) -> Side {
  Side::parse(&s.profile.settings.devtools_side)
}

/// Injected into the frontend: a Chrome-like grip along the panel's top edge
/// (bottom dock) or its left edge (right dock), plus the keyboard escape hatches.
///
/// The panel is a `devtools://` document, where neither Tauri's IPC nor
/// WebView2's host objects (`window.chrome.webview`) exist, so the signal is a
/// popup to a host that cannot exist: `on_new_window` sees the request, reads
/// the action off the path and denies it. Nothing is ever navigated to.
const GRIP_JS: &str = r#"(function () {
  if (window.top !== window) return;
  if (window.__nanoDtGrip) return;
  window.__nanoDtGrip = true;
  function post(a) {
    try { window.open('https://nano-dt.invalid/' + a, '_blank'); } catch (e) {}
  }
  var side = 'bottom';
  try {
    var m = /(?:^|[#&])nanoSide=([^&]+)/.exec(location.hash || '');
    side = m ? m[1] : 'bottom';
  } catch (e) {}
  var right = side === 'right';
  function install() {
    if (document.getElementById('__nano_dt_grip')) return;
    // the frontend renders into <body>; the bar hangs off <html> so a re-render
    // of the body cannot take it with it
    var host = document.documentElement || document.body;
    if (!host) return;
    var bar = document.createElement('div');
    bar.id = '__nano_dt_grip';
    // the bar sits on the edge that faces the page, and a small pill marks it
    bar.style.cssText = right
      ? 'position:fixed;left:0;top:0;bottom:0;width:7px;z-index:2147483647;cursor:ew-resize;'
      : 'position:fixed;left:0;right:0;top:0;height:7px;z-index:2147483647;cursor:ns-resize;';
    var pill = document.createElement('div');
    pill.style.cssText = right
      ? 'position:absolute;top:50%;left:2px;width:3px;height:44px;margin-top:-22px;border-radius:2px;background:transparent;transition:background .12s;'
      : 'position:absolute;left:50%;top:2px;width:44px;height:3px;margin-left:-22px;border-radius:2px;background:transparent;transition:background .12s;';
    bar.appendChild(pill);
    bar.addEventListener('mouseenter', function () { pill.style.background = 'rgba(128,131,141,.6)'; });
    bar.addEventListener('mouseleave', function () { pill.style.background = 'transparent'; });
    bar.addEventListener('mousedown', function (e) {
      e.preventDefault(); e.stopPropagation();
      post('grip');
    }, true);
    host.appendChild(bar);
  }
  window.addEventListener('keydown', function (e) {
    if (e.key === 'F12' || (e.ctrlKey && e.shiftKey && (e.key === 'I' || e.key === 'i'))) {
      e.preventDefault();
      post('toggle');
    }
  }, true);
  if (document.body) install(); else document.addEventListener('DOMContentLoaded', install);
  // the frontend finishes its first render a moment later; try again so the grip
  // is there even if it rebuilt the document in the meantime
  setTimeout(install, 1200);
})();"#;

/// Injected into the frontend: swaps the welcome tab's Edge branding for ours.
///
/// The frontend is WebView2's own build and nothing else in it is touched; only
/// the welcome page's heading is rewritten, in place, so the tab keeps its name,
/// its docs links, and its "show this again after an update" checkbox. The
/// heading is found through the element's shadow root, where the page actually
/// lives, and the rewrite is re-asserted on a timer because the template is
/// rendered by Lit and would otherwise put its own copy back.
pub const WELCOME_JS: &str = r##"(function () {
  if (window.top !== window) return;
  if (window.__nanoWelcome) return;
  window.__nanoWelcome = true;
  var MARK = '<svg class="nano-mark" viewBox="0 0 64 64" width="30" height="30" fill="none" aria-hidden="true">'
    + '<defs><linearGradient id="nanoBrandG" x1="0" y1="0" x2="1" y2="1">'
    + '<stop offset="0" stop-color="#5a9bff"/><stop offset="1" stop-color="#1a73e8"/>'
    + '</linearGradient></defs>'
    + '<rect x="3" y="3" width="58" height="58" rx="16" fill="url(#nanoBrandG)"/>'
    + '<path d="M23 43V21l18 22V21" stroke="#fff" stroke-width="5" stroke-linecap="round" stroke-linejoin="round"/>'
    + '<circle cx="45.5" cy="43" r="3.2" fill="#fff"/></svg>';
  // opacity and a neutral translucent fill keep the chips readable on either
  // DevTools theme without having to know which one is on
  var CSS = '.nano-mark{vertical-align:-6px;margin-right:10px}'
    + '.nano-title{vertical-align:middle}'
    + '.nano-sub{display:block;margin-top:5px;font-size:12px;font-weight:400;opacity:.7}'
    + '.nano-chips{display:flex;flex-wrap:wrap;gap:8px;margin:10px 0 0}'
    + '.nano-chips>span{display:flex;align-items:center;gap:6px;padding:4px 9px;border-radius:6px;'
    + 'background:rgba(128,131,141,.16);font-size:11.5px;font-weight:400}'
    + '.nano-chips kbd{font:inherit;font-size:10.5px;padding:0 5px;'
    + 'border:1px solid rgba(128,131,141,.5);border-radius:4px}';
  function brand() {
    var host = document.querySelector('devtools-welcome');
    var root = host && host.shadowRoot;
    if (!root) return;
    var header = root.querySelector('.welcome-header');
    var h1 = header && header.querySelector('h1');
    if (!h1) return;
    var done = h1.querySelector('.nano-mark') && header.querySelector('.nano-chips');
    if (done && root.querySelector('style[data-nano]')) return;
    if (h1.querySelector('.nano-mark')) {
      header.querySelector('.nano-chips').remove();
    }
    h1.innerHTML = MARK + '<span class="nano-title">NanoBrowser 开发者工具</span>'
      + '<span class="nano-sub">内嵌停靠面板 · 不使用调试端口 · 与 Chrome DevTools 同一套前端</span>';
    if (!root.querySelector('style[data-nano]')) {
      var st = document.createElement('style');
      st.setAttribute('data-nano', '1');
      st.textContent = CSS;
      root.appendChild(st);
    }
    var chips = document.createElement('div');
    chips.className = 'nano-chips';
    chips.innerHTML = '<span><kbd>F12</kbd>开关面板</span>'
      + '<span>拖拽面板边缘&nbsp;调整大小</span>'
      + '<span>主菜单&nbsp;切换停靠侧</span>'
      + '<span>面板跟随当前标签页</span>';
    header.appendChild(chips);
  }
  setInterval(brand, 1000);
  brand();
})();"##;

/// The magic host the panel's grip signals through; it is never resolved.
const SIGNAL_HOST: &str = "nano-dt.invalid";

/* ---------------- websocket plumbing (hand rolled: no extra dependency) ---------------- */

/// The handshake, framing and unmasking all live in `nanobrowser::ws`; what is
/// left here is the bridge itself.
fn accept_ws(stream: TcpStream) -> Option<(Ws, String)> {
  Ws::accept(stream)
}

/* ---------------- bridge state ---------------- */

struct Server {
  port: u16,
  stop: Arc<AtomicBool>,
}

struct SessionFor {
  id: String,
  label: String,
}

/// The panel's one live connection. The sender sits behind a mutex so that a
/// `Client` — and therefore the `Arc<Docked>` shared with the socket threads —
/// stays `Sync` (`mpsc::Sender` is `Send` but not `Sync`).
struct Client {
  out: Mutex<Sender<String>>,
}

impl Client {
  fn send(&self, msg: String) {
    let _ = self.out.lock().unwrap().send(msg);
  }
}

pub struct Docked {
  app: AppHandle<Wry>,
  server: Mutex<Option<Server>>,
  session: Mutex<Option<SessionFor>>,
  clients: Mutex<HashMap<String, Arc<Client>>>,
  /// (tab label, protocol domain) pairs whose events are already subscribed
  registered: Mutex<HashSet<(String, String)>>,
  cdps: Mutex<HashMap<String, Arc<nanobrowser::cdp::Cdp>>>,
}

impl Docked {
  pub fn new(app: AppHandle<Wry>) -> Self {
    Self {
      app,
      server: Mutex::new(None),
      session: Mutex::new(None),
      clients: Mutex::new(HashMap::new()),
      registered: Mutex::new(HashSet::new()),
      cdps: Mutex::new(HashMap::new()),
    }
  }

  fn cdp_for(&self, label: &str) -> Arc<nanobrowser::cdp::Cdp> {
    let mut g = self.cdps.lock().unwrap();
    let app = self.app.clone();
    let label = label.to_string();
    g.entry(label.clone())
      .or_insert_with(|| Arc::new(nanobrowser::cdp::Cdp::new(app, label)))
      .clone()
  }

  fn ensure_server(self: &Arc<Self>) -> Option<u16> {
    let mut g = self.server.lock().unwrap();
    if let Some(s) = g.as_ref() {
      return Some(s.port);
    }
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    let _ = listener.set_nonblocking(true);
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let me = self.clone();
    thread::spawn(move || {
      while !flag.load(Ordering::SeqCst) {
        match listener.accept() {
          Ok((stream, _)) => {
            let me = me.clone();
            thread::spawn(move || me.serve(stream));
          }
          Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
            thread::sleep(Duration::from_millis(40));
          }
          Err(_) => break,
        }
      }
    });
    *g = Some(Server { port, stop });
    Some(port)
  }

  fn stop_server(&self) {
    if let Some(s) = self.server.lock().unwrap().take() {
      s.stop.store(true, Ordering::SeqCst);
    }
  }

  /// One panel connection: relay its requests to the page, push page events back.
  fn serve(self: Arc<Self>, stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    // accepted sockets are handed over in blocking mode so the handshake read
    // below cannot come back empty-handed before the request has arrived
    let _ = stream.set_nonblocking(false);
    let Some((mut ws, path)) = accept_ws(stream) else { return };
    let Some(id) = path.strip_prefix("/devtools/page/") else { return };
    let id = id.trim_end_matches('/');
    let label = {
      let g = self.session.lock().unwrap();
      match g.as_ref() {
        Some(s) if s.id == id => s.label.clone(),
        _ => return,
      }
    };

    let (tx, rx) = channel::<String>();
    let client = Arc::new(Client { out: Mutex::new(tx) });
    self.clients.lock().unwrap().insert(label.clone(), client.clone());

    let mut buf: Vec<u8> = Vec::new();
    let mut frag: Vec<u8> = Vec::new();
    let mut frag_op = 0u8;
    let mut closed = false;
    while !closed {
      while let Ok(msg) = rx.try_recv() {
        ws.text(&msg);
      }
      let mut tmp = [0u8; 32768];
      match ws.read(&mut tmp) {
        Ok(0) => break,
        Ok(n) => buf.extend_from_slice(&tmp[..n]),
        Err(ref e) if Ws::is_idle(e) => {}
        Err(_) => break,
      }
      while let Some(f) = take_frame(&mut buf) {
        match f.op {
          0x8 => {
            ws.close();
            closed = true;
            break;
          }
          0x9 => ws.pong(&f.payload),
          0x0 => {
            frag.extend_from_slice(&f.payload);
            if f.fin {
              let msg = String::from_utf8_lossy(&frag).to_string();
              frag.clear();
              self.request(&label, &client, &msg, frag_op);
            }
          }
          0x1 | 0x2 => {
            if f.fin {
              if f.op == 0x1 {
                let msg = String::from_utf8_lossy(&f.payload).to_string();
                self.request(&label, &client, &msg, f.op);
              }
            } else {
              frag = f.payload;
              frag_op = f.op;
            }
          }
          _ => {}
        }
      }
      // a retarget (or a close) swaps out the client for this tab: this socket
      // is then superseded and stops feeding a panel the frontend has already left
      if !closed && self.client_gone(&label, &client) {
        break;
      }
    }

    {
      let mut g = self.clients.lock().unwrap();
      if g.get(&label).map(|c| Arc::ptr_eq(c, &client)).unwrap_or(false) {
        g.remove(&label);
      }
    }
  }

  fn client_gone(&self, label: &str, client: &Arc<Client>) -> bool {
    let g = self.clients.lock().unwrap();
    !g.get(label).map(|c| Arc::ptr_eq(c, client)).unwrap_or(false)
  }

  /// At most one request of this shape can arrive; keep the signature calm.
  fn request(self: &Arc<Self>, label: &str, client: &Arc<Client>, text: &str, op: u8) {
    if op != 0x1 {
      return;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else { return };
    let Some(id) = v.get("id").cloned() else { return };
    let Some(method) = v.get("method").and_then(|m| m.as_str()) else { return };
    let params = v.get("params").map(|p| p.to_string()).unwrap_or_else(|| "{}".to_string());

    self.subscribe(label, method);
    let cdp = self.cdp_for(label);
    let owned = method.to_string();
    let client = client.clone();
    let _ = cdp.send(&owned, &params, move |res| {
      let msg = match res {
        Ok(raw) => {
          let raw = raw.trim();
          let raw = if raw.is_empty() { "{}" } else { raw };
          format!("{{\"id\":{},\"result\":{}}}", id, raw)
        }
        Err(e) => format!(
          "{{\"id\":{},\"error\":{{\"code\":-32000,\"message\":{}}}}}",
          id,
          serde_json::Value::String(e)
        ),
      };
      let _ = client.send(msg);
    });
  }

  /// Register the page webview as a source of `method`'s domain events, once per
  /// (tab, domain). The frontend asks for the domains it uses, so this lands
  /// before the `enable` call that starts producing them.
  fn subscribe(self: &Arc<Self>, label: &str, method: &str) {
    let Some(domain) = method.split('.').next().filter(|_| method.contains('.')) else {
      return;
    };
    if !self
      .registered
      .lock()
      .unwrap()
      .insert((label.to_string(), domain.to_string()))
    {
      return;
    }
    let events = events_for(domain);
    if events.is_empty() {
      return;
    }
    let cdp = self.cdp_for(label);
    for ev in events {
      let name = format!("{}.{}", domain, ev);
      let me = self.clone();
      let lb = label.to_string();
      let nm = name.clone();
      // an event WebView2 does not implement simply has no receiver; skipping it
      // must not cost us the rest of the domain's events
      let _ = cdp.on_event(&name, move |json| me.forward(&lb, &nm, json));
    }
  }

  fn forward(&self, label: &str, name: &str, json: String) {
    let client = self.clients.lock().unwrap().get(label).cloned();
    let Some(c) = client else { return };
    let params = if json.trim().is_empty() { "{}".to_string() } else { json };
    c.send(format!(
      "{{\"method\":{},\"params\":{}}}",
      serde_json::Value::String(name.to_string()),
      params
    ));
  }
}

/* ---------------- public control ---------------- */

fn dock(app: &AppHandle<Wry>) -> Arc<Docked> {
  app.state::<Arc<Docked>>().inner().clone()
}

fn win_size(app: &AppHandle<Wry>) -> Option<(f64, f64)> {
  let win = app.get_window("main")?;
  let size = win.inner_size().ok()?;
  let scale = win.scale_factor().unwrap_or(1.0);
  Some((size.width as f64 / scale, size.height as f64 / scale))
}

fn rand_id() -> String {
  use std::sync::atomic::{AtomicU64, Ordering as O};
  static SEQ: AtomicU64 = AtomicU64::new(0);
  let n = SEQ.fetch_add(1, O::Relaxed);
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

/// Keep the panel at least `min_panel` big and leave at least `min_page` of the
/// content area to the page.
fn clamp_axis(panel: f64, content: f64, min_panel: f64, min_page: f64) -> f64 {
  let max = (content - min_page).max(min_panel);
  panel.clamp(min_panel.min(max), max)
}

fn active_id(app: &AppHandle<Wry>) -> Option<u32> {
  app.state::<Mutex<AppState>>().lock().unwrap().active
}

/// Toggle the docked panel for a tab; returns the resulting open state.
pub fn toggle_for(app: &AppHandle<Wry>, id: u32) -> bool {
  if app.state::<Mutex<AppState>>().lock().unwrap().devtools_open {
    close(app);
    false
  } else {
    open(app, id);
    true
  }
}

/// Show the panel, attached to `id`, and reload the frontend onto it.
pub fn open(app: &AppHandle<Wry>, id: u32) {
  let Some((label, _token)) = tab_identity(app, id) else { return };
  let dock = dock(app);
  let Some(port) = dock.ensure_server() else { return };
  let sid = rand_id();
  *dock.session.lock().unwrap() = Some(SessionFor { id: sid.clone(), label });
  let side = {
    let st = app.state::<Mutex<AppState>>();
    let mut s = st.lock().unwrap();
    s.devtools_open = true;
    if s.devtools_h < MIN_H {
      s.devtools_h = DEFAULT_H;
    }
    if s.devtools_w < MIN_W {
      s.devtools_w = DEFAULT_W;
    }
    side_of(&s)
  };
  ensure_panel(app);
  // the grip has to know which edge it sits on. It travels in the fragment,
  // never in the query: WebView2's devtools:// handler fails the load outright
  // when a second query key follows `?ws=`, while a fragment is resolved by the
  // browser and never reaches the handler.
  let url = format!(
    "devtools://devtools/bundled/inspector.html?ws=127.0.0.1:{}/devtools/page/{}#nanoSide={}",
    port,
    sid,
    side.as_str()
  );
  if let Some(p) = app.get_webview(PANEL_LABEL) {
    if let Ok(u) = Url::parse(&url) {
      let _ = p.navigate(u);
    }
  }
  // lay the window out before the panel becomes visible, so it never shows up
  // at the previous session's geometry for a frame
  crate::tabs::layout(app);
  if let Some(p) = app.get_webview(PANEL_LABEL) {
    let _ = p.show();
  }
}

/// Move the panel to another edge. The frontend is reloaded so the grip ends up
/// on the edge it now faces; the dock side itself is remembered in settings.
pub fn set_side(app: &AppHandle<Wry>, side: &str) {
  let side = Side::parse(side);
  {
    let st = app.state::<Mutex<AppState>>();
    let mut s = st.lock().unwrap();
    let name = side.as_str().to_string();
    if s.profile.settings.devtools_side == name {
      return;
    }
    s.profile.settings.devtools_side = name;
    crate::profile::save_profile(&s.profile);
  }
  if let Some(id) = active_id(app) {
    if app.state::<Mutex<AppState>>().lock().unwrap().devtools_open {
      open(app, id);
    }
  }
}

/// The active tab changed while the panel was open: point the panel at it.
/// The frontend reloads, so its panels start from a clean slate (as Chrome's
/// per-tab DevTools does for a tab that had none open).
pub fn retarget(app: &AppHandle<Wry>, id: u32) {
  if !app.state::<Mutex<AppState>>().lock().unwrap().devtools_open {
    return;
  }
  open(app, id);
}

/// A tab webview is gone: drop the protocol state that was tied to its label,
/// so a long session does not pile up receivers for dead tabs.
pub fn forget(app: &AppHandle<Wry>, id: u32) {
  let label = format!("tab-{}", id);
  let dock = dock(app);
  dock.cdps.lock().unwrap().remove(&label);
  dock.clients.lock().unwrap().remove(&label);
  dock.registered.lock().unwrap().retain(|(l, _)| l != &label);
}

pub fn close(app: &AppHandle<Wry>) {
  let dock = dock(app);
  *dock.session.lock().unwrap() = None;
  // dropping the client map lets any socket still looping notice it has been
  // superseded, instead of holding a thread until the frontend goes away
  dock.clients.lock().unwrap().clear();
  dock.stop_server();
  {
    let st = app.state::<Mutex<AppState>>();
    st.lock().unwrap().devtools_open = false;
  }
  if let Some(p) = app.get_webview(PANEL_LABEL) {
    if let Ok(u) = Url::parse("about:blank") {
      let _ = p.navigate(u);
    }
    let _ = p.hide();
  }
  crate::tabs::layout(app);
}

fn tab_identity(app: &AppHandle<Wry>, id: u32) -> Option<(String, String)> {
  let st = app.state::<Mutex<AppState>>();
  let s = st.lock().unwrap();
  s.tab(id).map(|t| (t.label.clone(), t.token.clone()))
}

/* ---------------- the grip, and the drag it drives ---------------- */

/// A signal from the panel's injected script (see `GRIP_JS` and `SIGNAL_HOST`).
/// It arrives already off the UI thread, so window work may block freely.
fn panel_signal(app: &AppHandle<Wry>, action: &str) {
  match action {
    // the grip was pressed: grow the shell over the whole window so the drag
    // keeps feeding us mouse moves even outside the panel's own bounds
    "grip" => {
      crate::tabs::set_overlay(app, true);
      let _ = app.emit("dt-drag", serde_json::json!({ "active": true }));
    }
    "toggle" => {
      if let Some(id) = active_id(app) {
        toggle_for(app, id);
      }
    }
    _ => {}
  }
}

/// A move of the shell's full-window drag catcher: the pointer's client position
/// along the axis the panel is docked to becomes the panel's new size.
pub fn drag_to(app: &AppHandle<Wry>, x: f64, y: f64) {
  let Some((w, h)) = win_size(app) else { return };
  let shell_h = {
    let st = app.state::<Mutex<AppState>>();
    let s = st.lock().unwrap();
    s.shell_h
  };
  let content_w = w.max(1.0);
  let content_h = (h - shell_h).max(1.0);
  {
    let st = app.state::<Mutex<AppState>>();
    let mut s = st.lock().unwrap();
    match side_of(&s) {
      Side::Bottom => s.devtools_h = clamp_axis(h - y, content_h, MIN_H, MIN_PAGE_H),
      Side::Right => s.devtools_w = clamp_axis(w - x, content_w, MIN_W, MIN_PAGE_W),
    }
  }
  crate::tabs::layout(app);
}

pub fn drag_end(app: &AppHandle<Wry>) {
  crate::tabs::set_overlay(app, false);
}

/* ---------------- panel webview ---------------- */

fn ensure_panel(app: &AppHandle<Wry>) {
  if app.get_webview(PANEL_LABEL).is_some() {
    return;
  }
  let Some(win) = app.get_window("main") else { return };
  let Ok(scale) = win.scale_factor() else { return };
  let (w, h) = win
    .inner_size()
    .map(|s| (s.width as f64 / scale, s.height as f64 / scale))
    .unwrap_or((1120.0, 700.0));
  let Ok(blank) = Url::parse("about:blank") else { return };
  let dark = app.state::<Mutex<AppState>>().lock().unwrap().resolved_dark();
  let builder = WebviewBuilder::new(PANEL_LABEL, WebviewUrl::External(blank))
    .data_directory(crate::profile::webview_data_dir())
    // no native DevTools for the panel either: F12 inside it is ours
    .devtools(false)
    .initialization_script(GRIP_JS)
    // the frontend's own welcome tab is rebranded in place, so the panel opens
    // looking like ours rather than like a stock Edge install
    .initialization_script(WELCOME_JS)
    // the grip signals through a popup to a host that cannot exist: the request
    // is read here and denied, so the panel never actually opens anything
    .on_new_window({
      let app = app.clone();
      move |url, _feat| {
        if url.host_str() == Some(SIGNAL_HOST) {
          let action = url.path().trim_start_matches('/').to_string();
          let app = app.clone();
          tauri::async_runtime::spawn(async move { panel_signal(&app, &action) });
        }
        NewWindowResponse::Deny
      }
    })
    .background_color(if dark {
      tauri::webview::Color(32, 33, 36, 255)
    } else {
      tauri::webview::Color(255, 255, 255, 255)
    });
  // the panel is created at the geometry the next layout pass would give it, so
  // it never flashes as a stub at the window's edge before that pass runs
  let shell_h = app.state::<Mutex<AppState>>().lock().unwrap().shell_h;
  let content_w = w.max(1.0);
  let content_h = (h - shell_h).max(1.0);
  let (_, panel) = split(app, content_w, content_h, shell_h);
  let Some((x, y, pw, ph)) = panel else { return };
  let _ = win.add_child(builder, LogicalPosition::new(x, y), LogicalSize::new(pw, ph));
}

/* ---------------- which protocol events exist ---------------- */

/// The protocol's event names, by domain. `Schema.getDomains` reports the
/// domains WebView2 implements but not their events, so the names are listed
/// here; only the domains the frontend actually asks for are subscribed.
fn events_for(domain: &str) -> &'static [&'static str] {
  match domain {
    "Runtime" => &[
      "consoleAPICalled",
      "exceptionThrown",
      "executionContextCreated",
      "executionContextDestroyed",
      "executionContextsCleared",
      "bindingCalled",
      "inspectRequested",
    ],
    "Debugger" => &["scriptParsed", "scriptFailedToParse", "paused", "resumed", "breakpointResolved"],
    "Profiler" => &["consoleProfileStarted", "consoleProfileFinished"],
    "HeapProfiler" => &[
      "addHeapSnapshotChunk",
      "heapStatsUpdate",
      "lastSeenObjectId",
      "reportHeapSnapshotProgress",
      "resetProfiles",
    ],
    "Console" => &["messageAdded", "messageRepeatCountUpdated", "messagesCleared"],
    "Page" => &[
      "domContentEventFired",
      "loadEventFired",
      "frameAttached",
      "frameDetached",
      "frameNavigated",
      "frameStartedLoading",
      "frameStoppedLoading",
      "frameScheduledNavigation",
      "frameClearedScheduledNavigation",
      "frameResized",
      "lifecycleEvent",
      "navigatedWithinDocument",
      "javascriptDialogOpening",
      "javascriptDialogClosed",
      "screencastFrame",
      "screencastVisibilityChanged",
      "interstitialShown",
      "interstitialHidden",
      "windowOpen",
      "compilationCacheProduced",
      "downloadWillBegin",
      "downloadProgress",
      "fileChooserOpened",
      "backForwardCacheNotUsed",
    ],
    "Network" => &[
      "dataReceived",
      "eventSourceMessageReceived",
      "loadingFailed",
      "loadingFinished",
      "requestIntercepted",
      "requestServedFromCache",
      "requestWillBeSent",
      "requestWillBeSentExtraInfo",
      "responseReceived",
      "responseReceivedExtraInfo",
      "resourceChangedPriority",
      "signedExchangeReceived",
      "webSocketClosed",
      "webSocketCreated",
      "webSocketFrameError",
      "webSocketFrameReceived",
      "webSocketFrameSent",
      "webSocketHandshakeResponseReceived",
      "webSocketWillSendHandshakeRequest",
      "webTransportCreated",
      "webTransportConnectionEstablished",
      "webTransportClosed",
      "policyUpdated",
    ],
    "DOM" => &[
      "attributeModified",
      "attributeRemoved",
      "characterDataModified",
      "childNodeCountUpdated",
      "childNodeInserted",
      "childNodeRemoved",
      "distributedNodesUpdated",
      "documentUpdated",
      "inlineStyleInvalidated",
      "pseudoElementAdded",
      "pseudoElementRemoved",
      "shadowRootPopped",
      "shadowRootPushed",
      "setChildNodes",
      "topLayerElementsUpdated",
    ],
    "CSS" => &[
      "fontsUpdated",
      "mediaQueryResultChanged",
      "styleSheetAdded",
      "styleSheetChanged",
      "styleSheetRemoved",
      "computedStyleUpdated",
    ],
    "Log" => &["entryAdded"],
    "Target" => &[
      "attachedToTarget",
      "detachedFromTarget",
      "receivedMessageFromTarget",
      "targetCreated",
      "targetDestroyed",
      "targetCrashed",
      "targetInfoChanged",
    ],
    "Inspector" => &["detached", "targetCrashed", "targetReloadedAfterCrash"],
    "Overlay" => &[
      "inspectModeCanceled",
      "inspectNodeRequested",
      "nodeHighlightRequested",
      "screenshotRequested",
      "screenshotTaken",
    ],
    "Animation" => &["animationCanceled", "animationCreated", "animationStarted", "animationUpdated"],
    "Audits" => &["issueAdded"],
    "Emulation" => &["virtualTimeBudgetExpired"],
    "ServiceWorker" => &["workerErrorReported", "workerRegistrationUpdated", "workerVersionUpdated"],
    "Storage" => &["cacheStorageContentUpdated", "cacheStorageListUpdated", "indexedDBContentUpdated", "indexedDBListUpdated"],
    "Fetch" => &["requestPaused", "authRequired"],
    "WebAudio" => &[
      "contextCreated",
      "contextWillBeDestroyed",
      "contextChanged",
      "audioListenerCreated",
      "audioListenerWillBeDestroyed",
      "audioNodeCreated",
      "audioNodeWillBeDestroyed",
      "audioParamCreated",
      "audioParamWillBeDestroyed",
      "nodesConnected",
      "nodesDisconnected",
      "nodeParamConnected",
      "nodeParamDisconnected",
    ],
    "Media" => &[
      "playerPropertiesChanged",
      "playerEventsAdded",
      "playerMessagesLogged",
      "playerErrorsRaised",
      "playerCreated",
    ],
    "LayerTree" => &["layerPainted", "layerTreeDidChange"],
    "DOMStorage" => &["domStorageItemAdded", "domStorageItemRemoved", "domStorageItemsCleared"],
    "Security" => &["securityStateChanged", "certificateError"],
    "BackgroundService" => &["recordingStateChanged", "backgroundServiceEventReceived"],
    "Accessibility" => &["loadComplete", "nodesUpdated"],
    "PerformanceTimeline" => &["timelineEventAdded"],
    "Performance" => &["metrics"],
    "Tracing" => &["bufferUsage", "dataCollected"],
    "WebAuthn" => &["credentialAdded", "credentialAsserted"],
    "DeviceOrientation" => &["deviceOrientationUpdated", "deviceOrientationAbsoluteUpdated"],
    "Cast" => &["sinksUpdated"],
    "Preload" => &["ruleSetUpdated", "ruleSetRemoved"],
    "CacheStorage" => &["cacheUpdated"],
    _ => &[],
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn geom(
    open: bool,
    side: Side,
    panel_h: f64,
    panel_w: f64,
    content_w: f64,
    content_h: f64,
  ) -> ((f64, f64), Option<(f64, f64, f64, f64)>) {
    split_geometry(open, side, panel_h, panel_w, content_w, content_h, 112.0)
  }

  #[test]
  fn side_is_parsed_leniently_but_only_the_two_known_edges_are_honoured() {
    assert_eq!(Side::parse("right"), Side::Right);
    assert_eq!(Side::parse("bottom"), Side::Bottom);
    // anything else — including a stale or hand-edited profile value — falls
    // back to the bottom edge rather than leaving the panel unsized
    assert_eq!(Side::parse(""), Side::Bottom);
    assert_eq!(Side::parse("left"), Side::Bottom);
    assert_eq!(Side::parse("RIGHT"), Side::Bottom);
    assert_eq!(Side::as_str(Side::Bottom), "bottom");
    assert_eq!(Side::as_str(Side::Right), "right");
  }

  #[test]
  fn a_closed_panel_leaves_the_page_the_whole_content_area() {
    let ((w, h), panel) = geom(false, Side::Bottom, 320.0, 480.0, 1120.0, 588.0);
    assert_eq!((w, h), (1120.0, 588.0));
    assert!(panel.is_none());
  }

  #[test]
  fn degenerate_content_never_reports_a_zero_sized_page() {
    let ((w, h), panel) = geom(false, Side::Right, 320.0, 480.0, 0.0, -5.0);
    assert_eq!((w, h), (1.0, 1.0));
    assert!(panel.is_none());
  }

  #[test]
  fn a_bottom_panel_sits_under_the_page_and_above_the_window_floor() {
    let ((pw, ph), panel) = geom(true, Side::Bottom, DEFAULT_H, DEFAULT_W, 1120.0, 588.0);
    let (x, y, w, h) = panel.expect("the panel is open");
    assert_eq!((pw, ph), (1120.0, 268.0), "the page keeps 588 - 320");
    assert_eq!((x, y, w, h), (0.0, 380.0, 1120.0, 320.0));
    // the two areas tile the content exactly: no gap, no overlap, and the panel
    // ends where the window does
    assert_eq!(ph + h, 588.0);
    assert_eq!(y, 112.0 + ph, "the panel starts where the page ends");
    assert_eq!(y + h, 112.0 + 588.0);
  }

  #[test]
  fn a_right_panel_sits_beside_the_page_and_reaches_the_right_edge() {
    let ((pw, ph), panel) = geom(true, Side::Right, DEFAULT_H, DEFAULT_W, 1120.0, 588.0);
    let (x, y, w, h) = panel.expect("the panel is open");
    assert_eq!((pw, ph), (640.0, 588.0), "the page keeps 1120 - 480");
    assert_eq!((x, y, w, h), (640.0, 112.0, 480.0, 588.0));
    assert_eq!(pw + w, 1120.0);
    assert_eq!(x, pw, "the panel starts where the page ends");
    assert_eq!(y, 112.0, "a side panel starts below the shell");
    assert_eq!(y + h, 112.0 + 588.0);
  }

  #[test]
  fn panel_size_is_clamped_so_the_page_keeps_a_minimum() {
    // a panel dragged far past the window: the page is pinned at MIN_PAGE_H
    let ((_, ph), panel) = geom(true, Side::Bottom, 1000.0, DEFAULT_W, 1120.0, 300.0);
    let (_, y, _, h) = panel.expect("the panel is open");
    assert_eq!(h, 300.0 - MIN_PAGE_H);
    assert_eq!(ph, MIN_PAGE_H);
    assert_eq!(y, 112.0 + MIN_PAGE_H);

    // and one dragged to nothing keeps its own minimum
    let (_, panel) = geom(true, Side::Bottom, 1.0, DEFAULT_W, 1120.0, 588.0);
    assert_eq!(panel.expect("open").3, MIN_H);

    let ((pw, _), panel) = geom(true, Side::Right, DEFAULT_H, 5000.0, 1120.0, 588.0);
    let (x, _, w, _) = panel.expect("the panel is open");
    assert_eq!(w, 1120.0 - MIN_PAGE_W);
    assert_eq!(pw, MIN_PAGE_W);
    assert_eq!(x, MIN_PAGE_W);

    let (_, panel) = geom(true, Side::Right, DEFAULT_H, 2.0, 1120.0, 588.0);
    assert_eq!(panel.expect("open").2, MIN_W);
  }

  #[test]
  fn a_window_too_short_for_both_areas_still_yields_a_sane_panel() {
    // content smaller than min panel + min page: the panel takes what it can
    let ((_, ph), panel) = geom(true, Side::Bottom, 320.0, DEFAULT_W, 1120.0, 150.0);
    let (_, y, _, h) = panel.expect("the panel is open");
    assert_eq!(h, 96.0);
    assert_eq!(ph, 54.0);
    assert_eq!(y, 112.0 + 54.0);
  }

  #[test]
  fn the_two_areas_never_overlap_for_any_window_size() {
    for content_h in [100.0, 200.0, 480.0, 588.0, 1200.0] {
      for content_w in [300.0, 640.0, 1120.0, 2560.0] {
        for panel in [0.0, 1.0, 96.0, 320.0, 480.0, 4000.0] {
          let ((pw, ph), side) = geom(true, Side::Bottom, panel, panel, content_w, content_h);
          let (x, y, w, h) = side.expect("the panel is open");
          assert_eq!(x, 0.0);
          assert_eq!(w, content_w);
          assert_eq!((pw, ph + h), (content_w, content_h));
          assert!(y >= 112.0, "the panel must stay below the shell");
          assert!(ph >= 1.0 && h >= MIN_H);

          let ((pw, ph), side) = geom(true, Side::Right, panel, panel, content_w, content_h);
          let (x, y, w, h) = side.expect("the panel is open");
          assert_eq!(y, 112.0);
          assert_eq!(h, content_h);
          assert_eq!((pw + w, ph), (content_w, content_h));
          assert!(x >= MIN_PAGE_W || content_w < MIN_W + MIN_PAGE_W);
          assert!(w >= MIN_W);
        }
      }
    }
  }

  #[test]
  fn clamp_axis_keeps_both_sides_of_the_split_alive() {
    assert_eq!(clamp_axis(320.0, 588.0, MIN_H, MIN_PAGE_H), 320.0);
    assert_eq!(clamp_axis(10.0, 588.0, MIN_H, MIN_PAGE_H), MIN_H);
    assert_eq!(clamp_axis(1000.0, 588.0, MIN_H, MIN_PAGE_H), 588.0 - MIN_PAGE_H);
    // when the window cannot hold both minimums the panel's own minimum wins,
    // because a panel that cannot render is worse than a small page
    assert_eq!(clamp_axis(320.0, 150.0, MIN_H, MIN_PAGE_H), MIN_H);
  }

  #[test]
  fn only_the_known_domains_have_events_to_subscribe() {
    for d in ["Runtime", "Debugger", "Network", "Page", "DOM", "CSS", "Log"] {
      assert!(!events_for(d).is_empty(), "{} has no event table", d);
    }
    assert!(events_for("NotADomain").is_empty());
  }
}

