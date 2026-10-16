//! In-process Chrome DevTools Protocol bridge over WebView2.
//!
//! WebView2 exposes CDP through `ICoreWebView2::CallDevToolsProtocolMethod`, so
//! no `--remote-debugging-port` socket is ever opened: neither browser can be
//! driven from outside this process.
//!
//! Two properties of that API shape the code below:
//!
//! * The completion callback receives only the method's own result object —
//!   there is no `{"id":…}` envelope to correlate against. Replies are therefore
//!   matched by a sequence number baked into each per-call handler.
//! * The callback runs on the WebView2 (UI) thread. Waiting for it there would
//!   deadlock, so every call is fire-and-forget and the *caller* (a worker
//!   thread, never the UI thread) waits on a condvar for the reply to land.
//!
//! Two call styles sit on top of that: [`Cdp::call`] blocks for the reply, which
//! is what the automation agent wants, while [`Cdp::send`] hands the reply to a
//! callback, which is what the docked DevTools websocket bridge wants (it must
//! never block, and it answers on its own socket thread).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, Webview, Wry};
use webview2_com::{
  CallDevToolsProtocolMethodCompletedHandler, DevToolsProtocolEventReceivedEventHandler,
};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::System::Com::CoTaskMemFree;

/// Label of the hidden window that hosts the automation page.
pub const AGENT_LABEL: &str = "nanoagent";

/// Null-terminated UTF-16, for the PCWSTR arguments of the WebView2 APIs.
fn wide(s: &str) -> Vec<u16> {
  s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Called once with the raw result JSON (or a transport error).
type Reply = Box<dyn FnOnce(Result<String, String>) + Send + 'static>;

#[derive(Default)]
struct Sink {
  replies: HashMap<u64, Result<String, String>>,
}

/// One CDP session bound to a single webview.
pub struct Cdp {
  app: AppHandle<Wry>,
  label: String,
  seq: AtomicU64,
  /// replies for blocking callers, plus the condvar they park on
  sink: Mutex<Sink>,
  cv: Condvar,
  /// replies for callback callers
  pending: Mutex<HashMap<u64, Reply>>,
}

impl Cdp {
  pub fn new(app: AppHandle<Wry>, label: impl Into<String>) -> Self {
    Self {
      app,
      label: label.into(),
      seq: AtomicU64::new(1),
      sink: Mutex::new(Sink::default()),
      cv: Condvar::new(),
      pending: Mutex::new(HashMap::new()),
    }
  }

  /// The webview this session drives: a child webview in the main window, or —
  /// for the automation agent — the webview that fills its own hidden window.
  fn webview(&self) -> Result<Webview<Wry>, String> {
    if let Some(w) = self.app.get_webview(&self.label) {
      return Ok(w);
    }
    self
      .app
      .get_webview_window(&self.label)
      .map(|w| AsRef::<Webview<Wry>>::as_ref(&w).clone())
      .ok_or_else(|| format!("webview '{}' is not ready", self.label))
  }

  /// Route a reply to whoever is waiting for it. Runs on the UI thread.
  fn complete(&self, seq: u64, res: Result<String, String>) {
    if let Some(cb) = self.pending.lock().unwrap().remove(&seq) {
      cb(res);
      return;
    }
    let mut g = self.sink.lock().unwrap();
    if g.replies.insert(seq, res).is_none() {
      self.cv.notify_all();
    }
  }

  /// Issue a CDP method and hand its reply to `cb`, without blocking.
  pub fn send(
    self: &Arc<Self>,
    method: &str,
    params: &str,
    cb: impl FnOnce(Result<String, String>) + Send + 'static,
  ) -> Result<(), String> {
    let seq = self.seq.fetch_add(1, Ordering::SeqCst);
    self.pending.lock().unwrap().insert(seq, Box::new(cb));
    if let Err(e) = self.issue(seq, method, params) {
      self.pending.lock().unwrap().remove(&seq);
      return Err(e);
    }
    Ok(())
  }

  fn issue(self: &Arc<Self>, seq: u64, method: &str, params: &str) -> Result<(), String> {
    let wv = self.webview()?;
    let on_done = self.clone();
    let on_fail = self.clone();
    let mname = wide(method);
    let mparams = wide(params);
    let fail_msg = format!("CDP {} could not be issued", method);

    wv
      .with_webview(move |pw| {
        let wv = match unsafe { pw.controller().CoreWebView2() } {
          Ok(w) => w,
          Err(e) => {
            on_done.complete(seq, Err(format!("CoreWebView2 unavailable: {}", e.message())));
            return;
          }
        };
        let handler =
          CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |code, result| {
            on_done.complete(
              seq,
              match code {
                Ok(()) => Ok(result),
                Err(e) => Err(e.message()),
              },
            );
            Ok(())
          }));
        if unsafe {
          wv.CallDevToolsProtocolMethod(PCWSTR(mname.as_ptr()), PCWSTR(mparams.as_ptr()), &handler)
        }
        .is_err()
        {
          // the handler will not fire, so unblock the waiter right away
          on_fail.complete(seq, Err(fail_msg));
        }
      })
      .map_err(|e| format!("with_webview failed: {}", e))
  }

  /// Issue a CDP method and parse its result. An empty reply (methods that
  /// return no result at all) becomes `null`.
  pub fn call(self: &Arc<Self>, method: &str, params: &str) -> Result<serde_json::Value, String> {
    self.call_within(method, params, Duration::from_secs(30))
  }

  pub fn call_within(
    self: &Arc<Self>,
    method: &str,
    params: &str,
    timeout: Duration,
  ) -> Result<serde_json::Value, String> {
    let raw = self.call_raw(method, params, timeout)?;
    if raw.trim().is_empty() {
      return Ok(serde_json::Value::Null);
    }
    serde_json::from_str(&raw).map_err(|e| format!("bad CDP reply for {}: {}", method, e))
  }

  /// Issue a CDP method and return its raw result JSON, blocking for the reply.
  pub fn call_raw(
    self: &Arc<Self>,
    method: &str,
    params: &str,
    timeout: Duration,
  ) -> Result<String, String> {
    let seq = self.seq.fetch_add(1, Ordering::SeqCst);
    self.issue(seq, method, params)?;
    self.await_reply(seq, method, timeout)
  }

  fn await_reply(&self, seq: u64, method: &str, timeout: Duration) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let mut g = self.sink.lock().unwrap();
    loop {
      if let Some(r) = g.replies.remove(&seq) {
        return r;
      }
      let now = Instant::now();
      if now >= deadline {
        return Err(format!("CDP {} timed out", method));
      }
      let (next, _) = self.cv.wait_timeout(g, deadline - now).unwrap();
      g = next;
    }
  }

  /// Subscribe to a CDP event. `cb` gets the event's parameter JSON.
  ///
  /// The subscription object is owned by WebView2 once registered, so the
  /// callback stays alive for as long as the webview does.
  pub fn on_event<F: Fn(String) + Send + 'static>(
    self: &Arc<Self>,
    event: &str,
    cb: F,
  ) -> Result<(), String> {
    let wv = self.webview()?;
    let name = wide(event);
    wv
      .with_webview(move |pw| {
        let Ok(wv) = (unsafe { pw.controller().CoreWebView2() }) else { return };
        let Ok(recv) = (unsafe { wv.GetDevToolsProtocolEventReceiver(PCWSTR(name.as_ptr())) })
        else {
          return;
        };
        let handler = DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_wv, args| {
          if let Some(a) = args {
            let mut p = PWSTR::null();
            if unsafe { a.ParameterObjectAsJson(&mut p) }.is_ok() && !p.is_null() {
              let json = unsafe { p.to_string() }.unwrap_or_default();
              unsafe { CoTaskMemFree(Some(p.0 as *const core::ffi::c_void)) };
              cb(json);
            }
          }
          Ok(())
        }));
        let mut token = 0i64;
        let _ = unsafe { recv.add_DevToolsProtocolEventReceived(&handler, &mut token) };
      })
      .map_err(|e| format!("with_webview failed: {}", e))
  }
}
