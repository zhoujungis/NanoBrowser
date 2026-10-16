//! Network traffic capture.
//!
//! `Network.requestWillBeSent` / `Network.responseReceived` are folded into one
//! record per request. Sensitive material — credentials in form bodies, JSON
//! secrets, `Authorization` / `Cookie` headers — is redacted *at capture time*,
//! so a secret never reaches the log buffer in the first place. Bodies are
//! truncated so a large upload cannot blow up the buffer.
//!
//! The log lives in memory only: nothing is written to disk, so there is no way
//! for captured traffic to end up on the system drive.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};

use nanobrowser::cdp::Cdp;

const MAX_ENTRIES: usize = 500;
const MAX_BODY: usize = 4096;
const MAX_VALUE: usize = 512;

/// Parameter/header names whose value must never be logged.
const SENSITIVE: [&str; 18] = [
  "password",
  "passwd",
  "pwd",
  "pass",
  "token",
  "secret",
  "apikey",
  "api_key",
  "api-key",
  "auth",
  "cookie",
  "session",
  "csrf",
  "credit",
  "card",
  "cvv",
  "ssn",
  "otp",
];

fn is_sensitive(key: &str) -> bool {
  let k = key.to_ascii_lowercase();
  SENSITIVE.iter().any(|s| k.contains(s))
}

const REDACTED: &str = "***";

fn truncate(s: &str, max: usize) -> String {
  if s.chars().count() <= max {
    s.to_string()
  } else {
    let cut: String = s.chars().take(max).collect();
    format!("{}…(+{} chars)", cut, s.chars().count() - max)
  }
}

/// Redact an object of headers / query parameters keyed by name.
fn redact_map(obj: &Map<String, Value>) -> Value {
  let mut out = Map::new();
  for (k, v) in obj {
    if is_sensitive(k) {
      out.insert(k.clone(), Value::String(REDACTED.into()));
    } else {
      let s = match v {
        Value::String(s) => truncate(s, MAX_VALUE),
        other => truncate(&other.to_string(), MAX_VALUE),
      };
      out.insert(k.clone(), Value::String(s));
    }
  }
  Value::Object(out)
}

fn redact_headers(v: &Value) -> Value {
  match v.as_object() {
    Some(o) => redact_map(o),
    None => Value::Null,
  }
}

/// Recursively redact JSON values whose key looks sensitive.
fn redact_json(v: &Value) -> Value {
  match v {
    Value::Object(o) => {
      let mut out = Map::new();
      for (k, val) in o {
        out.insert(
          k.clone(),
          if is_sensitive(k) { Value::String(REDACTED.into()) } else { redact_json(val) },
        );
      }
      Value::Object(out)
    }
    Value::Array(a) => Value::Array(a.iter().map(redact_json).collect()),
    other => other.clone(),
  }
}

/// Redact a request body: JSON, form-encoded, or free text.
fn redact_body(body: &str) -> String {
  let body = truncate(body, MAX_BODY);
  let trimmed = body.trim_start();
  if trimmed.starts_with('{') || trimmed.starts_with('[') {
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
      return redact_json(&v).to_string();
    }
  }
  if body.contains('=') && !trimmed.starts_with('<') {
    let pairs: Vec<String> = body
      .split('&')
      .map(|pair| match pair.split_once('=') {
        Some((k, _)) if is_sensitive(k) => format!("{}={}", k, REDACTED),
        _ => pair.to_string(),
      })
      .collect();
    return pairs.join("&");
  }
  body
}

#[derive(Clone)]
struct Entry {
  id: String,
  url: String,
  method: String,
  resource_type: String,
  request_headers: Value,
  post_body: Option<String>,
  status: Option<i64>,
  mime: Option<String>,
  response_headers: Option<Value>,
}

impl Entry {
  fn to_json(&self) -> Value {
    json!({
      "id": self.id,
      "url": self.url,
      "method": self.method,
      "type": self.resource_type,
      "status": self.status,
      "mime": self.mime,
      "requestHeaders": self.request_headers,
      "requestBody": self.post_body,
      "responseHeaders": self.response_headers,
    })
  }
}

#[derive(Default)]
struct Inner {
  /// entry keys, oldest first — what `list` walks
  order: Vec<String>,
  entries: HashMap<String, Entry>,
  /// requestId -> the entry that is currently accumulating that request
  current: HashMap<String, String>,
  /// counter for minting extra keys when one requestId spans a redirect chain
  chain: u32,
}

fn evict(g: &mut Inner) {
  while g.order.len() > MAX_ENTRIES {
    let old = g.order.remove(0);
    g.entries.remove(&old);
    g.current.retain(|_, k| k != &old);
  }
}

/// Rolling in-memory record of every request the page made.
#[derive(Default)]
pub struct Traffic {
  inner: Mutex<Inner>,
}

impl Traffic {
  /// Hook the CDP network events. Must run before `Network.enable`.
  pub fn attach(self: &Arc<Self>, cdp: &Arc<Cdp>) -> Result<(), String> {
    let t = self.clone();
    cdp.on_event("Network.requestWillBeSent", move |p| t.on_request(&p))?;
    let t = self.clone();
    cdp.on_event("Network.responseReceived", move |p| t.on_response(&p))?;
    Ok(())
  }

  fn on_request(self: &Arc<Self>, payload: &str) {
    let Ok(v) = serde_json::from_str::<Value>(payload) else { return };
    let rid = v["requestId"].as_str().unwrap_or_default().to_string();
    if rid.is_empty() {
      return;
    }
    let req = &v["request"];
    let entry = Entry {
      id: rid.clone(),
      url: req["url"].as_str().unwrap_or_default().to_string(),
      method: req["method"].as_str().unwrap_or("GET").to_string(),
      resource_type: v["type"].as_str().unwrap_or_default().to_string(),
      request_headers: redact_headers(&req["headers"]),
      post_body: req["postData"].as_str().map(redact_body),
      status: None,
      mime: None,
      response_headers: None,
    };

    let mut g = self.inner.lock().unwrap();
    let previous = g.current.get(&rid).cloned();

    // A redirect re-uses the requestId. Seal the finished hop with the redirect
    // response, then file the follow-up under its own key — otherwise the
    // follow-up would overwrite the original and its (redacted) body would be
    // lost, which is exactly the record that matters for a login POST.
    if let Some(prev_key) = &previous {
      let redirect = &v["redirectResponse"];
      if !redirect.is_null() {
        if let Some(prev) = g.entries.get_mut(prev_key) {
          prev.status = redirect["status"].as_i64();
          prev.mime = redirect["mimeType"].as_str().map(str::to_string);
          prev.response_headers = Some(redact_headers(&redirect["headers"]));
        }
      }
    }

    let key = match previous {
      Some(_) => {
        g.chain += 1;
        format!("{}~{}", rid, g.chain)
      }
      None => rid.clone(),
    };
    g.current.insert(rid, key.clone());
    g.order.push(key.clone());
    g.entries.insert(key, entry);
    evict(&mut g);
  }

  fn on_response(self: &Arc<Self>, payload: &str) {
    let Ok(v) = serde_json::from_str::<Value>(payload) else { return };
    let rid = v["requestId"].as_str().unwrap_or_default();
    let mut g = self.inner.lock().unwrap();
    let Some(key) = g.current.get(rid).cloned() else { return };
    let Some(entry) = g.entries.get_mut(&key) else { return };
    let res = &v["response"];
    entry.status = res["status"].as_i64();
    entry.mime = res["mimeType"].as_str().map(str::to_string);
    entry.response_headers = Some(redact_headers(&res["headers"]));
  }

  /// The most recent `limit` recorded requests, oldest first.
  pub fn list(&self, limit: usize) -> Vec<Value> {
    let g = self.inner.lock().unwrap();
    let start = g.order.len().saturating_sub(limit);
    g.order[start..]
      .iter()
      .filter_map(|k| g.entries.get(k))
      .map(Entry::to_json)
      .collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn request(rid: &str, url: &str, headers: Value, body: Option<&str>) -> String {
    json!({
      "requestId": rid,
      "type": "XHR",
      "request": { "url": url, "method": "POST", "headers": headers, "postData": body },
    })
    .to_string()
  }

  fn response(rid: &str, status: i64, headers: Value) -> String {
    json!({
      "requestId": rid,
      "response": { "status": status, "mimeType": "text/html", "headers": headers },
    })
    .to_string()
  }

  fn keyed(k: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(k.into(), Value::String("v".into()));
    m
  }

  #[test]
  fn secret_shaped_names_are_recognised_in_any_case_or_position() {
    for k in [
      "password", "PASSWORD", "user_password", "passwd", "pwd", "pass", "access_token", "token",
      "clientSecret", "apiKey", "api_key", "x-api-key", "authorization", "cookie", "SessionId",
      "csrf_token", "credit_card", "card_number", "cvv", "ssn", "otp",
    ] {
      assert!(is_sensitive(k), "{} must be redacted", k);
    }
    for k in ["username", "email", "q", "search", "page", "monkey", "tiny", "okay"] {
      assert!(!is_sensitive(k), "{} is not a secret", k);
    }
  }

  #[test]
  fn truncate_counts_characters_and_never_splits_one() {
    assert_eq!(truncate("abc", 3), "abc");
    assert_eq!(truncate("abcd", 3), "abc…(+1 chars)");
    // a multi-byte string is cut on a character boundary, not a byte one
    let s = "中文测试";
    assert_eq!(truncate(s, 2), "中文…(+2 chars)");
    assert_eq!(truncate(s, 4), s);
  }

  #[test]
  fn header_values_are_redacted_by_name_and_capped_in_size() {
    let mut m = Map::new();
    m.insert("Authorization".into(), Value::String("Bearer sk-live-123".into()));
    m.insert("cookie".into(), Value::String("sid=abc".into()));
    m.insert("Content-Type".into(), Value::String("application/json".into()));
    m.insert("X-Long".into(), Value::String("z".repeat(600)));
    let out = redact_headers(&Value::Object(m));
    assert_eq!(out["Authorization"], REDACTED);
    assert_eq!(out["cookie"], REDACTED);
    assert_eq!(out["Content-Type"], "application/json");
    assert_eq!(out["X-Long"].as_str().unwrap().chars().count(), 512 + "…(+88 chars)".chars().count());
    assert_eq!(redact_headers(&Value::Null), Value::Null, "no headers at all");
  }

  #[test]
  fn json_secrets_are_redacted_no_matter_how_deeply_they_are_wrapped() {
    let body = r#"{"user":"ana","password":"hunter2","items":[{"token":"t-9"},{"note":"fine"}],"outer":{"inner":{"cvv":"123"}}}"#;
    let out = redact_body(body);
    for secret in ["hunter2", "t-9", "123"] {
      assert!(!out.contains(secret), "{} leaked out of {}", secret, out);
    }
    assert_eq!(out.matches(REDACTED).count(), 3);
    assert!(out.contains("ana") && out.contains("fine"), "the rest survives");
  }

  #[test]
  fn form_bodies_are_redacted_pair_by_pair() {
    let out = redact_body("user=ana&password=hunter2&csrf_token=t9&keep=1&empty=");
    assert_eq!(out, "user=ana&password=***&csrf_token=***&keep=1&empty=");
  }

  #[test]
  fn bodies_that_are_neither_json_nor_a_form_are_left_alone() {
    assert_eq!(redact_body("plain description text"), "plain description text");
    // markup can contain '=' without being a form, and must not be mangled
    assert_eq!(redact_body("<div class=\"a=b\">x</div>"), "<div class=\"a=b\">x</div>");
    // JSON that does not parse is handed back as text rather than dropped
    assert_eq!(redact_body("{not json}"), "{not json}");
  }

  #[test]
  fn a_large_body_is_truncated_before_it_can_be_parsed() {
    let secret = format!("{{\"password\":\"{}\"}}", "p".repeat(MAX_BODY * 2));
    let out = redact_body(&secret);
    assert!(out.starts_with("{\"password\":\"pppp"));
    assert!(out.ends_with("chars)"), "truncated, and the log says so");
    assert!(out.chars().count() < MAX_BODY + 32);
  }

  #[test]
  fn a_request_is_recorded_and_its_secrets_never_reach_the_log() {
    let t = Arc::new(Traffic::default());
    let mut h = Map::new();
    h.insert("Authorization".into(), Value::String("Bearer sk-live-123".into()));
    h.insert("Accept".into(), Value::String("*/*".into()));
    t.on_request(&request("r1", "https://site/login", Value::Object(h), Some("user=ana&password=hunter2")));
    t.on_response(&response("r1", 200, Value::Object(keyed("Set-Cookie"))));

    let out = serde_json::to_string(&t.list(10)).unwrap();
    for secret in ["sk-live-123", "hunter2"] {
      assert!(!out.contains(secret), "{} leaked into {}", secret, out);
    }
    let v = &t.list(10)[0];
    assert_eq!(v["url"], "https://site/login");
    assert_eq!(v["status"], 200);
    assert_eq!(v["requestBody"], "user=ana&password=***");
    assert_eq!(v["requestHeaders"]["Accept"], "*/*");
    assert_eq!(v["responseHeaders"]["Set-Cookie"], REDACTED);
  }

  #[test]
  fn a_redirect_chain_keeps_both_hops_under_their_own_keys() {
    // a login POST that 302s: the requestId is reused, and the first hop's body
    // is the record that matters, so it must not be overwritten
    let t = Arc::new(Traffic::default());
    t.on_request(&request("r1", "https://site/login", Value::Null, Some("user=ana&password=hunter2")));
    let mut redirect = serde_json::from_str::<Value>(&request("r1", "https://site/home", Value::Null, None)).unwrap();
    redirect["redirectResponse"] = json!({ "status": 302, "mimeType": "text/plain", "headers": { "Location": "https://site/home" } });
    t.on_request(&redirect.to_string());
    t.on_response(&response("r1", 200, Value::Null));

    let log = t.list(10);
    assert_eq!(log.len(), 2, "one entry per hop");
    assert_eq!(log[0]["url"], "https://site/login");
    assert_eq!(log[0]["status"], 302, "the hop is sealed with the redirect response");
    assert_eq!(log[0]["requestBody"], "user=ana&password=***");
    assert_eq!(log[1]["url"], "https://site/home");
    assert_eq!(log[1]["status"], 200, "and the follow-up gets its own response");
  }

  #[test]
  fn events_without_a_usable_id_are_ignored() {
    let t = Arc::new(Traffic::default());
    t.on_request(r#"{"type":"XHR"}"#);
    t.on_request("not json");
    t.on_response(r#"{"requestId":"nope","response":{"status":200}}"#);
    assert!(t.list(10).is_empty());
    // a response for a request that was never seen leaves the log alone
    t.on_request(&request("r1", "https://a", Value::Null, None));
    t.on_response(r#"{"requestId":"other","response":{"status":500}}"#);
    assert_eq!(t.list(10).len(), 1);
    assert_eq!(t.list(10)[0]["status"], Value::Null);
  }

  #[test]
  fn the_log_stops_growing_at_its_cap_by_dropping_the_oldest() {
    let t = Arc::new(Traffic::default());
    for i in 0..(MAX_ENTRIES + 25) {
      t.on_request(&request(&format!("r{}", i), &format!("https://a/{}", i), Value::Null, None));
    }
    let log = t.list(usize::MAX);
    assert_eq!(log.len(), MAX_ENTRIES);
    assert_eq!(log[0]["url"], format!("https://a/{}", 25), "the first 25 fell off");
    assert_eq!(log[MAX_ENTRIES - 1]["url"], format!("https://a/{}", MAX_ENTRIES + 24));
    // and `list` hands back the newest tail in oldest-first order
    let tail = t.list(3);
    assert_eq!(tail[0]["url"], format!("https://a/{}", MAX_ENTRIES + 22));
  }
}

