//! Filesystem locations used by the automation agent.
//!
//! The rules live in `nanobrowser::paths`, which both binaries share: data sits
//! beside the app (or the project root for a build tree), and the agent gets its
//! own WebView2 profile folder (`data\automation`) so automation logins never mix
//! with the ones the visible browser holds.

use std::path::PathBuf;

/// WebView2 user data folder for automation: `<data>\automation`
/// (or `NANOBROWSER_AGENT_DIR`).
pub fn automation_dir() -> PathBuf {
  nanobrowser::paths::automation_dir()
}
