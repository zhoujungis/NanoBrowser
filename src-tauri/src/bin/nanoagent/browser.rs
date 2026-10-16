//! High level browser operations on top of the CDP bridge.
//!
//! Everything a caller needs to drive a page — navigate, read, click, fill,
//! wait, screenshot, cookies, traffic — is expressed as CDP calls so the same
//! primitives serve both the one-shot CLI and the MCP tools.
//!
//! Input is delivered through the CDP `Input` domain rather than synthetic
//! `el.click()` calls: those events are trusted by the page, which is what lets
//! the agent drive real login forms.

use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use nanobrowser::cdp::Cdp;

use crate::traffic::Traffic;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

pub struct Browser {
  cdp: Arc<Cdp>,
  traffic: Arc<Traffic>,
  /// live screencast frames; set only while a screenshot is being taken
  frames: Arc<Mutex<Option<mpsc::Sender<String>>>>,
}

impl Browser {
  pub fn new(cdp: Arc<Cdp>, traffic: Arc<Traffic>) -> Self {
    Self { cdp, traffic, frames: Arc::new(Mutex::new(None)) }
  }

  /// Enable the CDP domains we rely on and start recording traffic.
  pub fn init(&self) -> Result<(), String> {
    // subscribe before enabling, otherwise the first events are missed
    self.traffic.attach(&self.cdp)?;
    let frames = self.frames.clone();
    self.cdp.on_event("Page.screencastFrame", move |p| {
      if let Some(tx) = frames.lock().unwrap().as_ref() {
        let _ = tx.send(p);
      }
    })?;
    self.cdp.call("Network.enable", "{}")?;
    self.cdp.call("Page.enable", "{}")?;
    self.cdp.call("Runtime.enable", "{}")?;
    Ok(())
  }

  /// True once the webview answers CDP — used to wait out webview startup.
  pub fn is_ready(&self) -> bool {
    self.eval("1").is_ok()
  }

  /* ---------------- evaluation ---------------- */

  /// Evaluate an expression in the page and return its value.
  pub fn eval(&self, expr: &str) -> Result<Value, String> {
    let r = self.cdp.call(
      "Runtime.evaluate",
      &json!({ "expression": expr, "returnByValue": true, "awaitPromise": true }).to_string(),
    )?;
    if let Some(ex) = r.get("exceptionDetails") {
      let msg = ex["exception"]["description"]
        .as_str()
        .or_else(|| ex["text"].as_str())
        .unwrap_or("script error");
      return Err(msg.to_string());
    }
    Ok(r.get("result").and_then(|v| v.get("value")).cloned().unwrap_or(Value::Null))
  }

  /* ---------------- navigation ---------------- */

  /// Navigate and wait for the document to finish loading.
  pub fn navigate(&self, url: &str, timeout: Duration) -> Result<Value, String> {
    self.cdp.call("Page.navigate", &json!({ "url": url }).to_string())?;
    let state = self.wait_ready(timeout);
    Ok(json!({
      "url": self.eval("location.href").unwrap_or(Value::Null),
      "title": self.eval("document.title").unwrap_or(Value::Null),
      "readyState": state,
    }))
  }

  /// Wait until `document.readyState` is `complete`; returns the last state seen.
  pub fn wait_ready(&self, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
      let state = self
        .eval("document.readyState")
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_default();
      if state == "complete" || Instant::now() >= deadline {
        return state;
      }
      std::thread::sleep(Duration::from_millis(50));
    }
  }

  /// Read the page as plain text or HTML, optionally scoped to one element.
  pub fn read(&self, format: &str, selector: Option<&str>) -> Result<String, String> {
    let prop = if format == "html" { "outerHTML" } else { "innerText" };
    let expr = match selector {
      Some(sel) => format!(
        "(() => {{ const el = document.querySelector({}); return el ? (el.{}) : ''; }})()",
        json!(sel).to_string(),
        prop
      ),
      None => format!(
        "(() => {{ const el = document.body || document.documentElement; return el ? (el.{}) : ''; }})()",
        prop
      ),
    };
    Ok(self.eval(&expr)?.as_str().unwrap_or_default().to_string())
  }

  /* ---------------- interaction ---------------- */

  /// Click the element matching `selector` with a real mouse event.
  pub fn click(&self, selector: &str) -> Result<(), String> {
    let js = format!(
      "(() => {{ const el = document.querySelector({}); if (!el) return null; \
       el.scrollIntoView({{ block: 'center', inline: 'center' }}); \
       const r = el.getBoundingClientRect(); \
       return {{ x: r.left + r.width / 2, y: r.top + r.height / 2 }}; }})()",
      json!(selector).to_string()
    );
    let pt = self.eval(&js)?;
    let (x, y) = match (pt["x"].as_f64(), pt["y"].as_f64()) {
      (Some(x), Some(y)) => (x, y),
      _ => return Err(format!("no element matches {}", selector)),
    };
    for (kind, buttons) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 0)] {
      self.cdp.call(
        "Input.dispatchMouseEvent",
        &json!({ "type": kind, "x": x, "y": y, "button": "left", "buttons": buttons, "clickCount": 1 })
          .to_string(),
      )?;
    }
    Ok(())
  }

  /// Focus the field and type `value` into it.
  pub fn fill(&self, selector: &str, value: &str) -> Result<(), String> {
    let focus = format!(
      "(() => {{ const el = document.querySelector({}); if (!el) return false; el.focus(); \
       if (typeof el.select === 'function') el.select(); \
       else if (typeof el.setSelectionRange === 'function') el.setSelectionRange(0, (el.value || '').length); \
       return true; }})()",
      json!(selector).to_string()
    );
    if self.eval(&focus)? != Value::Bool(true) {
      return Err(format!("no element matches {}", selector));
    }
    // replace whatever was selected with the new text
    self
      .cdp
      .call("Input.insertText", &json!({ "text": value }).to_string())?;
    Ok(())
  }

  /// Poll until the selector matches an element.
  pub fn wait_for(&self, selector: &str, timeout: Duration) -> Result<Value, String> {
    let expr = format!("!!document.querySelector({})", json!(selector).to_string());
    let deadline = Instant::now() + timeout;
    loop {
      if self.eval(&expr).ok() == Some(Value::Bool(true)) {
        return Ok(json!({ "found": true, "selector": selector }));
      }
      if Instant::now() >= deadline {
        return Err(format!("timed out after {:?} waiting for {}", timeout, selector));
      }
      std::thread::sleep(Duration::from_millis(100));
    }
  }

  /* ---------------- capture ---------------- */

  /// Grab a PNG of the current page, base64 encoded.
  ///
  /// The window is hidden but WebView2 still composites, so the frame is a real
  /// render of the page. This is the one CDP capability WebView2 does not offer
  /// as `Page.captureScreenshot`, hence the screencast route.
  pub fn screenshot_b64(&self, timeout: Duration) -> Result<String, String> {
    let (tx, rx) = mpsc::channel::<String>();
    *self.frames.lock().unwrap() = Some(tx);

    let start = self.cdp.call(
      "Page.startScreencast",
      &json!({ "format": "png", "quality": 90, "maxWidth": 0, "maxHeight": 0, "everyNthFrame": 1 })
        .to_string(),
    );
    let result = match start {
      Ok(_) => rx.recv_timeout(timeout).map_err(|_| "no screencast frame arrived".to_string()),
      Err(e) => Err(e),
    };
    let _ = self.cdp.call("Page.stopScreencast", "{}");
    *self.frames.lock().unwrap() = None;

    let frame: Value = serde_json::from_str(&result?).map_err(|e| e.to_string())?;
    let _ = self.cdp.call(
      "Page.screencastFrameAck",
      &json!({ "sessionId": frame["sessionId"].as_i64().unwrap_or(0) }).to_string(),
    );
    Ok(frame["data"].as_str().ok_or("screencast frame carried no data")?.to_string())
  }

  /// The same frame, decoded for callers that write a `.png` to disk.
  pub fn screenshot(&self, timeout: Duration) -> Result<Vec<u8>, String> {
    b64_decode(&self.screenshot_b64(timeout)?)
  }

  /* ---------------- state ---------------- */

  /// Cookies the automation profile currently holds for the active page.
  pub fn cookies(&self) -> Result<Value, String> {
    let url = self.eval("location.href").ok().and_then(|v| v.as_str().map(|s| s.to_string()));
    let params = match &url {
      Some(u) if !u.is_empty() && u != "about:blank" => json!({ "urls": [u] }),
      _ => json!({}),
    };
    let r = self.cdp.call("Network.getCookies", &params.to_string())?;
    let cookies: Vec<Value> = r["cookies"]
      .as_array()
      .map(|a| {
        a.iter()
          .map(|c| {
            json!({
              "name": c["name"],
              "value": c["value"],
              "domain": c["domain"],
              "path": c["path"],
              "expires": c["expires"],
              "httpOnly": c["httpOnly"],
              "secure": c["secure"],
              "session": c["session"],
            })
          })
          .collect()
      })
      .unwrap_or_default();
    Ok(json!({ "cookies": cookies, "count": cookies.len() }))
  }

  /// Network traffic recorded so far (already redacted).
  pub fn traffic(&self, limit: usize) -> Value {
    let items = self.traffic.list(limit);
    json!({ "count": items.len(), "requests": items })
  }
}

/* ---------------- base64 ---------------- */

fn b64_val(c: u8) -> Option<u8> {
  match c {
    b'A'..=b'Z' => Some(c - b'A'),
    b'a'..=b'z' => Some(c - b'a' + 26),
    b'0'..=b'9' => Some(c - b'0' + 52),
    b'+' => Some(62),
    b'/' => Some(63),
    _ => None,
  }
}

/// Decode standard base64 (the form CDP screencast frames use).
fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
  let mut out = Vec::with_capacity(s.len() / 4 * 3);
  let mut acc: u32 = 0;
  let mut bits = 0u32;
  for c in s.bytes() {
    if c == b'=' || c == b'\n' || c == b'\r' {
      continue;
    }
    let Some(v) = b64_val(c) else { return Err(format!("invalid base64 byte 0x{:02x}", c)) };
    acc = (acc << 6) | v as u32;
    bits += 6;
    if bits >= 8 {
      bits -= 8;
      out.push((acc >> bits) as u8);
    }
  }
  if out.is_empty() {
    return Err("empty base64 payload".into());
  }
  Ok(out)
}
