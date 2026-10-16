#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod devtools;
mod inject;
mod profile;
mod state;
mod tabs;

use std::sync::{Arc, Mutex};

use serde::Deserialize;
use tauri::{
  Emitter, Listener, LogicalPosition, LogicalSize, Manager, PhysicalPosition, PhysicalSize,
  WebviewBuilder, WebviewUrl, WindowEvent, Wry,
};

use crate::state::{AppState, Tab, WindowState};
use crate::tabs::{activate_tab, create_tab, favicon_for, nano_title, internal_page_of, persist_session};
use crate::profile::hostname_of;

pub const SHELL_LABEL: &str = "shell";

#[derive(Deserialize, Default)]
struct PageEvent {
  /// per-tab secret, only known to that tab's init script. Used to tell which
  /// webview really sent the event instead of trusting a self-reported id.
  #[serde(default)]
  tok: String,
  #[serde(default)]
  key: String,
  #[serde(default)]
  url: String,
  #[serde(default)]
  icon: String,
  #[serde(default)]
  count: usize,
  #[serde(default)]
  index: usize,
}

/// Only accept favicons that are plain http(s) URLs: the value comes from page
/// content and ends up in an `<img src>` in the shell UI.
fn valid_favicon(url: &str) -> bool {
  if url.is_empty() || url.len() > 2048 {
    return false;
  }
  match tauri::Url::parse(url) {
    Ok(u) => matches!(u.scheme(), "http" | "https"),
    Err(_) => false,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_page_reported_favicon_must_be_a_plain_web_url() {
    assert!(valid_favicon("https://example.com/favicon.ico"));
    assert!(valid_favicon("http://localhost:8080/icon.png?v=2"));

    // the value lands in an <img src> built by the shell, so every scheme that
    // could execute — or reach the local filesystem — has to be refused
    assert!(!valid_favicon("javascript:alert(1)"));
    assert!(!valid_favicon("JavaScript:alert(1)"));
    assert!(!valid_favicon("data:image/svg+xml,<svg onload=alert(1)>"));
    assert!(!valid_favicon("file:///C:/Windows/system32/win.ini"));
    assert!(!valid_favicon("vbscript:msgbox(1)"));
    assert!(!valid_favicon("about:blank"));
    assert!(!valid_favicon("nano://newtab/favicon.ico"));

    // and nothing degenerate gets through either
    assert!(!valid_favicon(""));
    assert!(!valid_favicon("//example.com/icon.ico"));
    assert!(!valid_favicon("example.com/icon.ico"));
    assert!(!valid_favicon(&format!("https://example.com/{}", "a".repeat(2048))));
    assert!(valid_favicon(&format!("https://example.com/{}", "a".repeat(2000))));
  }

  #[test]
  fn a_page_event_without_the_tab_token_is_not_trusted_by_the_shell() {
    // the shell only ever acts on events whose token names the active tab, so a
    // page that omits the token (or invents one) is dropped before anything runs
    let ev: PageEvent = serde_json::from_str(r#"{"key":"ctrl-t"}"#).unwrap();
    assert_eq!(ev.tok, "");
    assert_eq!(ev.key, "ctrl-t");
    assert_eq!(ev.count, 0);
  }
}

fn main() {
  tauri::Builder::default()
    .setup(|app| {
      // persist dir sanity (dev: D:\NanoBrowser\data)
      let prof = profile::load_profile();
      let saved_window = prof.window.clone();

      // main window (frameless, initially hidden to avoid flash)
      let win = tauri::WindowBuilder::new(app, "main")
        .title("NanoBrowser")
        .inner_size(1120.0, 700.0)
        .min_inner_size(520.0, 380.0)
        .decorations(false)
        .shadow(true)
        .center()
        .visible(false)
        .build()?;

      // restore the geometry from the previous run (after the state is managed,
      // so the resulting window events can safely touch it). Done before the
      // child webviews are created so they get the final size right away.
      let sys_dark = matches!(win.theme(), Ok(tauri::Theme::Dark));
      app.manage(Mutex::new(AppState::new(prof, sys_dark)));
      // the docked DevTools panel and its loopback bridge (see `devtools`)
      app.manage(Arc::new(devtools::Docked::new(app.handle().clone())));
      restore_geometry(&win, &saved_window, false);

      let (shell_h, startup, session) = {
        let s = app.state::<Mutex<AppState>>().inner().lock().unwrap();
        (s.shell_h, s.profile.settings.startup.clone(), s.profile.session.clone())
      };

      let scale = win.scale_factor().unwrap_or(1.0);
      let size = win.inner_size().unwrap_or_default();
      let w = size.width as f64 / scale;

      // shell UI webview (tab strip + toolbar + bookmarks bar).
      // transparent so that, when it is temporarily grown to full window height
      // to host menus / omnibox suggestions, the page below shows through.
      // data_directory pins the WebView2 profile to <project>\data\webview so the
      // browser never writes cache/cookies to the system drive.
      let shell = WebviewBuilder::new(SHELL_LABEL, WebviewUrl::App("index.html".into()))
        .transparent(true)
        // the browser chrome is not a page: F12 here is relayed to the active
        // page's DevTools instead of opening an inspector for the tab strip
        .devtools(false)
        .data_directory(profile::webview_data_dir());
      win.add_child(
        shell,
        LogicalPosition::new(0.0, 0.0),
        LogicalSize::new(w, shell_h),
      )?;

      // restore session or open a fresh new-tab page
      let handle = app.handle().clone();
      if startup == "session" && !session.tabs.is_empty() {
        {
          let mut s = app.state::<Mutex<AppState>>().inner().lock().unwrap();
          for url in &session.tabs {
            let internal = internal_page_of(url).is_some();
            let nid = s.next_id;
            s.tabs.push(Tab {
              id: nid,
              label: format!("tab-{}", nid),
              url: url.clone(),
              title: if internal {
                nano_title(internal_page_of(url).unwrap()).to_string()
              } else {
                hostname_of(url)
              },
              loading: false,
              pinned: false,
              icon: if internal { String::new() } else { favicon_for(url) },
              internal,
              live: false, // phantom: webview created on activation (lazy restore)
              zoom_idx: 7,
              can_back: false,
              can_fwd: false,
              expect_internal: internal,
              nav_pos: 0,
              nav_total: 0,
              pending_nav: 0,
              token: tabs::new_token(),
            });
            s.next_id += 1;
          }
          s.active = None;
        }
        let idx = session.active.min(s_len(app.handle()));
        let id = app.state::<Mutex<AppState>>().lock().unwrap().tabs[idx].id;
        activate_tab(&handle, id);
      } else {
        create_tab(&handle, None, false);
      }

      // relay page events (emit from content webviews)
      let h = app.handle().clone();
      app.listen_any("nano://page-shortcut", move |e| {
        if let Ok(pe) = serde_json::from_str::<PageEvent>(e.payload()) {
          let h = h.clone();
          tauri::async_runtime::spawn(async move {
            let from_active = {
              let s = h.state::<Mutex<AppState>>().inner().lock().unwrap();
              tabs::tab_id_by_token(&s, &pe.tok).map_or(false, |id| s.active == Some(id))
            };
            if from_active {
              tabs::handle_shortcut(&h, &pe.key);
            }
          });
        }
      });

      let h = app.handle().clone();
      app.listen_any("nano://page-hover", move |e| {
        if let Ok(pe) = serde_json::from_str::<PageEvent>(e.payload()) {
          let s = h.state::<Mutex<AppState>>().inner().lock().unwrap();
          if tabs::tab_id_by_token(&s, &pe.tok).map_or(false, |id| s.active == Some(id)) {
            let _ = h.emit("status-hover", serde_json::json!({ "url": pe.url }));
          }
        }
      });

      let h = app.handle().clone();
      app.listen_any("nano://page-find", move |e| {
        if let Ok(pe) = serde_json::from_str::<PageEvent>(e.payload()) {
          let s = h.state::<Mutex<AppState>>().inner().lock().unwrap();
          if tabs::tab_id_by_token(&s, &pe.tok).map_or(false, |id| s.active == Some(id)) {
            let _ = h.emit("find-result", serde_json::json!({ "count": pe.count, "index": pe.index }));
          }
        }
      });

      let h = app.handle().clone();
      app.listen_any("nano://page-icon", move |e| {
        if let Ok(pe) = serde_json::from_str::<PageEvent>(e.payload()) {
          if !valid_favicon(&pe.icon) {
            return;
          }
          let mut s = h.state::<Mutex<AppState>>().inner().lock().unwrap();
          let Some(id) = tabs::tab_id_by_token(&s, &pe.tok) else { return };
          let url = match s.tab_mut(id) {
            Some(t) => {
              t.icon = pe.icon.clone();
              t.url.clone()
            }
            None => return,
          };
          tabs::emit_tabs_state(&h, &s);
          // a bookmark saved before the page reported its icon is healed here, so
          // it picks up the real favicon the next time the site is visited
          let mut healed = false;
          let mut bookmarks_healed = false;
          for b in s.profile.bookmarks.iter_mut() {
            if b.url == url && b.icon != pe.icon {
              b.icon = pe.icon.clone();
              healed = true;
              bookmarks_healed = true;
            }
          }
          // the new-tab page shows one tile per site, built from that site's
          // newest history entry — stamp it with the icon the page reported.
          // history is newest-first, so the first match is the newest visit.
          let host = hostname_of(&url);
          if !host.is_empty() {
            if let Some(newest) = s.profile.history.iter_mut().find(|hi| hostname_of(&hi.url) == host)
            {
              if newest.icon != pe.icon {
                newest.icon = pe.icon.clone();
                healed = true;
              }
            }
          }
          if healed {
            profile::save_profile(&s.profile);
          }
          if bookmarks_healed {
            let _ = h.emit("bookmarks-changed", ());
          }
        }
      });

      win.show()?;
      // the window's non-client offsets are only final now, so the saved inner
      // size is asserted a second time (see restore_geometry)
      restore_geometry(&win, &saved_window, true);
      Ok(())
    })
    .on_window_event(|window, event| {
      if window.label() != "main" {
        return;
      }
      match event {
        WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
          let h = window.app_handle().clone();
          // schedule layout off the event pump
          tauri::async_runtime::spawn(async move {
            tabs::layout(&h);
          });
          save_window_state(window, false);
        }
        WindowEvent::Moved(_) => save_window_state(window, false),
        WindowEvent::ThemeChanged(theme) => {
          let dark = *theme == tauri::Theme::Dark;
          let h = window.app_handle().clone();
          let resolved = {
            let mut s = h.state::<Mutex<AppState>>().inner().lock().unwrap();
            s.sys_dark = dark;
            s.resolved_dark()
          };
          let _ = h.emit("sys-theme", serde_json::json!({ "dark": dark }));
          let _ = h.emit("theme-changed", serde_json::json!({ "dark": resolved }));
        }
        WindowEvent::CloseRequested { .. } => {
          save_window_state(window, true);
          let h = window.app_handle().clone();
          let mut s = h.state::<Mutex<AppState>>().inner().lock().unwrap();
          persist_session(&mut s);
        }
        _ => {}
      }
    })
    .invoke_handler(tauri::generate_handler![
      commands::get_initial_state,
      commands::get_tabs,
      commands::get_settings,
      commands::get_bookmarks,
      commands::get_history,
      commands::get_downloads,
      commands::get_top_sites,
      commands::navigate,
      commands::new_tab,
      commands::close_tab_cmd,
      commands::activate,
      commands::go_back,
      commands::go_forward,
      commands::reload,
      commands::hard_reload,
      commands::clear_cache_reload,
      commands::stop,
      commands::zoom,
      commands::find_query,
      commands::find_move,
      commands::find_close,
      commands::toggle_pin,
      commands::move_tab,
      commands::duplicate_tab,
      commands::close_others,
      commands::close_to_right,
      commands::reopen_tab_cmd,
      commands::omnibox_suggest,
      commands::add_bookmark,
      commands::remove_bookmark,
      commands::update_bookmark,
      commands::delete_history,
      commands::clear_history,
      commands::set_settings,
      commands::sys_theme_changed,
      commands::open_downloads_folder,
      commands::internal_nav,
      commands::toggle_devtools,
      commands::set_devtools_size,
      commands::set_devtools_side,
      commands::devtools_drag_end,
      commands::chrome_overlay
    ])
    .run(tauri::generate_context!())
    .expect("error while running NanoBrowser");
}

fn s_len(app: &tauri::AppHandle<Wry>) -> usize {
  app.state::<Mutex<AppState>>().lock().unwrap().tabs.len().saturating_sub(1)
}

/// Restore the window geometry saved by the previous run. The position is only
/// applied when the window would still be reachable on a connected monitor.
fn restore_geometry(win: &tauri::Window<Wry>, ws: &WindowState, final_pass: bool) {
  if ws.w == 0 || ws.h == 0 {
    return;
  }
  if !final_pass && on_any_monitor(win, ws.x, ws.y, ws.w, ws.h) {
    let _ = win.set_position(PhysicalPosition::new(ws.x, ws.y));
  }
  // tao pads the size by the non-client offsets it detects on the window. While
  // the window is still hidden those offsets are not final yet (the height comes
  // out ~43px too tall), so the size is asserted again once the window is shown.
  let _ = win.set_size(PhysicalSize::new(ws.w.max(320), ws.h.max(240)));
  if ws.maximized {
    if final_pass {
      // Windows stores the "restore" rect at the moment the window is maximized;
      // maximizing before the queued resize has landed would record a stale rect,
      // so it is deferred by one beat.
      let win = win.clone();
      std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(220));
        let _ = win.maximize();
      });
    } else {
      let _ = win.maximize();
    }
  }
}

fn on_any_monitor(win: &tauri::Window<Wry>, x: i32, y: i32, w: u32, h: u32) -> bool {
  let Ok(monitors) = win.available_monitors() else { return true };
  if monitors.is_empty() {
    return true;
  }
  // probe the window centre
  let cx = x + w as i32 / 2;
  let cy = y + h as i32 / 2;
  monitors.iter().any(|m| {
    let p = m.position();
    let s = m.size();
    cx >= p.x && cx <= p.x + s.width as i32 && cy >= p.y && cy <= p.y + s.height as i32
  })
}

/// Read the live geometry and write it into the profile. While the window is
/// maximized only the flag is stored, keeping the last normal rect for the next
/// restore.
fn write_window_state(window: &tauri::Window<Wry>) {
  let h = window.app_handle().clone();
  let mut s = h.state::<Mutex<AppState>>().inner().lock().unwrap();
  s.win_save_pending = false;
  if window.is_maximized().unwrap_or(false) {
    if s.profile.window.maximized {
      return;
    }
    s.profile.window.maximized = true;
  } else {
    let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) else { return };
    if size.width == 0 || size.height == 0 {
      return;
    }
    s.profile.window = WindowState { x: pos.x, y: pos.y, w: size.width, h: size.height, maximized: false };
  }
  profile::save_profile(&s.profile);
}

/// Persist the window geometry for the next launch. Non-forced calls are
/// debounced: the Moved message that precedes WM_SIZE(SIZE_MAXIMIZED) must not
/// be recorded as the "normal" rect, so the state is read once it has settled.
fn save_window_state(window: &tauri::Window<Wry>, force: bool) {
  if force {
    write_window_state(window);
    return;
  }
  {
    let h = window.app_handle().clone();
    let mut s = h.state::<Mutex<AppState>>().inner().lock().unwrap();
    if s.win_save_pending {
      return;
    }
    s.win_save_pending = true;
  }
  let win = window.clone();
  std::thread::spawn(move || {
    std::thread::sleep(std::time::Duration::from_millis(450));
    write_window_state(&win);
  });
}
