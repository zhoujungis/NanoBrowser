//! End-to-end tests for the headless agent front ends.
//!
//! These launch the real `nanoagent` binary against a throwaway HTTP server on
//! loopback, using the same WebView2 engine the browser ships. What they pin
//! down is the part unit tests cannot reach: that a command really reaches a
//! page, that a screenshot really is a PNG, that credentials never survive into
//! the traffic log, and that a profile written by one process is readable by the
//! next — the three promises the agent makes to an AI tool.
//!
//! Each test gets its own automation profile under `target/…/_agent_e2e`, so
//! parallel runs never share a WebView2 user-data directory and nothing is ever
//! written outside the project.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// The agent binary cargo builds alongside these tests.
const EXE: &str = env!("CARGO_BIN_EXE_nanoagent");

/* ---------------- a throwaway site ---------------- */

/// The page every command is pointed at. The click handler and the delayed
/// `#late` div give the interaction commands something observable to act on, and
/// the synchronous XHR is what makes the traffic log deterministic: it has
/// finished before `readyState` turns `complete`, so `navigate` never returns
/// before its credentials have gone over the wire.
const PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Nano E2E</title></head>
<body>
<h1 id="head">hello nano</h1>
<p class="note">the quick brown fox</p>
<input id="name" value="">
<button id="go">go</button>
<script>
document.getElementById('go').addEventListener('click', function () {
  document.title = 'clicked-nano';
});
(function () {
  var x = new XMLHttpRequest();
  x.open('POST', '/leak', false);
  x.setRequestHeader('Authorization', 'Bearer sk-live-123');
  x.setRequestHeader('Content-Type', 'application/x-www-form-urlencoded');
  x.send('user=ana&password=hunter2');
})();
setTimeout(function () {
  var d = document.createElement('div');
  d.id = 'late';
  d.textContent = 'arrived';
  document.body.appendChild(d);
}, 300);
</script>
</body></html>"#;

/// Serve the test site on an ephemeral loopback port; returns that port.
fn start_site() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").expect("loopback should be bindable");
  let port = listener.local_addr().unwrap().port();
  thread::spawn(move || {
    for stream in listener.incoming() {
      let Ok(s) = stream else { continue };
      thread::spawn(move || handle(s));
    }
  });
  port
}

fn handle(mut s: TcpStream) {
  let mut buf = Vec::new();
  let mut tmp = [0u8; 2048];
  loop {
    match s.read(&mut tmp) {
      Ok(0) | Err(_) => return,
      Ok(n) => buf.extend_from_slice(&tmp[..n]),
    }
    let Some(at) = find(&buf, b"\r\n\r\n") else { continue };
    let head = String::from_utf8_lossy(&buf[..at]).to_string();
    let body_have = buf.len() - (at + 4);
    let mut left = header_number(&head, "content-length").saturating_sub(body_have);
    while left > 0 {
      match s.read(&mut tmp) {
        Ok(0) | Err(_) => break,
        Ok(n) => left = left.saturating_sub(n),
      }
    }
    route(&mut s, &head);
    return;
  }
}

fn route(s: &mut TcpStream, head: &str) {
  let line = head.lines().next().unwrap_or_default();
  let path = line.split_whitespace().nth(1).unwrap_or("/");
  let path = path.split('?').next().unwrap_or("/");
  match path {
    // a persistent cookie, so surviving into the next process does not depend on
    // WebView2's session-restore behaviour
    "/setcookie" => respond(s, "200 OK", &[("Set-Cookie", "sid=abc123; Max-Age=3600; Path=/")], "<html><body>cookie set</body></html>"),
    "/leak" => respond(s, "200 OK", &[], "ok"),
    _ => respond(s, "200 OK", &[], PAGE),
  }
}

fn respond(s: &mut TcpStream, status: &str, extra: &[(&str, &str)], body: &str) {
  let mut head = format!(
    "HTTP/1.1 {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n",
    status,
    body.len()
  );
  for (k, v) in extra {
    head.push_str(&format!("{}: {}\r\n", k, v));
  }
  head.push_str("\r\n");
  let _ = s.write_all(head.as_bytes());
  let _ = s.write_all(body.as_bytes());
  let _ = s.flush();
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
  hay.windows(needle.len()).position(|w| w == needle)
}

fn header_number(head: &str, name: &str) -> usize {
  head
    .lines()
    .filter_map(|l| l.split_once(':'))
    .find(|(k, _)| k.trim().eq_ignore_ascii_case(name))
    .and_then(|(_, v)| v.trim().parse().ok())
    .unwrap_or(0)
}

/* ---------------- driving the binary ---------------- */

fn scratch(tag: &str) -> PathBuf {
  let base = std::env::current_exe().unwrap().parent().unwrap().join("_agent_e2e");
  let dir = base.join(format!("{}-{}", tag, std::process::id()));
  let _ = fs::remove_dir_all(&dir);
  fs::create_dir_all(&dir).expect("scratch dir should be creatable");
  dir
}

/// A bare CI image may ship without the WebView2 runtime. When that happens the
/// agent says so on stderr and exits; the test reports it and passes, rather
/// than failing over an engine it was never given.
fn cannot_host_webview(code: i32, err: &str) -> bool {
  let e = err.to_ascii_lowercase();
  code != 0
    && (e.contains("never became responsive")
      || e.contains("unable to initialise")
      || e.contains("nanoagent failed to start")
      || e.contains("webview2"))
}

fn announce_skip() {
  eprintln!("skipping: this environment has no WebView2 runtime to drive");
}

/// Run one command in `dir`'s profile. `None` means the environment cannot host
/// the engine at all; anything else is handed back for the test to judge.
fn run_or_skip(dir: &Path, args: &[&str]) -> Option<(i32, String, String)> {
  let out = Command::new(EXE)
    .args(args)
    .env("NANOBROWSER_AGENT_DIR", dir)
    .output()
    .expect("nanoagent should launch");
  let code = out.status.code().unwrap_or(-1);
  let stdout = String::from_utf8_lossy(&out.stdout).to_string();
  let stderr = String::from_utf8_lossy(&out.stderr).to_string();
  if cannot_host_webview(code, &stderr) {
    announce_skip();
    return None;
  }
  Some((code, stdout, stderr))
}

/// Run one command that must succeed, and return its stdout.
fn ok(dir: &Path, args: &[&str]) -> Option<String> {
  let (code, stdout, stderr) = run_or_skip(dir, args)?;
  assert_eq!(code, 0, "nanoagent {:?} should succeed\nstdout:\n{}\nstderr:\n{}", args, stdout, stderr);
  Some(stdout.trim().to_string())
}

fn cleanup(dir: &Path) {
  // WebView2's helper processes let go of the profile a moment after the agent
  // exits, so the first attempts can trip over a locked file
  for _ in 0..20 {
    if fs::remove_dir_all(dir).is_ok() {
      return;
    }
    thread::sleep(Duration::from_millis(250));
  }
}

/* ---------------- MCP, over a real stdio pipe ---------------- */

fn mcp_session(dir: &Path, steps: &[Value]) -> Option<Vec<Value>> {
  let mut child = Command::new(EXE)
    .arg("--mcp")
    .env("NANOBROWSER_AGENT_DIR", dir)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("nanoagent should launch");

  let mut stdin = child.stdin.take().unwrap();
  let (tx, rx) = channel::<String>();
  drain(child.stdout.take().unwrap(), tx);
  let errs = Arc::new(Mutex::new(String::new()));
  drain(child.stderr.take().unwrap(), errs.clone());

  let mut replies = Vec::new();
  for step in steps {
    if writeln!(stdin, "{}", step).is_err() || stdin.flush().is_err() {
      break;
    }
    match rx.recv_timeout(Duration::from_secs(120)) {
      Ok(line) => replies.push(serde_json::from_str::<Value>(&line).expect("a reply must be JSON")),
      Err(_) => break,
    }
  }
  drop(stdin);
  wait_bounded(&mut child, Duration::from_secs(20));

  if replies.len() < steps.len() {
    let e = errs.lock().unwrap().clone();
    if cannot_host_webview(2, &e) {
      announce_skip();
      return None;
    }
    panic!("the MCP session stopped after {} of {} steps\nstderr:\n{}", replies.len(), steps.len(), e);
  }
  Some(replies)
}

/// Forward a stream's lines, either to a channel or into a shared buffer.
trait Sink {
  fn line(&self, l: String);
}
impl Sink for std::sync::mpsc::Sender<String> {
  fn line(&self, l: String) {
    let _ = self.send(l);
  }
}
impl Sink for Arc<Mutex<String>> {
  fn line(&self, l: String) {
    let mut s = self.lock().unwrap();
    s.push_str(&l);
    s.push('\n');
  }
}

fn drain<R: Read + Send + 'static, S: Sink + Send + 'static>(r: R, sink: S) {
  thread::spawn(move || {
    for line in BufReader::new(r).lines().map_while(Result::ok) {
      sink.line(line);
    }
  });
}

fn wait_bounded(child: &mut Child, budget: Duration) {
  let deadline = Instant::now() + budget;
  loop {
    match child.try_wait() {
      Ok(Some(_)) => return,
      Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
      _ => {
        let _ = child.kill();
        let _ = child.wait();
        return;
      }
    }
  }
}

fn reply_text(reply: &Value) -> String {
  reply["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string()
}

fn call(id: u32, tool: &str, args: Value) -> Value {
  json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call",
          "params": { "name": tool, "arguments": args } })
}

/* ---------------- tests ---------------- */

#[test]
fn one_shot_commands_reach_a_real_page() {
  let dir = scratch("cli");
  let port = start_site();
  let url = format!("http://127.0.0.1:{}/", port);

  // the whole page as text
  let Some(body) = ok(&dir, &["read", &url]) else { return };
  assert!(body.contains("hello nano"), "page text: {}", body);
  assert!(body.contains("the quick brown fox"), "page text: {}", body);

  // one element, as text and as markup
  let Some(head) = ok(&dir, &["read", &url, "#head"]) else { return };
  assert_eq!(head, "hello nano");
  let Some(html) = ok(&dir, &["read", &url, "#head", "--html"]) else { return };
  assert!(html.contains("<h1 id=\"head\">"), "element html: {}", html);

  // a value crossing the JS boundary intact
  let Some(n) = ok(&dir, &["eval", &url, "document.querySelectorAll('p.note').length"]) else { return };
  assert_eq!(n, "1");

  // a selector that only appears after the page settles
  let Some(found) = ok(&dir, &["wait", &url, "#late"]) else { return };
  assert!(found.contains("\"found\":true"), "wait output: {}", found);

  // and a selector that cannot be resolved is a loud failure, not a silent pass
  let (code, _, err) = run_or_skip(&dir, &["wait", &url, "#absent", "400"]).expect("webview");
  assert_eq!(code, 1, "waiting for a missing element must fail");
  assert!(err.contains("timed out"), "stderr: {}", err);

  cleanup(&dir);
}

#[test]
fn a_screenshot_is_a_real_png() {
  let dir = scratch("shot");
  let port = start_site();
  let url = format!("http://127.0.0.1:{}/", port);
  let png = dir.join("page.png");

  let Some(out) = ok(&dir, &["shot", &url, png.to_str().unwrap()]) else { return };
  assert!(out.contains("\"bytes\""), "shot output: {}", out);

  let bytes = fs::read(&png).expect("the screenshot should have been written");
  assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "a PNG signature");
  assert!(bytes.len() > 1000, "a real render, not an empty canvas: {} bytes", bytes.len());

  cleanup(&dir);
}

#[test]
fn traffic_logs_requests_but_never_their_credentials() {
  let dir = scratch("traffic");
  let port = start_site();
  let url = format!("http://127.0.0.1:{}/", port);

  let Some(out) = ok(&dir, &["traffic", &url, "50"]) else { return };
  assert!(out.contains("/leak"), "the XHR should have been recorded: {}", out);
  assert!(out.contains("password=***"), "the form body is kept, its secret is not: {}", out);

  // the two secrets the page actually sent must not appear anywhere
  for secret in ["hunter2", "sk-live-123", "Bearer"] {
    assert!(!out.contains(secret), "{} leaked into the traffic log", secret);
  }

  cleanup(&dir);
}

#[test]
fn cookies_survive_between_processes() {
  let dir = scratch("cookies");
  let port = start_site();
  let url = format!("http://127.0.0.1:{}/setcookie", port);

  // first process: the response hands the page a cookie
  let Some(first) = ok(&dir, &["cookies", &url]) else { return };
  assert!(first.contains("abc123"), "the cookie was set on this visit: {}", first);

  // second process, same profile: the login state is still there
  let Some(second) = ok(&dir, &["cookies", &url]) else { return };
  assert!(second.contains("\"sid\""), "the cookie should still be listed: {}", second);
  assert!(second.contains("abc123"), "and keep its value: {}", second);

  cleanup(&dir);
}

#[test]
fn unknown_commands_and_missing_arguments_fail_loudly() {
  let dir = scratch("errors");

  let Some((code, _, err)) = run_or_skip(&dir, &["bogus"]) else { return };
  assert_eq!(code, 1, "an unknown command is a failure, not a no-op");
  assert!(err.contains("unknown command: bogus"), "stderr: {}", err);

  let Some((code, _, err)) = run_or_skip(&dir, &["navigate"]) else { return };
  assert_eq!(code, 1, "a missing argument is a failure");
  assert!(err.contains("missing argument: url"), "stderr: {}", err);

  cleanup(&dir);
}

#[test]
fn an_mcp_session_keeps_one_page_warm_across_calls() {
  let dir = scratch("mcp");
  let port = start_site();
  let url = format!("http://127.0.0.1:{}/", port);

  let steps = vec![
    json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": "2024-11-05" } }),
    json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
    call(3, "browser_navigate", json!({ "url": url })),
    // a value read back from the page proves the navigate really landed
    call(4, "browser_eval", json!({ "expression": "document.title" })),
    // a trusted click must actually run the page's handler
    call(5, "browser_click", json!({ "selector": "#go" })),
    call(6, "browser_eval", json!({ "expression": "document.title" })),
    // typing must land in the field
    call(7, "browser_fill", json!({ "selector": "#name", "value": "ana-42" })),
    call(8, "browser_eval", json!({ "expression": "document.querySelector('#name').value" })),
    call(9, "browser_wait_for", json!({ "selector": "#late" })),
    call(10, "browser_screenshot", json!({})),
    call(11, "browser_traffic", json!({ "limit": 50 })),
    call(12, "browser_nope", json!({})),
    json!({ "jsonrpc": "2.0", "id": 13, "method": "ping" }),
  ];

  let Some(r) = mcp_session(&dir, &steps) else { return };

  // handshake: the client's protocol version is echoed and tools are advertised
  assert_eq!(r[0]["result"]["protocolVersion"], "2024-11-05");
  assert_eq!(r[0]["result"]["serverInfo"]["name"], "nanobrowser-agent");
  let names: Vec<&str> = r[1]["result"]["tools"]
    .as_array()
    .expect("tools must be a list")
    .iter()
    .filter_map(|t| t["name"].as_str())
    .collect();
  for t in ["browser_navigate", "browser_click", "browser_fill", "browser_screenshot", "browser_traffic"] {
    assert!(names.contains(&t), "{} should be advertised, got {:?}", t, names);
  }

  assert!(reply_text(&r[2]).contains("Nano E2E"), "navigate landed on our page");
  assert!(reply_text(&r[3]).contains("Nano E2E"), "title before the click");
  // if the click were synthetic or lost, the title would not have changed
  assert!(reply_text(&r[5]).contains("clicked-nano"), "the trusted click ran the handler: {}", reply_text(&r[5]));
  assert!(reply_text(&r[7]).contains("ana-42"), "the typed value is in the field");
  assert!(reply_text(&r[8]).contains("\"found\":true"), "the late element arrived");

  // the screenshot comes back as a PNG data payload, not a URL
  let shot = &r[9]["result"]["content"][0];
  assert_eq!(shot["type"], "image");
  assert_eq!(shot["mimeType"], "image/png");
  let data = shot["data"].as_str().expect("image data");
  assert!(data.starts_with("iVBORw0KGgo"), "base64 PNG magic: {}", &data[..12.min(data.len())]);

  // and the traffic log of that same warm session is redacted too
  let traffic = reply_text(&r[10]);
  assert!(traffic.contains("/leak"), "the page's XHR was recorded");
  assert!(!traffic.contains("hunter2") && !traffic.contains("sk-live-123"), "no secrets in {}", traffic);

  // protocol edges: an unknown tool is a JSON-RPC error, ping answers
  assert_eq!(r[11]["error"]["code"], -32602);
  assert!(r[11]["result"].is_null(), "an error reply carries no result");
  assert_eq!(r[12]["result"], json!({}));

  cleanup(&dir);
}
