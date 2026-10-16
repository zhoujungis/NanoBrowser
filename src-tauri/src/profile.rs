use std::{
  fs,
  path::PathBuf,
  time::{SystemTime, UNIX_EPOCH},
};

use crate::state::{Profile, Settings};

pub fn now_ms() -> u64 {
  SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Data directory: the `NANOBROWSER_DATA_DIR` / `NANO_DATA_DIR` override, else
/// derived from the executable (dev build `D:\NanoBrowser\src-tauri\target\debug\nanobrowser.exe`
/// -> `D:\NanoBrowser\data`). The rules live in `nanobrowser::paths`.
pub fn data_dir() -> PathBuf {
  nanobrowser::paths::app_data_dir()
}

pub fn profile_path() -> PathBuf {
  data_dir().join("profile.json")
}

/// WebView2 user data folder (cache, cookies, GPU shader cache). Kept inside the
/// project data dir so the browser never writes its profile to the system drive.
/// Every webview gets the same path so they share one WebView2 environment,
/// which is what makes cookies and logins carry across tabs.
pub fn webview_data_dir() -> PathBuf {
  nanobrowser::paths::webview_dir()
}

/// Built-in download directory: `<app dir>\Downloads` (i.e. D:\NanoBrowser\Downloads
/// for this project layout) so nothing lands on the system drive by default.
pub fn default_downloads_dir() -> PathBuf {
  nanobrowser::paths::downloads_dir()
}

pub fn resolve_downloads_dir(settings: &Settings) -> PathBuf {
  let custom = settings.downloads_dir.trim();
  if custom.is_empty() {
    default_downloads_dir()
  } else {
    PathBuf::from(custom)
  }
}

/// Parse a stored profile, capping what can grow without bound. Split out from
/// the file read so the fallbacks can be checked without touching the disk.
fn parse_profile(s: &str) -> Profile {
  match serde_json::from_str::<Profile>(s) {
    Ok(mut prof) => {
      prof.history.truncate(500);
      prof.downloads.truncate(100);
      prof
    }
    Err(_) => Profile::default(),
  }
}

pub fn load_profile() -> Profile {
  match fs::read_to_string(profile_path()) {
    Ok(s) => parse_profile(&s),
    Err(_) => Profile::default(),
  }
}

pub fn save_profile(p: &Profile) {
  let dir = data_dir();
  let _ = fs::create_dir_all(&dir);
  let path = profile_path();
  let tmp = dir.join(".profile.tmp");
  let mut prof = p.clone();
  prof.history.truncate(500);
  prof.downloads.truncate(100);
  if let Ok(json) = serde_json::to_string_pretty(&prof) {
    if fs::write(&tmp, json).is_ok() {
      let _ = fs::remove_file(&path);
      let _ = fs::rename(&tmp, &path);
    }
  }
}

pub fn hostname_of(url: &str) -> String {
  if let Ok(u) = tauri::Url::parse(url) {
    if let Some(h) = u.host_str() {
      return h.trim_start_matches("www.").to_string();
    }
  }
  url.to_string()
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::state::{DownloadItem, HistoryItem, Settings};

  #[test]
  fn a_missing_or_unparsable_profile_falls_back_to_defaults() {
    assert_eq!(parse_profile("").settings.search_engine, "bing");
    assert_eq!(parse_profile("not json at all").settings.devtools_side, "bottom");
    assert_eq!(parse_profile("{").bookmarks.len(), 0);
    // a profile written by an older build: unknown fields are ignored and the
    // missing ones take their defaults rather than failing the whole load
    let old = parse_profile(r#"{"settings":{"theme":"dark"},"extra":1}"#);
    assert_eq!(old.settings.theme, "dark");
    assert_eq!(old.settings.startup, "session");
    assert_eq!(old.settings.bookmarks_bar, true);
    assert!(old.session.tabs.is_empty());
  }

  #[test]
  fn unbounded_lists_are_capped_on_load() {
    let history: Vec<String> = (0..600)
      .map(|i| format!(r#"{{"url":"https://x/{i}","title":"t","time":{i}}}"#))
      .collect();
    let downloads: Vec<String> = (0..150)
      .map(|i| format!(r#"{{"url":"https://x/{i}","name":"f","path":"p","time":{i},"status":"done"}}"#))
      .collect();
    let json = format!(r#"{{"history":[{}],"downloads":[{}]}}"#, history.join(","), downloads.join(","));
    let p = parse_profile(&json);
    assert_eq!(p.history.len(), 500);
    assert_eq!(p.downloads.len(), 100);
    // both lists are stored newest-first (`insert(0, ..)`), so the cap drops the
    // oldest tail and leaves the newest entries — index 0 is the freshest
    assert_eq!(p.history.first().unwrap().time, 0);
    assert_eq!(p.history.last().unwrap().time, 499);
    assert_eq!(p.downloads.first().unwrap().time, 0);
    assert_eq!(p.downloads.last().unwrap().time, 99);
  }

  #[test]
  fn a_round_tripped_profile_keeps_the_docked_side() {
    let mut p = Profile::default();
    p.settings.devtools_side = "right".into();
    p.history.push(HistoryItem { url: "https://a".into(), title: "a".into(), time: 1, icon: "i.png".into() });
    p.downloads.push(DownloadItem { name: "f".into(), status: "done".into(), ..Default::default() });
    let json = serde_json::to_string(&p).unwrap();
    let back = parse_profile(&json);
    assert_eq!(back.settings.devtools_side, "right");
    assert_eq!(back.history[0].url, "https://a");
    assert_eq!(back.history[0].icon, "i.png");
    assert_eq!(back.downloads[0].name, "f");
  }

  #[test]
  fn a_profile_written_before_history_carried_icons_still_parses() {
    // the new-tab tiles read this field, so an older profile.json must keep
    // loading with the icon simply unknown
    let p = parse_profile(r#"{"history":[{"url":"https://a","title":"a","time":1}]}"#);
    assert_eq!(p.history.len(), 1);
    assert_eq!(p.history[0].icon, "");
  }

  #[test]
  fn hostnames_lose_their_www_prefix() {
    assert_eq!(hostname_of("https://www.example.com/a/b?c=1"), "example.com");
    assert_eq!(hostname_of("https://example.com"), "example.com");
    assert_eq!(hostname_of("http://localhost:8080/x"), "localhost");
    assert_eq!(hostname_of("https://sub.www.example.com/"), "sub.www.example.com");
    // not a URL at all: the raw text is what the tab strip shows
    assert_eq!(hostname_of("nano://newtab"), "newtab");
    assert_eq!(hostname_of("hello world"), "hello world");
  }

  #[test]
  fn the_built_in_download_directory_lives_beside_the_app() {
    // the built-in default ignores nothing but the settings; a custom path wins
    assert_eq!(
      resolve_downloads_dir(&Settings { downloads_dir: r"D:\dl".into(), ..Default::default() }),
      PathBuf::from(r"D:\dl")
    );
    assert_eq!(
      resolve_downloads_dir(&Settings { downloads_dir: "   ".into(), ..Default::default() }),
      default_downloads_dir()
    );
    assert_eq!(default_downloads_dir().file_name().unwrap(), "Downloads");
  }
}

