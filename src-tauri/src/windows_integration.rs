//! Windows shell integration: register this copy of NanoBrowser as a browser
//! under HKCU, and turn command-line arguments into navigable targets.
//!
//! Everything is per-user, so a portable copy can register itself without an
//! installer or administrator rights. The executable path is read from
//! `current_exe` every time, which keeps the registry valid when the app is
//! moved (D:\NanoBrowser, C:\Program Files\NanoBrowser, a removable drive, ...).

use std::path::Path;
use std::sync::Mutex;

use tauri::{AppHandle, Manager, Wry};

pub const APP_NAME: &str = "NanoBrowser";
pub const PROGID_HTML: &str = "NanoBrowserHTML";
pub const PROGID_URL: &str = "NanoBrowserURL";

const CAPABILITIES_KEY: &str = r"Software\NanoBrowser\Capabilities";

/// A second instance can be launched while the first is still in `setup`.
/// Its URLs wait here until the window and session restore are finished.
#[derive(Default)]
struct LaunchQueue {
  ready: bool,
  pending: Vec<String>,
}

impl LaunchQueue {
  fn accept(&mut self, targets: Vec<String>) -> Option<Vec<String>> {
    if self.ready {
      return Some(targets);
    }
    for t in targets {
      if !self.pending.contains(&t) {
        self.pending.push(t);
      }
    }
    None
  }

  fn mark_ready(&mut self) -> Vec<String> {
    self.ready = true;
    std::mem::take(&mut self.pending)
  }
}

static LAUNCH_QUEUE: Mutex<LaunchQueue> = Mutex::new(LaunchQueue { ready: false, pending: Vec::new() });

/// Called from the single-instance callback. Returns the targets when the app
/// is ready to open them immediately, or queues them for setup to drain.
pub fn accept_launch_targets(targets: Vec<String>) -> Option<Vec<String>> {
  LAUNCH_QUEUE.lock().unwrap().accept(targets)
}

/// Finish setup: from now on forwarded targets open immediately. Whatever
/// arrived during setup is returned for the caller to open before showing the
/// window.
pub fn mark_ready_and_take_pending() -> Vec<String> {
  LAUNCH_QUEUE.lock().unwrap().mark_ready()
}

/// One registry value NanoBrowser writes. Kept as data so the registration
/// layout can be tested without touching the live registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegWrite {
  pub key: &'static str,
  pub name: &'static str,
  pub value: String,
}

fn reg(key: &'static str, name: &'static str, value: impl Into<String>) -> RegWrite {
  RegWrite { key, name, value: value.into() }
}

/// The complete HKCU registration: ProgIDs, file/URL associations, the
/// RegisteredApplications entry, and the Capabilities block Windows 10/11 reads
/// when it lists NanoBrowser under Settings -> Apps -> Default apps.
pub fn registration_writes(exe: &Path) -> Vec<RegWrite> {
  let exe = exe.to_string_lossy();
  let command = format!("\"{}\" \"%1\"", exe);
  let icon = format!("\"{}\",0", exe);

  vec![
    // HTML documents: the ProgID and the command that opens one.
    reg(r"Software\Classes\NanoBrowserHTML", "", "NanoBrowser HTML Document"),
    reg(r"Software\Classes\NanoBrowserHTML\DefaultIcon", "", icon.clone()),
    reg(r"Software\Classes\NanoBrowserHTML\shell\open\command", "", command.clone()),

    // URL protocol: Windows hands the URL to `%1`.
    reg(r"Software\Classes\NanoBrowserURL", "", "NanoBrowser URL"),
    reg(r"Software\Classes\NanoBrowserURL", "URL Protocol", ""),
    reg(r"Software\Classes\NanoBrowserURL\DefaultIcon", "", icon.clone()),
    reg(r"Software\Classes\NanoBrowserURL\shell\open\command", "", command),

    // Direct associations, kept for older Windows versions and for apps that
    // still resolve handlers through Classes.
    reg(r"Software\Classes\.html", "", PROGID_HTML),
    reg(r"Software\Classes\.htm", "", PROGID_HTML),
    reg(r"Software\Classes\.xhtml", "", PROGID_HTML),
    reg(r"Software\Classes\http", "", PROGID_URL),
    reg(r"Software\Classes\http", "URL Protocol", ""),
    reg(r"Software\Classes\https", "", PROGID_URL),
    reg(r"Software\Classes\https", "URL Protocol", ""),

    // The modern Default Apps surface.
    reg(r"Software\RegisteredApplications", APP_NAME, CAPABILITIES_KEY),
    reg(CAPABILITIES_KEY, "ApplicationName", APP_NAME),
    reg(
      CAPABILITIES_KEY,
      "ApplicationDescription",
      "NanoBrowser - an ultra-light Chrome-style browser",
    ),
    reg(CAPABILITIES_KEY, "ApplicationIcon", icon),
    reg(r"Software\NanoBrowser\Capabilities\FileAssociations", ".html", PROGID_HTML),
    reg(r"Software\NanoBrowser\Capabilities\FileAssociations", ".htm", PROGID_HTML),
    reg(r"Software\NanoBrowser\Capabilities\FileAssociations", ".xhtml", PROGID_HTML),
    reg(r"Software\NanoBrowser\Capabilities\URLAssociations", "http", PROGID_URL),
    reg(r"Software\NanoBrowser\Capabilities\URLAssociations", "https", PROGID_URL),
  ]
}

/// Register this executable as a browser for the current user. Idempotent:
/// every launch rewrites the same values, so moving the app and starting it
/// again repairs the command paths.
pub fn register_default_browser() -> Result<(), String> {
  let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
  apply(&registration_writes(&exe))
}

#[cfg(target_os = "windows")]
fn apply(writes: &[RegWrite]) -> Result<(), String> {
  use winreg::enums::HKEY_CURRENT_USER;
  use winreg::RegKey;

  let hkcu = RegKey::predef(HKEY_CURRENT_USER);
  for w in writes {
    let (key, _) = hkcu
      .create_subkey(w.key)
      .map_err(|e| format!("create HKCU\\{}: {e}", w.key))?;
    key
      .set_value(w.name, &w.value)
      .map_err(|e| format!("write HKCU\\{}\\{}: {e}", w.key, w.name))?;
  }
  Ok(())
}

#[cfg(not(target_os = "windows"))]
fn apply(_writes: &[RegWrite]) -> Result<(), String> {
  Err("default-browser registration is Windows-only".into())
}

/// Bring the existing window back to the foreground when a second launch
/// forwards a URL to this instance.
pub fn focus_main_window(app: &AppHandle<Wry>) {
  if let Some(w) = app.get_window("main") {
    let _ = w.unminimize();
    let _ = w.show();
    let _ = w.set_focus();
  }
}

/// Open the Windows Default Apps page, where the user picks NanoBrowser for
/// http/https and HTML files. Windows 10/11 does not let an app silently take
/// the user's default, so this is the honest follow-up to registration.
pub fn open_default_apps_settings() -> Result<(), String> {
  #[cfg(target_os = "windows")]
  {
    // explorer is a GUI process (no console flash) and already understands
    // ms-settings: URIs.
    std::process::Command::new("explorer.exe")
      .arg("ms-settings:defaultapps")
      .spawn()
      .map(|_| ())
      .map_err(|e| format!("open default apps settings: {e}"))
  }
  #[cfg(not(target_os = "windows"))]
  {
    Err("default apps settings are Windows-only".into())
  }
}

/// Command-line arguments that should open a tab. The executable path and
/// switches are ignored; web URLs, `nano://` pages and existing HTML files are
/// kept. Duplicates collapse, so `NanoBrowser.exe a.html a.html` opens one tab.
pub fn launch_targets<I, S>(args: I) -> Vec<String>
where
  I: IntoIterator<Item = S>,
  S: AsRef<str>,
{
  let mut out: Vec<String> = Vec::new();
  for arg in args {
    if let Some(t) = launch_target(arg.as_ref()) {
      if !out.contains(&t) {
        out.push(t);
      }
    }
  }
  out
}

pub fn launch_target(raw: &str) -> Option<String> {
  let arg = raw.trim().trim_matches('"');
  if arg.is_empty() || arg.starts_with('-') {
    return None;
  }

  if let Ok(u) = tauri::Url::parse(arg) {
    if matches!(u.scheme(), "http" | "https" | "file" | "nano") {
      return Some(arg.to_string());
    }
  }

  // A file association passes a plain Windows path, not a URL. Only the types
  // NanoBrowser registers are accepted, and only if the file is really there.
  let path = Path::new(arg);
  if !path.is_file() {
    return None;
  }
  let is_html = path
    .extension()
    .and_then(|e| e.to_str())
    .map(|e| matches!(e.to_ascii_lowercase().as_str(), "html" | "htm" | "xhtml"))
    .unwrap_or(false);
  if !is_html {
    return None;
  }
  tauri::Url::from_file_path(path).ok().map(|u| u.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use std::path::PathBuf;

  fn scratch(name: &str) -> PathBuf {
    let dir = std::env::current_exe()
      .unwrap_or_default()
      .parent()
      .unwrap_or(Path::new("."))
      .join("_testscratch")
      .join(name);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("scratch dir");
    dir
  }

  fn value<'a>(writes: &'a [RegWrite], key: &str, name: &str) -> Option<&'a str> {
    writes
      .iter()
      .find(|w| w.key == key && w.name == name)
      .map(|w| w.value.as_str())
  }

  #[test]
  fn the_registration_covers_urls_files_and_the_default_apps_page() {
    let writes = registration_writes(Path::new(r"D:\Apps\NanoBrowser\nanobrowser.exe"));

    assert_eq!(
      value(&writes, r"Software\RegisteredApplications", APP_NAME),
      Some(CAPABILITIES_KEY)
    );
    assert_eq!(value(&writes, CAPABILITIES_KEY, "ApplicationName"), Some(APP_NAME));
    assert_eq!(
      value(&writes, r"Software\NanoBrowser\Capabilities\FileAssociations", ".xhtml"),
      Some(PROGID_HTML)
    );
    assert_eq!(
      value(&writes, r"Software\NanoBrowser\Capabilities\FileAssociations", ".html"),
      Some(PROGID_HTML)
    );
    assert_eq!(
      value(&writes, r"Software\NanoBrowser\Capabilities\URLAssociations", "http"),
      Some(PROGID_URL)
    );
    assert_eq!(
      value(&writes, r"Software\Classes\NanoBrowserURL\shell\open\command", ""),
      Some(r#""D:\Apps\NanoBrowser\nanobrowser.exe" "%1""#)
    );
  }

  #[test]
  fn the_command_follows_the_executable_instead_of_a_baked_in_path() {
    let other = registration_writes(Path::new(r"C:\Program Files\NanoBrowser\nanobrowser.exe"));
    assert_eq!(
      value(&other, r"Software\Classes\NanoBrowserHTML\shell\open\command", ""),
      Some(r#""C:\Program Files\NanoBrowser\nanobrowser.exe" "%1""#)
    );
    assert_eq!(
      value(&other, CAPABILITIES_KEY, "ApplicationIcon"),
      Some(r#""C:\Program Files\NanoBrowser\nanobrowser.exe",0"#)
    );
  }

  #[test]
  fn only_web_urls_and_known_pages_become_launch_targets() {
    assert_eq!(launch_target("https://example.com/a"), Some("https://example.com/a".into()));
    assert_eq!(launch_target("http://localhost:8080"), Some("http://localhost:8080".into()));
    assert_eq!(launch_target("file:///C:/x.html"), Some("file:///C:/x.html".into()));
    assert_eq!(launch_target("nano://settings"), Some("nano://settings".into()));

    // the executable itself, switches and bare phrases are not pages
    assert_eq!(launch_target(r"D:\NanoBrowser\nanobrowser.exe"), None);
    assert_eq!(launch_target("--flag"), None);
    assert_eq!(launch_target("example.com"), None);
    assert_eq!(launch_target("hello world"), None);
    assert_eq!(launch_target(""), None);
  }

  #[test]
  fn an_existing_html_path_is_turned_into_a_file_url() {
    let dir = scratch("launch_target");
    let file = dir.join("hello world.html");
    fs::write(&file, "<!doctype html>").unwrap();

    let url = launch_target(file.to_str().unwrap()).expect("html file");
    assert!(url.starts_with("file:///"), "{}", url);
    assert!(url.ends_with("hello%20world.html"), "{}", url);

    let txt = dir.join("notes.txt");
    fs::write(&txt, "x").unwrap();
    assert_eq!(launch_target(txt.to_str().unwrap()), None);
    assert_eq!(launch_target(dir.join("missing.html").to_str().unwrap()), None);

    let _ = fs::remove_dir_all(&dir);
  }

  #[test]
  fn duplicate_arguments_open_one_tab_each() {
    let args = ["nanobrowser.exe", "https://a.com", "https://a.com", "https://b.com"];
    assert_eq!(launch_targets(args), vec!["https://a.com", "https://b.com"]);
  }

  #[test]
  fn a_url_forwarded_during_setup_waits_for_the_window() {
    let mut q = LaunchQueue::default();
    // still in setup: the URL is held, and a repeated one is not doubled
    assert_eq!(q.accept(vec!["https://a.com".into()]), None);
    assert_eq!(q.accept(vec!["https://a.com".into(), "https://b.com".into()]), None);
    // setup finishes: the held URLs are handed over in arrival order
    assert_eq!(q.mark_ready(), vec!["https://a.com", "https://b.com"]);
    // from now on the callback opens immediately
    assert_eq!(
      q.accept(vec!["https://c.com".into()]),
      Some(vec!["https://c.com".into()])
    );
  }
}
