//! One-shot command line front end.
//!
//! Each invocation starts a fresh automation browser, performs one action and
//! exits — convenient for a human or a shell script. Long-lived, multi-step
//! sessions belong to the MCP mode, where the page stays warm between calls.

use std::path::Path;
use std::time::Duration;

use serde_json::json;

use crate::browser::{Browser, DEFAULT_TIMEOUT};

/// Run one command; returns the process exit code.
pub fn run(b: &Browser, cmd: &str, args: &[String]) -> i32 {
  match dispatch(b, cmd, args) {
    Ok(out) => {
      println!("{}", out);
      0
    }
    Err(e) => {
      eprintln!("nanoagent: {}", e);
      1
    }
  }
}

fn need<'a>(args: &'a [String], i: usize, name: &str) -> Result<&'a str, String> {
  args
    .get(i)
    .map(|s| s.as_str())
    .ok_or_else(|| format!("missing argument: {}", name))
}

/// Open a URL, run `f`, and print its JSON result.
fn on_url<F>(b: &Browser, url: &str, f: F) -> Result<String, String>
where
  F: FnOnce(&Browser) -> Result<serde_json::Value, String>,
{
  b.navigate(url, DEFAULT_TIMEOUT)?;
  f(b).map(|v| v.to_string())
}

fn dispatch(b: &Browser, cmd: &str, args: &[String]) -> Result<String, String> {
  match cmd {
    "navigate" => on_url(b, need(args, 0, "url")?, |b| {
      b.eval("({ url: location.href, title: document.title })")
    }),

    "read" => {
      let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
      let html = args.iter().any(|a| a == "--html");
      let url = need(args, 0, "url")?;
      let selector = positional.get(1).map(|s| s.as_str());
      let format = if html { "html" } else { "text" };
      b.navigate(url, DEFAULT_TIMEOUT)?;
      b.read(format, selector)
    }

    "shot" => {
      let url = need(args, 0, "url")?;
      let out = need(args, 1, "output path")?;
      b.navigate(url, DEFAULT_TIMEOUT)?;
      let png = b.screenshot(DEFAULT_TIMEOUT)?;
      let path = Path::new(out);
      if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
          std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {}", parent.display(), e))?;
        }
      }
      std::fs::write(path, &png).map_err(|e| format!("cannot write {}: {}", out, e))?;
      Ok(json!({ "path": out, "bytes": png.len() }).to_string())
    }

    "cookies" => {
      let url = need(args, 0, "url")?;
      on_url(b, url, |b| b.cookies())
    }

    "traffic" => {
      let url = need(args, 0, "url")?;
      let limit: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(100);
      on_url(b, url, |b| Ok(b.traffic(limit)))
    }

    "eval" => {
      let url = need(args, 0, "url")?;
      let expr = need(args, 1, "expression")?;
      on_url(b, url, |b| b.eval(expr))
    }

    "click" => {
      let url = need(args, 0, "url")?;
      let selector = need(args, 1, "selector")?;
      on_url(b, url, |b| b.click(selector).map(|_| json!({ "clicked": selector })))
    }

    "fill" => {
      let url = need(args, 0, "url")?;
      let selector = need(args, 1, "selector")?;
      let value = need(args, 2, "value")?;
      on_url(b, url, |b| b.fill(selector, value).map(|_| json!({ "filled": selector })))
    }

    "wait" => {
      let url = need(args, 0, "url")?;
      let selector = need(args, 1, "selector")?;
      let ms: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20_000);
      on_url(b, url, |b| b.wait_for(selector, Duration::from_millis(ms)))
    }

    other => Err(format!("unknown command: {} (try --help)", other)),
  }
}

pub const HELP: &str = "\
nanoagent - headless automation browser for AI tools

USAGE
  nanoagent --mcp                       speak MCP over stdio (long-lived session)
  nanoagent <command> [args...]         run one action and exit

COMMANDS
  navigate <url>                        open a URL, print {url,title}
  read <url> [selector] [--html]        print page text (or HTML) of an element/page
  shot <url> <out.png>                  save a PNG screenshot
  cookies <url>                         list the profile's cookies for that page
  traffic <url> [limit]                 print recorded requests (secrets redacted)
  eval <url> <js>                       evaluate JavaScript, print its value
  click <url> <selector>                click an element with a trusted mouse event
  fill <url> <selector> <value>         type into an input
  wait <url> <selector> [timeout_ms]    wait for an element to appear

  -h, --help / -V, --version

The automation profile (cookies, login state) lives in <project>\\data\\automation.";
