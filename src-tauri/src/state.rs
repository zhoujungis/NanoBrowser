use serde::{Deserialize, Serialize};

pub const STRIP_H: f64 = 40.0;
pub const TOOLBAR_H: f64 = 44.0;
pub const BM_H: f64 = 28.0;

pub const ZOOM_STEPS: [f64; 15] = [
  0.25, 0.33, 0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0,
];

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
  pub search_engine: String, // bing | baidu | google
  pub theme: String,         // system | light | dark
  pub homepage: String,      // url or nano://newtab
  pub bookmarks_bar: bool,
  pub startup: String, // session | ntp
  /// download directory; empty means the built-in default (<app dir>\Downloads)
  pub downloads_dir: String,
  /// which edge the docked DevTools panel takes: bottom | right
  pub devtools_side: String,
}

impl Default for Settings {
  fn default() -> Self {
    Self {
      search_engine: "bing".into(),
      theme: "system".into(),
      homepage: "nano://newtab".into(),
      bookmarks_bar: true,
      startup: "session".into(),
      downloads_dir: String::new(),
      devtools_side: "bottom".into(),
    }
  }
}

/// Last known window geometry, restored on the next launch.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct WindowState {
  pub x: i32, // outer position, physical pixels
  pub y: i32,
  pub w: u32, // inner size, physical pixels
  pub h: u32,
  pub maximized: bool,
}

impl Default for WindowState {
  fn default() -> Self {
    Self { x: 0, y: 0, w: 0, h: 0, maximized: false }
  }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Bookmark {
  pub url: String,
  pub title: String,
  pub added: u64,
  /// the favicon the page itself reported, captured when the bookmark was made.
  /// empty means unknown, and the UI falls back to a globe glyph
  pub icon: String,
}

impl Default for Bookmark {
  fn default() -> Self {
    Self { url: String::new(), title: String::new(), added: 0, icon: String::new() }
  }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct HistoryItem {
  pub url: String,
  pub title: String,
  pub time: u64, // unix millis
  /// the favicon the page itself reported, filled in once the site is visited
  /// (the new-tab tiles show it); empty means unknown
  pub icon: String,
}

impl Default for HistoryItem {
  fn default() -> Self {
    Self { url: String::new(), title: String::new(), time: 0, icon: String::new() }
  }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct DownloadItem {
  pub url: String,
  pub name: String,
  pub path: String,
  pub time: u64,
  pub status: String, // downloading | done | failed
}

impl Default for DownloadItem {
  fn default() -> Self {
    Self { url: String::new(), name: String::new(), path: String::new(), time: 0, status: "downloading".into() }
  }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default, rename_all = "camelCase")]
pub struct Session {
  pub tabs: Vec<String>,
  pub active: usize,
}

impl Default for Session {
  fn default() -> Self {
    Self { tabs: vec![], active: 0 }
  }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Profile {
  pub settings: Settings,
  pub bookmarks: Vec<Bookmark>,
  pub history: Vec<HistoryItem>,
  pub downloads: Vec<DownloadItem>,
  pub session: Session,
  pub window: WindowState,
}

#[derive(Clone, Debug)]
pub struct Tab {
  pub id: u32,
  pub label: String,
  pub url: String, // display URL, e.g. https://x.com or nano://newtab
  pub title: String,
  pub loading: bool,
  pub pinned: bool,
  pub icon: String,
  pub internal: bool,
  pub live: bool,
  pub zoom_idx: usize,
  pub can_back: bool,
  pub can_fwd: bool,
  pub expect_internal: bool,
  pub nav_pos: usize,  // approximate history position
  pub nav_total: usize,
  pub pending_nav: u8, // 0 none, 1 back, 2 forward
  /// secret baked into this tab's init script; page events must carry it, which
  /// is how the app knows which webview really sent them
  pub token: String,
}

impl Tab {
  pub fn zoom(&self) -> f64 {
    ZOOM_STEPS[self.zoom_idx.min(ZOOM_STEPS.len() - 1)]
  }
}

pub struct AppState {
  pub tabs: Vec<Tab>,
  pub active: Option<u32>,
  pub closed: Vec<(String, String)>, // (url, title) stack for Ctrl+Shift+T
  pub shell_h: f64,
  pub profile: Profile,
  pub next_id: u32,
  pub sys_dark: bool,
  /// true while a menu / suggestion dropdown is open: the shell webview is
  /// temporarily grown to the full window and raised above the page webviews.
  pub overlay: bool,
  /// the docked DevTools panel is showing beside the page
  pub devtools_open: bool,
  /// its height in logical pixels when docked to the bottom (the page keeps the
  /// rest of the content area)
  pub devtools_h: f64,
  /// its width in logical pixels when docked to the right
  pub devtools_w: f64,
  /// a debounced window-geometry save is already scheduled
  pub win_save_pending: bool,
}

impl AppState {
  pub fn new(profile: Profile, sys_dark: bool) -> Self {
    let shell_h = STRIP_H + TOOLBAR_H + if profile.settings.bookmarks_bar { BM_H } else { 0.0 };
    Self {
      tabs: vec![],
      active: None,
      closed: vec![],
      shell_h,
      profile,
      next_id: 1,
      sys_dark,
      overlay: false,
      devtools_open: false,
      devtools_h: 320.0,
      devtools_w: 480.0,
      win_save_pending: false,
    }
  }

  pub fn resolved_dark(&self) -> bool {
    match self.profile.settings.theme.as_str() {
      "dark" => true,
      "light" => false,
      _ => self.sys_dark,
    }
  }

  pub fn tab(&self, id: u32) -> Option<&Tab> {
    self.tabs.iter().find(|t| t.id == id)
  }

  pub fn tab_mut(&mut self, id: u32) -> Option<&mut Tab> {
    self.tabs.iter_mut().find(|t| t.id == id)
  }

  pub fn active_tab(&self) -> Option<&Tab> {
    self.active.and_then(|id| self.tab(id))
  }

  pub fn active_index(&self) -> Option<usize> {
    self.active.and_then(|id| self.tabs.iter().position(|t| t.id == id))
  }

  pub fn search_url(&self, q: &str) -> String {
    let enc = urlencode(q);
    match self.profile.settings.search_engine.as_str() {
      "baidu" => format!("https://www.baidu.com/s?wd={}", enc),
      "google" => format!("https://www.google.com/search?q={}", enc),
      _ => format!("https://www.bing.com/search?q={}", enc),
    }
  }
}

pub fn urlencode(s: &str) -> String {
  let mut out = String::new();
  for b in s.as_bytes() {
    match b {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
      b' ' => out.push('+'),
      _ => out.push_str(&format!("%{:02X}", b)),
    }
  }
  out
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TabView {
  pub id: u32,
  pub title: String,
  pub url: String,
  pub icon: String,
  pub loading: bool,
  pub pinned: bool,
  pub zoom: f64,
}

impl From<&Tab> for TabView {
  fn from(t: &Tab) -> Self {
    Self {
      id: t.id,
      title: t.title.clone(),
      url: t.url.clone(),
      icon: t.icon.clone(),
      loading: t.loading,
      pinned: t.pinned,
      zoom: t.zoom(),
    }
  }
}

/// A tab with nothing set but its identity, for tests.
#[cfg(test)]
pub(crate) fn sample_tab(id: u32, url: &str) -> Tab {
  Tab {
    id,
    label: format!("tab-{}", id),
    url: url.to_string(),
    title: url.to_string(),
    loading: false,
    pinned: false,
    icon: String::new(),
    internal: false,
    live: true,
    zoom_idx: 7,
    can_back: false,
    can_fwd: false,
    expect_internal: false,
    nav_pos: 0,
    nav_total: 0,
    pending_nav: 0,
    token: format!("tok{}", id),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_shell_height_follows_the_bookmarks_bar() {
    assert_eq!(AppState::new(Profile::default(), false).shell_h, STRIP_H + TOOLBAR_H + BM_H);
    let mut p = Profile::default();
    p.settings.bookmarks_bar = false;
    assert_eq!(AppState::new(p, false).shell_h, STRIP_H + TOOLBAR_H);
  }

  #[test]
  fn a_docked_panel_starts_closed_at_its_default_size() {
    let s = AppState::new(Profile::default(), false);
    assert!(!s.devtools_open);
    assert_eq!((s.devtools_h, s.devtools_w), (320.0, 480.0));
  }

  #[test]
  fn the_theme_setting_overrides_the_system_one() {
    let dark = |theme: &str, sys: bool| {
      let mut p = Profile::default();
      p.settings.theme = theme.into();
      AppState::new(p, sys).resolved_dark()
    };
    assert_eq!(dark("dark", false), true);
    assert_eq!(dark("light", true), false);
    assert_eq!(dark("system", true), true);
    assert_eq!(dark("system", false), false);
    // an unknown value behaves like "system" rather than leaving the UI unstyled
    assert_eq!(dark("sepia", true), true);
  }

  #[test]
  fn search_urls_use_the_configured_engine() {
    let with = |engine: &str| {
      let mut p = Profile::default();
      p.settings.search_engine = engine.into();
      AppState::new(p, false)
    };
    assert_eq!(with("bing").search_url("hello world"), "https://www.bing.com/search?q=hello+world");
    assert_eq!(with("baidu").search_url("你好"), "https://www.baidu.com/s?wd=%E4%BD%A0%E5%A5%BD");
    assert_eq!(with("google").search_url("a&b"), "https://www.google.com/search?q=a%26b");
    // default engine, and any unknown value, both land on bing
    assert_eq!(with("").search_url("x"), "https://www.bing.com/search?q=x");
    assert_eq!(with("duckduckgo").search_url("x"), "https://www.bing.com/search?q=x");
  }

  #[test]
  fn urlencode_escapes_everything_outside_the_unreserved_set() {
    assert_eq!(urlencode(""), "");
    assert_eq!(urlencode("abcXYZ019"), "abcXYZ019");
    assert_eq!(urlencode("-_.~"), "-_.~");
    assert_eq!(urlencode("a b"), "a+b");
    assert_eq!(urlencode("/?#&=:"), "%2F%3F%23%26%3D%3A");
    assert_eq!(urlencode("中文"), "%E4%B8%AD%E6%96%87");
  }

  #[test]
  fn the_zoom_ladder_is_ordered_and_clamped_at_both_ends() {
    assert!(ZOOM_STEPS.windows(2).all(|w| w[0] < w[1]));
    let mut t = sample_tab(1, "https://x");
    assert_eq!(t.zoom(), 1.0, "0.9/1.1 straddle 1.0 and index 7 is 100%");
    t.zoom_idx = 0;
    assert_eq!(t.zoom(), 0.25);
    t.zoom_idx = ZOOM_STEPS.len() - 1;
    assert_eq!(t.zoom(), 3.0);
    // a stale index from an older profile cannot panic
    t.zoom_idx = 999;
    assert_eq!(t.zoom(), 3.0);
  }

  #[test]
  fn tab_lookups_ignore_unknown_ids_and_an_empty_selection() {
    let mut s = AppState::new(Profile::default(), false);
    s.tabs.push(sample_tab(4, "https://a"));
    s.tabs.push(sample_tab(9, "https://b"));
    assert_eq!(s.tab(9).unwrap().url, "https://b");
    assert!(s.tab(1).is_none());
    assert!(s.active_tab().is_none(), "nothing is selected yet");
    assert!(s.active_index().is_none());
    s.active = Some(9);
    assert_eq!(s.active_tab().unwrap().id, 9);
    assert_eq!(s.active_index(), Some(1));
    // an id that has since been closed resolves to nothing, not to a neighbour
    s.active = Some(7);
    assert!(s.active_tab().is_none());
    assert!(s.active_index().is_none());
    assert_eq!(s.tab_mut(4).unwrap().url, "https://a");
  }

  #[test]
  fn the_tab_view_carries_the_current_zoom() {
    let t = sample_tab(3, "https://x");
    let v = TabView::from(&t);
    assert_eq!((v.id, v.url.as_str(), v.zoom), (3, "https://x", 1.0));
    assert!(!v.loading && !v.pinned);
  }
}

