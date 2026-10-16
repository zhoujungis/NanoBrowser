use std::sync::{Mutex, MutexGuard};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::profile::{now_ms, save_profile};
use crate::state::{AppState, Bookmark, HistoryItem, Settings, TabView};
use crate::tabs::{
  self, activate_tab, close_tab, create_tab, find_clear, find_in_page, go_history, hard_reload_tab,
  internal_page_of, looks_like_url, navigate_tab, parse_input, reload_tab, set_zoom, stop_tab,
};

/// Lock the AppState and return a guard whose lifetime is tied to `app`
/// (State::inner() reborrows the managed value independent of the State temp).
fn st<'a>(app: &'a AppHandle<Wry>) -> MutexGuard<'a, AppState> {
  app.state::<Mutex<AppState>>().inner().lock().unwrap()
}

/// A user-supplied download directory must be absolute and off the system drive.
fn downloads_dir_ok(dir: &str) -> bool {
  if !std::path::Path::new(dir).is_absolute() {
    return false;
  }
  let sys = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
  !dir.to_lowercase().starts_with(&sys.to_lowercase())
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SuggestItem {
  pub kind: String, // url | search | history | bookmark
  pub text: String,
  pub url: String,
}

/* ---------------- state queries ---------------- */

#[tauri::command]
pub async fn get_initial_state(app: AppHandle<Wry>) -> serde_json::Value {
  let s = st(&app);
  serde_json::json!({
    "settings": s.profile.settings,
    "bookmarks": s.profile.bookmarks,
    "session": s.profile.session,
    "tabs": s.tabs.iter().map(TabView::from).collect::<Vec<_>>(),
    "active": s.active,
    "resolvedDark": s.resolved_dark(),
    "shellHeight": s.shell_h,
    "version": "0.1.0",
    "defaultDownloadsDir": crate::profile::resolve_downloads_dir(&s.profile.settings)
      .to_string_lossy(),
  })
}

#[tauri::command]
pub async fn get_tabs(app: AppHandle<Wry>) -> serde_json::Value {
  let s = st(&app);
  serde_json::json!({
    "tabs": s.tabs.iter().map(TabView::from).collect::<Vec<_>>(),
    "active": s.active
  })
}

#[tauri::command]
pub async fn get_settings(app: AppHandle<Wry>) -> Settings {
  st(&app).profile.settings.clone()
}

#[tauri::command]
pub async fn get_bookmarks(app: AppHandle<Wry>) -> Vec<Bookmark> {
  st(&app).profile.bookmarks.clone()
}

#[tauri::command]
pub async fn get_history(app: AppHandle<Wry>, query: Option<String>) -> Vec<crate::state::HistoryItem> {
  let s = st(&app);
  let q = query.unwrap_or_default().to_lowercase();
  if q.is_empty() {
    return s.profile.history.clone();
  }
  s.profile
    .history
    .iter()
    .filter(|h| h.title.to_lowercase().contains(&q) || h.url.to_lowercase().contains(&q))
    .cloned()
    .collect()
}

#[tauri::command]
pub async fn get_downloads(app: AppHandle<Wry>) -> Vec<crate::state::DownloadItem> {
  st(&app).profile.downloads.clone()
}

/// One tile on the new-tab page: a site (grouped by host) with the title and the
/// favicon of its newest visit.
#[derive(Debug, PartialEq)]
pub struct TopSite {
  pub url: String,
  pub title: String,
  pub icon: String,
  pub visits: u64,
}

/// History → new-tab tiles: one row per site (host), most-visited first, ties
/// going to the more recently visited site. The URL and the title come from that
/// site's newest visit; the favicon comes from the newest visit that recorded
/// one, which may be an older visit — a page only reports its icon once it has
/// loaded, so the newest entry is often still empty at that moment.
pub fn top_sites(history: &[HistoryItem], limit: usize) -> Vec<TopSite> {
  struct Row {
    url: String,
    title: String,
    icon: String,
    visits: u64,
    newest: usize,
  }
  let mut by_host: std::collections::HashMap<String, Row> = std::collections::HashMap::new();
  for (i, hi) in history.iter().enumerate() {
    let host = crate::profile::hostname_of(&hi.url);
    if host.is_empty() {
      continue;
    }
    // history is newest-first, so the first time a host shows up is its latest visit
    let row = by_host.entry(host).or_insert_with(|| Row {
      url: hi.url.clone(),
      title: hi.title.clone(),
      icon: String::new(),
      visits: 0,
      newest: i,
    });
    row.visits += 1;
    if row.icon.is_empty() && !hi.icon.is_empty() {
      row.icon = hi.icon.clone();
    }
  }
  let mut rows: Vec<Row> = by_host.into_values().collect();
  rows.sort_by(|a, b| b.visits.cmp(&a.visits).then(a.newest.cmp(&b.newest)));
  rows
    .into_iter()
    .take(limit)
    .map(|r| TopSite { url: r.url, title: r.title, icon: r.icon, visits: r.visits })
    .collect()
}

#[tauri::command]
pub async fn get_top_sites(app: AppHandle<Wry>, limit: Option<usize>) -> serde_json::Value {
  let s = st(&app);
  serde_json::Value::Array(
    top_sites(&s.profile.history, limit.unwrap_or(8))
      .into_iter()
      .map(|t| {
        serde_json::json!({ "url": t.url, "title": t.title, "icon": t.icon, "visits": t.visits })
      })
      .collect(),
  )
}

/* ---------------- navigation ---------------- */

#[tauri::command]
pub async fn navigate(app: AppHandle<Wry>, id: u32, input: String) {
  navigate_tab(&app, id, &input);
}

#[tauri::command]
pub async fn new_tab(app: AppHandle<Wry>, input: Option<String>, background: Option<bool>) -> u32 {
  let target = input.map(|i| {
    let s = st(&app);
    parse_input(&s, &i)
  });
  create_tab(&app, target, background.unwrap_or(false))
}

#[tauri::command]
pub async fn close_tab_cmd(app: AppHandle<Wry>, id: u32) {
  close_tab(&app, id, true);
}

#[tauri::command]
pub async fn activate(app: AppHandle<Wry>, id: u32) {
  activate_tab(&app, id);
}

#[tauri::command]
pub async fn go_back(app: AppHandle<Wry>, id: u32) {
  go_history(&app, id, true);
}

#[tauri::command]
pub async fn go_forward(app: AppHandle<Wry>, id: u32) {
  go_history(&app, id, false);
}

#[tauri::command]
pub async fn reload(app: AppHandle<Wry>, id: u32) {
  reload_tab(&app, id);
}

/// Shift-reload: skip the HTTP cache for this navigation.
#[tauri::command]
pub async fn hard_reload(app: AppHandle<Wry>, id: u32) {
  hard_reload_tab(&app, id, false);
}

/// Chrome's "empty cache and hard reload": drop the profile's disk cache first.
#[tauri::command]
pub async fn clear_cache_reload(app: AppHandle<Wry>, id: u32) {
  hard_reload_tab(&app, id, true);
}

#[tauri::command]
pub async fn stop(app: AppHandle<Wry>, id: u32) {
  stop_tab(&app, id);
}

#[tauri::command]
pub async fn zoom(app: AppHandle<Wry>, id: u32, delta: i32) -> Option<f64> {
  set_zoom(&app, id, delta)
}

/// Open/close the docked DevTools panel for a tab; the returned flag drives the menu check.
#[tauri::command]
pub async fn toggle_devtools(app: AppHandle<Wry>, id: u32) -> bool {
  tabs::toggle_devtools(&app, id)
}

/// The panel's grip is being dragged: `x`/`y` are the pointer's position in the
/// window, so the panel keeps everything past it on the axis it is docked to.
#[tauri::command]
pub async fn set_devtools_size(app: AppHandle<Wry>, x: f64, y: f64) {
  crate::devtools::drag_to(&app, x, y);
}

/// Which edge the panel is docked to: "bottom" or "right".
#[tauri::command]
pub async fn set_devtools_side(app: AppHandle<Wry>, side: String) {
  crate::devtools::set_side(&app, &side);
}

/// The grip drag ended: release the overlay that was catching the mouse moves.
#[tauri::command]
pub async fn devtools_drag_end(app: AppHandle<Wry>) {
  crate::devtools::drag_end(&app);
}

#[tauri::command]
pub async fn find_query(app: AppHandle<Wry>, id: u32, q: String) {
  find_in_page(&app, id, &q, None);
}

#[tauri::command]
pub async fn find_move(app: AppHandle<Wry>, id: u32, delta: i32) {
  find_in_page(&app, id, "", Some(delta));
}

#[tauri::command]
pub async fn find_close(app: AppHandle<Wry>, id: u32) {
  find_clear(&app, id);
}

/* ---------------- tab management ---------------- */

#[tauri::command]
pub async fn toggle_pin(app: AppHandle<Wry>, id: u32) {
  let mut s = st(&app);
  if let Some(t) = s.tab_mut(id) {
    t.pinned = !t.pinned;
  }
  tabs::emit_tabs_state(&app, &s);
  tabs::persist_session(&mut s);
}

#[tauri::command]
pub async fn move_tab(app: AppHandle<Wry>, id: u32, to: usize) {
  let mut s = st(&app);
  let Some(from) = s.tabs.iter().position(|t| t.id == id) else { return };
  if to >= s.tabs.len() || from == to {
    return;
  }
  let t = s.tabs.remove(from);
  let to = to.min(s.tabs.len());
  s.tabs.insert(to, t);
  tabs::emit_tabs_state(&app, &s);
  tabs::persist_session(&mut s);
}

#[tauri::command]
pub async fn duplicate_tab(app: AppHandle<Wry>, id: u32) {
  let url = st(&app).tab(id).map(|t| t.url.clone());
  if let Some(u) = url {
    create_tab(&app, Some(u), false);
  }
}

#[tauri::command]
pub async fn close_others(app: AppHandle<Wry>, id: u32) {
  let ids: Vec<u32> = {
    let s = st(&app);
    s.tabs.iter().filter(|t| t.id != id).map(|t| t.id).collect()
  };
  for tid in ids {
    close_tab(&app, tid, false);
  }
}

#[tauri::command]
pub async fn close_to_right(app: AppHandle<Wry>, id: u32) {
  let ids: Vec<u32> = {
    let s = st(&app);
    let Some(pos) = s.tabs.iter().position(|t| t.id == id) else { return };
    s.tabs[pos + 1..].iter().map(|t| t.id).collect()
  };
  for tid in ids {
    close_tab(&app, tid, false);
  }
}

#[tauri::command]
pub async fn reopen_tab_cmd(app: AppHandle<Wry>) {
  tabs::handle_shortcut(&app, "ctrl-shift-t");
}

/* ---------------- omnibox ---------------- */

#[tauri::command]
pub async fn omnibox_suggest(app: AppHandle<Wry>, input: String) -> Vec<SuggestItem> {
  let s = st(&app);
  let raw = input.trim().to_string();
  if raw.is_empty() {
    return vec![];
  }
  let mut out: Vec<SuggestItem> = vec![];
  let url_like = looks_like_url(&raw);
  if url_like.is_some() {
    out.push(SuggestItem {
      kind: "url".into(),
      text: raw.clone(),
      url: url_like.clone().unwrap_or_else(|| raw.clone()),
    });
  }
  out.push(SuggestItem {
    kind: "search".into(),
    text: raw.clone(),
    url: s.search_url(&raw),
  });
  let q = raw.to_lowercase();
  let mut seen: std::collections::HashSet<String> = Default::default();
  seen.insert(out.iter().map(|i| i.url.clone()).collect::<Vec<_>>().join("\u{1}"));
  for h in &s.profile.history {
    if q.is_empty() || h.title.to_lowercase().contains(&q) || h.url.to_lowercase().contains(&q) {
      if seen.insert(h.url.clone()) {
        out.push(SuggestItem { kind: "history".into(), text: h.title.clone(), url: h.url.clone() });
      }
    }
    if out.len() >= 8 {
      break;
    }
  }
  for b in &s.profile.bookmarks {
    if out.len() >= 8 {
      break;
    }
    if q.is_empty() || b.title.to_lowercase().contains(&q) || b.url.to_lowercase().contains(&q) {
      if seen.insert(b.url.clone()) {
        out.push(SuggestItem { kind: "bookmark".into(), text: b.title.clone(), url: b.url.clone() });
      }
    }
  }
  out
}

/* ---------------- bookmarks ---------------- */

#[tauri::command]
pub async fn add_bookmark(app: AppHandle<Wry>, url: String, title: String) {
  let mut s = st(&app);
  if url.is_empty() || s.profile.bookmarks.iter().any(|b| b.url == url) {
    return;
  }
  // store the favicon the page actually reported: guessing `<origin>/favicon.ico`
  // 404s on most modern sites, which is why the bar showed no icon at all
  let icon = s
    .tabs
    .iter()
    .find(|t| t.url == url && !t.icon.is_empty())
    .map(|t| t.icon.clone())
    .unwrap_or_else(|| tabs::favicon_for(&url));
  s.profile.bookmarks.insert(0, Bookmark { url, title, added: now_ms(), icon });
  save_profile(&s.profile);
  let _ = app.emit("bookmarks-changed", ());
  tabs::emit_omnibox(&app, &s);
}

#[tauri::command]
pub async fn remove_bookmark(app: AppHandle<Wry>, url: String) {
  let mut s = st(&app);
  s.profile.bookmarks.retain(|b| b.url != url);
  save_profile(&s.profile);
  let _ = app.emit("bookmarks-changed", ());
  tabs::emit_omnibox(&app, &s);
}

#[tauri::command]
pub async fn update_bookmark(app: AppHandle<Wry>, old_url: String, url: String, title: String) {
  let mut s = st(&app);
  if let Some(b) = s.profile.bookmarks.iter_mut().find(|b| b.url == old_url) {
    if b.url != url {
      b.icon.clear(); // the stored favicon belonged to the old site
    }
    b.url = url;
    b.title = title;
    save_profile(&s.profile);
    let _ = app.emit("bookmarks-changed", ());
  }
}

/* ---------------- history ---------------- */

#[tauri::command]
pub async fn delete_history(app: AppHandle<Wry>, url: String, time: u64) {
  let mut s = st(&app);
  s.profile.history.retain(|h| !(h.url == url && h.time == time));
  save_profile(&s.profile);
}

#[tauri::command]
pub async fn clear_history(app: AppHandle<Wry>) {
  let mut s = st(&app);
  s.profile.history.clear();
  save_profile(&s.profile);
}

/* ---------------- settings ---------------- */

#[tauri::command]
pub async fn set_settings(app: AppHandle<Wry>, settings: Settings) {
  let mut s = st(&app);
  let mut settings = settings;
  // a custom download directory has to be an absolute path on a non-system drive:
  // a relative value would resolve against the launch directory, and nothing of
  // this browser is supposed to land on the system drive. Invalid input keeps the
  // previous value instead of silently redirecting downloads.
  let dir = settings.downloads_dir.trim().to_string();
  if !dir.is_empty() && !downloads_dir_ok(&dir) {
    settings.downloads_dir = s.profile.settings.downloads_dir.clone();
  }
  let bar_changed = s.profile.settings.bookmarks_bar != settings.bookmarks_bar;
  s.profile.settings = settings;
  if bar_changed {
    s.shell_h = crate::state::STRIP_H + crate::state::TOOLBAR_H
      + if s.profile.settings.bookmarks_bar { crate::state::BM_H } else { 0.0 };
  }
  save_profile(&s.profile);
  let dark = s.resolved_dark();
  let _ = app.emit("theme-changed", serde_json::json!({ "dark": dark }));
  drop(s);
  if bar_changed {
    tabs::layout(&app);
  }
}

#[tauri::command]
pub async fn sys_theme_changed(app: AppHandle<Wry>, dark: bool) {
  let mut s = st(&app);
  s.sys_dark = dark;
  let resolved = s.resolved_dark();
  drop(s);
  let _ = app.emit("theme-changed", serde_json::json!({ "dark": resolved }));
}

/* ---------------- misc ---------------- */

/// Toggle the chrome overlay: the shell webview grows to full window height and
/// is raised above the page so menus / suggestion lists are fully visible.
#[tauri::command]
pub async fn chrome_overlay(app: AppHandle<Wry>, active: bool) {
  tabs::set_overlay(&app, active);
}

/// Reveal the download directory in Explorer, creating it if it does not exist.
#[tauri::command]
pub async fn open_downloads_folder(app: AppHandle<Wry>) {
  let dir = {
    let s = st(&app);
    crate::profile::resolve_downloads_dir(&s.profile.settings)
  };
  let _ = std::fs::create_dir_all(&dir);
  let _ = std::process::Command::new("explorer.exe").arg(dir).spawn();
}

#[tauri::command]
pub async fn internal_nav(app: AppHandle<Wry>, id: u32, input: String) {
  // used by internal pages (NTP etc.) to navigate their own tab
  if internal_page_of(&input).is_some() || !input.is_empty() {
    navigate_tab(&app, id, &input);
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// history entries are stored newest-first, so this helper reads naturally
  fn item(url: &str, title: &str, icon: &str) -> HistoryItem {
    HistoryItem { url: url.into(), title: title.into(), time: 0, icon: icon.into() }
  }

  #[test]
  fn tiles_group_by_site_and_rank_by_visits() {
    let h = vec![
      item("https://a.com/x", "A x", ""),
      item("https://b.com", "B", ""),
      item("https://a.com", "A", "https://a.com/icon.png"),
      item("https://a.com/y", "A y", ""),
    ];
    let rows = top_sites(&h, 8);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].url, "https://a.com/x"); // newest visit of the most-visited site
    assert_eq!(rows[0].title, "A x");
    assert_eq!(rows[0].icon, "https://a.com/icon.png"); // from the newest visit that had one
    assert_eq!(rows[0].visits, 3);
    assert_eq!(rows[1].url, "https://b.com");
    assert_eq!(rows[1].title, "B");
    assert_eq!(rows[1].icon, "");
  }

  #[test]
  fn the_tile_shows_the_title_of_its_newest_visit() {
    let h = vec![item("https://a.com/p2", "第二章", ""), item("https://a.com", "首页", "i.png")];
    let rows = top_sites(&h, 8);
    assert_eq!(rows[0].title, "第二章");
  }

  #[test]
  fn the_www_prefix_does_not_split_a_site_in_two() {
    let h = vec![item("https://www.a.com", "A", ""), item("https://a.com", "A", "")];
    assert_eq!(top_sites(&h, 8).len(), 1);
  }

  #[test]
  fn entries_without_a_url_are_dropped_and_the_limit_applies() {
    let h = vec![item("", "-", ""), item("https://a.com", "A", ""), item("https://b.com", "B", "")];
    let rows = top_sites(&h, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].url, "https://a.com"); // both have one visit, a.com is the newer one
  }
}
