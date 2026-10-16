//! nanoagent — the headless automation browser behind NanoBrowser.
//!
//! It runs the same WebView2 engine the browser does, in a window that is never
//! shown, against its own user data directory (`data\automation`). Driving it
//! goes through the DevTools Protocol *in process*: no debugging port is opened,
//! so nothing outside this process can attach to the automation browser.
//!
//! Two front ends share one page session:
//!   * `--mcp` — JSON-RPC tools for an AI client (the page stays warm)
//!   * one-shot commands — for scripts and humans

mod browser;
mod cli;
mod mcp;
mod paths;
mod traffic;

use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, Wry};

use nanobrowser::cdp::{Cdp, AGENT_LABEL};

use crate::browser::Browser;
use crate::traffic::Traffic;

enum Mode {
  /// MCP over stdio, until the client closes it.
  Mcp,
  /// A single command from the command line.
  Once(String, Vec<String>),
}

fn main() {
  let args: Vec<String> = std::env::args().skip(1).collect();
  let mode = match args.first().map(|s| s.as_str()) {
    None | Some("-h") | Some("--help") => {
      println!("{}", cli::HELP);
      return;
    }
    Some("-V") | Some("--version") => {
      println!("nanoagent {}", env!("CARGO_PKG_VERSION"));
      return;
    }
    Some("--mcp") | Some("mcp") => Mode::Mcp,
    Some(other) if other.starts_with('-') => {
      eprintln!("nanoagent: unknown option {}\n\n{}", other, cli::HELP);
      std::process::exit(2);
    }
    Some(cmd) => Mode::Once(cmd.to_string(), args[1..].to_vec()),
  };

  tauri::Builder::default()
    .setup(move |app| {
      // The automation browser is a normal WebView2 window that simply never
      // becomes visible: WebView2 keeps rendering and running scripts, so CDP
      // works exactly as it would for a shown window.
      let blank = tauri::Url::parse("about:blank").expect("about:blank is a valid url");
      tauri::WebviewWindowBuilder::new(app, AGENT_LABEL, tauri::WebviewUrl::External(blank))
        .title("NanoBrowser Agent")
        .inner_size(1280.0, 900.0)
        .decorations(false)
        .visible(false)
        .skip_taskbar(true)
        .data_directory(paths::automation_dir())
        .build()?;

      let handle = app.handle().clone();
      std::thread::spawn(move || worker(handle, mode));
      Ok(())
    })
    .run(tauri::generate_context!())
    .expect("nanoagent failed to start");
}

/// Owns the browser once the webview exists; never runs on the UI thread.
fn worker(app: AppHandle<Wry>, mode: Mode) {
  let cdp = Arc::new(Cdp::new(app.clone(), AGENT_LABEL));
  let traffic = Arc::new(Traffic::default());
  let browser = Browser::new(cdp, traffic);

  // The webview needs a moment (and a running event loop) before it answers CDP.
  let mut ready = false;
  for _ in 0..600 {
    if browser.is_ready() {
      ready = true;
      break;
    }
    std::thread::sleep(Duration::from_millis(50));
  }
  if !ready {
    eprintln!("nanoagent: the webview never became responsive");
    app.exit(2);
    return;
  }

  if let Err(e) = browser.init() {
    eprintln!("nanoagent: unable to initialise the CDP session: {}", e);
    app.exit(3);
    return;
  }

  let code = match mode {
    Mode::Mcp => {
      mcp::serve(&browser);
      0
    }
    Mode::Once(cmd, args) => cli::run(&browser, &cmd, &args),
  };
  app.exit(code);
}
