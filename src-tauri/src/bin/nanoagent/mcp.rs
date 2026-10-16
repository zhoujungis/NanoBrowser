//! Model Context Protocol server over stdio.
//!
//! The agent speaks newline-delimited JSON-RPC 2.0: one message per line on
//! stdin, one reply per line on stdout. Every log line goes to stderr so the
//! stdout stream stays a clean protocol channel.
//!
//! The browser stays warm between calls, so an AI tool can navigate, inspect,
//! and interact with the same page across many tool calls — this is what makes
//! an authenticated session possible.

use std::io::{BufRead, BufReader, Write};

use serde_json::{json, Value};

use crate::browser::{Browser, DEFAULT_TIMEOUT};

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Serve MCP on stdio until the client closes it.
pub fn serve(browser: &Browser) {
  let stdin = std::io::stdin();
  for line in BufReader::new(stdin.lock()).lines() {
    let Ok(line) = line else { break };
    let line = line.trim();
    if line.is_empty() {
      continue;
    }
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
      continue;
    };
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg["method"].as_str().unwrap_or_default();
    let params = msg.get("params").cloned().unwrap_or(json!({}));

    // notifications carry no id and get no reply
    if id.is_null() {
      continue;
    }
    let reply = match method {
      "initialize" => {
        let version = params["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION).to_string();
        ok(id, json!({
          "protocolVersion": version,
          "capabilities": { "tools": {} },
          "serverInfo": { "name": "nanobrowser-agent", "version": env!("CARGO_PKG_VERSION") },
        }))
      }
      "ping" => ok(id, json!({})),
      "tools/list" => ok(id, json!({ "tools": tools() })),
      "tools/call" => call(browser, id, &params),
      other => err(id, -32601, &format!("method not found: {}", other)),
    };
    send(&reply);
  }
}

fn send(v: &Value) {
  let mut out = std::io::stdout().lock();
  let _ = writeln!(out, "{}", v);
  let _ = out.flush();
}

fn ok(id: Value, result: Value) -> Value {
  json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: Value, code: i64, message: &str) -> Value {
  json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn text(s: &str) -> Value {
  json!({ "content": [ { "type": "text", "text": s } ] })
}

fn failed(s: &str) -> Value {
  json!({ "content": [ { "type": "text", "text": s } ], "isError": true })
}

/* ---------------- tool catalogue ---------------- */

fn tools() -> Value {
  let sel = json!({ "type": "string", "description": "CSS selector" });
  json!([
    {
      "name": "browser_navigate",
      "description": "Open a URL in the headless browser and wait for the page to load. Returns the final URL and title.",
      "inputSchema": {
        "type": "object",
        "properties": { "url": { "type": "string", "description": "Absolute URL to open" } },
        "required": ["url"]
      }
    },
    {
      "name": "browser_read",
      "description": "Read the current page as plain text or HTML, optionally restricted to one element.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "format": { "type": "string", "enum": ["text", "html"], "description": "Default: text" },
          "selector": sel
        }
      }
    },
    {
      "name": "browser_click",
      "description": "Click the element matching a CSS selector using a real trusted mouse event.",
      "inputSchema": {
        "type": "object",
        "properties": { "selector": { "type": "string", "description": "CSS selector of the element to click" } },
        "required": ["selector"]
      }
    },
    {
      "name": "browser_fill",
      "description": "Type a value into the input matching a CSS selector, replacing any existing text.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "selector": { "type": "string", "description": "CSS selector of the input" },
          "value": { "type": "string", "description": "Text to type" }
        },
        "required": ["selector", "value"]
      }
    },
    {
      "name": "browser_wait_for",
      "description": "Wait until an element matching the selector appears on the page.",
      "inputSchema": {
        "type": "object",
        "properties": {
          "selector": { "type": "string", "description": "CSS selector to wait for" },
          "timeout_ms": { "type": "number", "description": "Default: 20000" }
        },
        "required": ["selector"]
      }
    },
    {
      "name": "browser_screenshot",
      "description": "Capture the current page as a PNG image.",
      "inputSchema": { "type": "object", "properties": {} }
    },
    {
      "name": "browser_cookies",
      "description": "List cookies held by the automation profile for the current page (login state lives here).",
      "inputSchema": { "type": "object", "properties": {} }
    },
    {
      "name": "browser_traffic",
      "description": "Return recorded network requests and responses. Credentials and tokens are redacted.",
      "inputSchema": {
        "type": "object",
        "properties": { "limit": { "type": "number", "description": "Most recent N requests. Default: 100" } }
      }
    },
    {
      "name": "browser_eval",
      "description": "Evaluate a JavaScript expression in the page and return its value.",
      "inputSchema": {
        "type": "object",
        "properties": { "expression": { "type": "string", "description": "JavaScript expression" } },
        "required": ["expression"]
      }
    }
  ])
}

/* ---------------- dispatch ---------------- */

fn arg<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
  params["arguments"].get(key).and_then(|v| v.as_str())
}

fn call(browser: &Browser, id: Value, params: &Value) -> Value {
  let name = params["name"].as_str().unwrap_or_default();
  let result: Result<Value, String> = match name {
    "browser_navigate" => match arg(params, "url") {
      Some(u) => browser.navigate(u, DEFAULT_TIMEOUT).map(|v| text(&v.to_string())),
      None => Err("missing required argument: url".into()),
    },
    "browser_read" => {
      let format = arg(params, "format").unwrap_or("text");
      browser.read(format, arg(params, "selector")).map(|s| text(&s))
    }
    "browser_click" => match arg(params, "selector") {
      Some(s) => browser.click(s).map(|_| text(&json!({ "clicked": s }).to_string())),
      None => Err("missing required argument: selector".into()),
    },
    "browser_fill" => match (arg(params, "selector"), arg(params, "value")) {
      (Some(s), Some(v)) => {
        browser.fill(s, v).map(|_| text(&json!({ "filled": s }).to_string()))
      }
      _ => Err("missing required arguments: selector and value".into()),
    },
    "browser_wait_for" => match arg(params, "selector") {
      Some(s) => {
        let ms = params["arguments"]["timeout_ms"].as_u64().unwrap_or(20_000);
        browser
          .wait_for(s, std::time::Duration::from_millis(ms))
          .map(|v| text(&v.to_string()))
      }
      None => Err("missing required argument: selector".into()),
    },
    "browser_screenshot" => browser.screenshot_b64(DEFAULT_TIMEOUT).map(|data| {
      json!({ "content": [ { "type": "image", "data": data, "mimeType": "image/png" } ] })
    }),
    "browser_cookies" => browser.cookies().map(|v| text(&v.to_string())),
    "browser_traffic" => {
      let limit = params["arguments"]["limit"].as_u64().unwrap_or(100) as usize;
      Ok(text(&browser.traffic(limit).to_string()))
    }
    "browser_eval" => match arg(params, "expression") {
      Some(e) => browser.eval(e).map(|v| text(&v.to_string())),
      None => Err("missing required argument: expression".into()),
    },
    other => return err(id, -32602, &format!("unknown tool: {}", other)),
  };

  ok(
    id,
    match result {
      Ok(v) => v,
      Err(e) => failed(&format!("{} failed: {}", name, e)),
    },
  )
}
