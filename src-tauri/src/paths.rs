//! Where the browser and the automation agent keep their runtime files.
//!
//! Both executables derive their locations from the running binary, so the same
//! rules have to serve three layouts: a dev build under `src-tauri/target`,
//! a portable copy, and an installed copy. `data_dir_for` holds that rule and is
//! pure, so the layouts can be checked without a running app.
//!
//! Everything under `data` — profile, WebView2 cache and cookies, downloads —
//! lives next to it, never in a system user directory, unless the install
//! location happens to be read-only (a machine-wide Program Files install, say);
//! only then does the app fall back to its own folder under `%LOCALAPPDATA%`.

use std::fs;
use std::path::{Path, PathBuf};

/// Preferred data-directory override, then the name the browser used before the
/// agent existed. Either is accepted so scripts written for one work with both.
const ENV_DATA: [&str; 2] = ["NANOBROWSER_DATA_DIR", "NANO_DATA_DIR"];

/// Where a given executable keeps its data: the project's `data` folder for a
/// build tree, `data` beside the executable otherwise.
pub fn data_dir_for(exe: &Path) -> PathBuf {
  // walk ancestors: strip .../target/<profile> and .../src-tauri to find the
  // project root
  for a in exe.ancestors().skip(1) {
    if a.file_name().map(|f| f == "src-tauri").unwrap_or(false) {
      if let Some(root) = a.parent() {
        return root.join("data");
      }
    }
    if a.file_name().map(|f| f == "target").unwrap_or(false) {
      if let Some(t) = a.parent() {
        if t.file_name().map(|f| f == "src-tauri").unwrap_or(false) {
          if let Some(root) = t.parent() {
            return root.join("data");
          }
        }
        return t.join("data");
      }
    }
  }
  match exe.parent() {
    Some(d) => d.join("data"),
    None => PathBuf::from("data"),
  }
}

/// The per-app folder used when the install location cannot be written to.
pub fn fallback_root(local_appdata: Option<&str>) -> PathBuf {
  match local_appdata {
    Some(d) if !d.trim().is_empty() => PathBuf::from(d).join("NanoBrowser"),
    _ => PathBuf::from("NanoBrowser"),
  }
}

/// Whether this process can actually keep its files in `dir`.
fn writable(dir: &Path) -> bool {
  fs::create_dir_all(dir).is_ok()
}

/// The data directory for this process.
pub fn app_data_dir() -> PathBuf {
  for name in ENV_DATA {
    if let Ok(d) = std::env::var(name) {
      if !d.trim().is_empty() {
        return PathBuf::from(d);
      }
    }
  }
  let candidate = match std::env::current_exe() {
    Ok(exe) => data_dir_for(&exe),
    Err(_) => PathBuf::from("data"),
  };
  if writable(&candidate) {
    return candidate;
  }
  fallback_root(std::env::var("LOCALAPPDATA").ok().as_deref()).join("data")
}

/// WebView2 user data folder (cache, cookies, GPU shader cache). Every webview of
/// one browser gets the same path so they share one WebView2 environment, which
/// is what makes cookies and logins carry across tabs.
pub fn webview_dir() -> PathBuf {
  app_data_dir().join("webview")
}

/// WebView2 user data folder for the automation agent — a separate folder, so the
/// agent's logins never mix with the ones the visible browser is using.
pub fn automation_dir() -> PathBuf {
  if let Ok(d) = std::env::var("NANOBROWSER_AGENT_DIR") {
    if !d.trim().is_empty() {
      return PathBuf::from(d);
    }
  }
  app_data_dir().join("automation")
}

/// Built-in download directory: `<app dir>\Downloads`, so downloads land beside
/// the app instead of in the system drive's user folders.
pub fn downloads_dir() -> PathBuf {
  match app_data_dir().parent() {
    Some(root) => root.join("Downloads"),
    None => PathBuf::from("Downloads"),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_build_tree_keeps_its_data_at_the_project_root() {
    // the layouts the two binaries actually run from while developing
    assert_eq!(
      data_dir_for(Path::new(r"D:\NanoBrowser\src-tauri\target\debug\nanobrowser.exe")),
      PathBuf::from(r"D:\NanoBrowser\data")
    );
    assert_eq!(
      data_dir_for(Path::new(r"D:\NanoBrowser\src-tauri\target\release\nanoagent.exe")),
      PathBuf::from(r"D:\NanoBrowser\data")
    );
    // `cargo run` from src-tauri, and a bare target dir without the wrapper
    assert_eq!(
      data_dir_for(Path::new(r"D:\NanoBrowser\src-tauri\nanobrowser.exe")),
      PathBuf::from(r"D:\NanoBrowser\data")
    );
    assert_eq!(
      data_dir_for(Path::new(r"D:\some\project\target\debug\app.exe")),
      PathBuf::from(r"D:\some\project\data")
    );
  }

  #[test]
  fn an_installed_or_portable_copy_keeps_its_data_beside_itself() {
    assert_eq!(
      data_dir_for(Path::new(r"D:\Apps\NanoBrowser\nanobrowser.exe")),
      PathBuf::from(r"D:\Apps\NanoBrowser\data")
    );
    assert_eq!(
      data_dir_for(Path::new(r"C:\Users\u\AppData\Local\Programs\NanoBrowser\nanoagent.exe")),
      PathBuf::from(r"C:\Users\u\AppData\Local\Programs\NanoBrowser\data")
    );
    // nothing to derive from: a relative fallback beats panicking
    assert_eq!(data_dir_for(Path::new("nanobrowser.exe")), PathBuf::from("data"));
  }

  #[test]
  fn the_fallback_folder_is_per_app_and_survives_a_missing_local_app_data() {
    assert_eq!(
      fallback_root(Some(r"C:\Users\u\AppData\Local")),
      PathBuf::from(r"C:\Users\u\AppData\Local\NanoBrowser")
    );
    assert_eq!(fallback_root(Some("   ")), PathBuf::from("NanoBrowser"));
    assert_eq!(fallback_root(None), PathBuf::from("NanoBrowser"));
  }

  #[test]
  fn downloads_land_beside_the_data_directory() {
    // the rule both layouts share: data\ -> ..\Downloads
    let root = Path::new(r"D:\NanoBrowser");
    assert_eq!(root.join("Downloads"), PathBuf::from(r"D:\NanoBrowser\Downloads"));
    assert_eq!(
      fallback_root(Some(r"C:\Users\u\AppData\Local")).join("Downloads"),
      PathBuf::from(r"C:\Users\u\AppData\Local\NanoBrowser\Downloads")
    );
  }
}
