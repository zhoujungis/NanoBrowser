# NanoBrowser

> An ultra-light, Chrome-style browser built on Tauri 2 + WebView2 — plus a headless automation agent that AI tools can drive over MCP.

[中文文档](README.zh-CN.md) · English

---

## Table of contents

1. [What it is](#1-what-it-is)
2. [Highlights](#2-highlights)
3. [Tech stack](#3-tech-stack)
4. [Repository layout](#4-repository-layout)
5. [Architecture](#5-architecture)
6. [Build and run](#6-build-and-run)
7. [Runtime data](#7-runtime-data)
8. [Browser features](#8-browser-features)
9. [Keyboard shortcuts](#9-keyboard-shortcuts)
10. [Docked DevTools panel](#10-docked-devtools-panel)
11. [Headless automation agent (`nanoagent`)](#11-headless-automation-agent-nanoagent)
12. [Security model](#12-security-model)
13. [The in-process CDP kernel](#13-the-in-process-cdp-kernel)
14. [Design notes and non-obvious constraints](#14-design-notes-and-non-obvious-constraints)
15. [Known limitations](#15-known-limitations)
16. [License](#16-license)

---

## 1. What it is

NanoBrowser is a lightweight desktop browser. It does **not** ship its own rendering
engine: it drives the WebView2 (Chromium/Edge) engine that is already installed on
Windows, which is what keeps the binary tiny and the resource footprint low.

Two binaries are produced from one workspace:

| Binary | Purpose | Approx. size (release) |
| --- | --- | --- |
| `nanobrowser.exe` | The visible GUI browser | **4.52 MB** |
| `nanoagent.exe` | Headless automation browser (CLI + MCP) | **4.24 MB** |

Both share the same in-process DevTools-Protocol bridge (the `nanobrowser` library crate).

The project is deliberately opinionated about a few things:

- **Everything stays on the D: drive.** Runtime data, caches, downloads and the
  WebView2 user-data folder all live inside the project directory. Nothing is written
  to the system drive.
- **No debugging sockets, ever.** Neither the GUI nor the agent opens
  `--remote-debugging-port`. CDP is spoken *in process* through
  `ICoreWebView2::CallDevToolsProtocolMethod`.
- **No external dependencies for the hard parts.** The DevTools websocket bridge is
  hand-written (SHA-1 handshake + frame codec) rather than pulling in `tungstenite`.

---

## 2. Highlights

- **Chrome-like tab strip** — drag to reorder with a FLIP animation, pin/unpin,
  middle-click to close, per-tab context menu, overflow handling.
- **Docked DevTools** — the *real* DevTools frontend (`devtools://devtools/bundled/`)
  loaded into a child webview inside the window, dockable to the **bottom** or the
  **right**, with a draggable grip to resize. No separate popup window.
- **Full browser chrome** — omnibox with suggestions, bookmarks bar, find-in-page,
  downloads manager, history, zoom, light/dark/system themes, native window controls.
- **Internal pages** under the `nano://` scheme.
- **Headless agent** for AI tooling — one-shot CLI commands *and* an MCP server over
  stdio, with redacted network-traffic capture, trusted input synthesis and
  screenshots.

---

## 3. Tech stack

| Layer | Choice | Version |
| --- | --- | --- |
| App framework | Tauri | 2.12.4 |
| WebView abstraction | wry | 0.57.0 |
| Engine | Microsoft Edge WebView2 (Chromium) | system-installed |
| WebView2 COM bindings | `webview2-com` | 0.39.1 |
| Win32 bindings | `windows` | 0.62.2 |
| Serialization | `serde` / `serde_json` | 1.0.154 |
| Language | Rust | edition 2021 |

Cargo features of note:

```toml
tauri = { version = "2", features = ["unstable", "devtools"] }
```

`unstable` is required for the child-webview APIs (`add_child`, `get_webview`,
per-webview `devtools(false)`), and `devtools` for `ICoreWebView2` interop used by the
protocol bridge.

Release profile is tuned for size:

```toml
[profile.release]
opt-level = "s"   # optimize for size
lto = true        # link-time optimization
codegen-units = 1
strip = true      # strip symbols
panic = "abort"   # drop unwinding tables
```

The crates registry is mirrored through `rsproxy` in [`.cargo/config.toml`](.cargo/config.toml).

---

## 4. Repository layout

```
NanoBrowser/
├─ start.bat                       # launcher: prefers release, falls back to debug
├─ .cargo/config.toml              # rsproxy registry mirror
├─ ui/                             # frontend (plain HTML/CSS/JS, no bundler)
│  ├─ index.html                   # trusted shell: tab strip, toolbar, SVG icon sprite
│  ├─ shell.js / shell.css         # shell behaviour and styling
│  ├─ internal.css / internal.js   # shared styling/script for internal pages
│  ├─ ntp.html|js                  # nano://newtab   (new tab page, top sites)
│  ├─ history.html|js              # nano://history
│  ├─ bookmarks.html|js            # nano://bookmarks
│  ├─ downloads.html|js            # nano://downloads
│  ├─ settings.html|js             # nano://settings
│  └─ about.html                   # nano://about
└─ src-tauri/
   ├─ Cargo.toml / Cargo.lock
   ├─ tauri.conf.json              # productName, frontendDist=../ui, withGlobalTauri
   ├─ build.rs
   ├─ capabilities/                # per-webview permission scoping (see §12)
   │  ├─ default.json              # "shell" webview
   │  └─ tabs.json                 # "tab-*": local (nano://) vs remote (http/https)
   ├─ icons/                       # icon.ico / icon.png + _gen_icon.ps1
   └─ src/
      ├─ lib.rs                    # library crate: `pub mod cdp`
      ├─ main.rs                   # GUI binary: window + shell webview + wiring
      ├─ cdp.rs                    # in-process CDP bridge (shared by both binaries)
      ├─ state.rs                  # Settings, Profile, AppState, Tab, constants
      ├─ profile.rs                # data dirs, profile.json load/atomic save
      ├─ tabs.rs                   # tab lifecycle, layout, downloads, shortcuts
      ├─ inject.rs                 # per-tab init script (shortcuts, favicon, find)
      ├─ commands.rs               # the `#[tauri::command]` surface
      ├─ devtools.rs               # docked DevTools panel + websocket bridge
      └─ bin/nanoagent/
         ├─ main.rs                # hidden window, mode dispatch (--mcp vs one-shot)
         ├─ browser.rs             # navigate/read/click/fill/screenshot/cookies/wait
         ├─ cli.rs                 # one-shot command front end + --help
         ├─ mcp.rs                 # MCP server over stdio (tools/list, tools/call)
         ├─ traffic.rs             # Network.* capture with capture-time redaction
         └─ paths.rs               # data dir + automation profile dir
```

---

## 5. Architecture

### 5.1 Webview topology — sibling HWNDs in one window

Everything lives inside a single OS window. Inside it there are up to three kinds of
**child webviews**, each of which is a sibling HWND:

```
┌─ main window ───────────────────────────────────────────────┐
│  shell webview  (label "shell")   ← y: 0 .. shell_h         │
│    tab strip (40) + toolbar (44) + bookmarks bar (28, opt.) │
├──────────────────────────┬──────────────────────────────────┤
│  page webview            │  devtools panel webview          │
│  (label "tab-<id>")      │  (label "devtools")              │
│  one per tab, only the   │  only when open; bottom or right │
│  active one is visible   │                                  │
└──────────────────────────┴──────────────────────────────────┘
```

- **shell** — the trusted browser chrome (`ui/index.html`). It owns all IPC commands.
- **tab-`<id>`** — one child webview per tab, created lazily, hidden/shown on activation.
  All of them share the *same* WebView2 user-data folder so cookies and login state
  carry across tabs.
- **devtools** — the docked panel, created on first open (see §10).

Layout is computed in Rust ([`tabs.rs::layout`](src-tauri/src/tabs.rs)) and applied with
`set_position` / `set_size` on each webview.

`shell_h` is derived from the chrome heights:

```
shell_h = STRIP_H(40) + TOOLBAR_H(44) + (bookmarks_bar ? BM_H(28) : 0)
```

i.e. **84** logical pixels without the bookmarks bar, **112** with it. The page area is
everything below `shell_h`.

### 5.2 Z-order and the "overlay" trick

Because the page webview is created *after* the shell, it is stacked **above** the shell
HWND. That would clip any shell UI that overflows the chrome strip — the main menu, the
omnibox suggestion dropdown, the tab context menu.

The fix (`chrome_overlay` command → [`tabs.rs::set_overlay`](src-tauri/src/tabs.rs)):

1. While a menu/suggestion list is open, the shell webview is grown to the **whole
   window** and raised to the top with `SetWindowPos(HWND_TOP)`.
2. When it closes, the shell is restored to `shell_h`.
3. After any tab create/activate/close, `ensure_shell_top` re-asserts the z-order so the
   chrome does not disappear behind a newly created page webview.

### 5.3 State model

`AppState` (guarded by a single `Mutex`, managed by Tauri):

| Field | Meaning |
| --- | --- |
| `tabs: Vec<Tab>` | live tabs |
| `active: Option<u32>` | active tab id |
| `closed: Vec<(String,String)>` | (url, title) stack for Ctrl+Shift+T |
| `shell_h: f64` | chrome height (see above) |
| `profile: Profile` | persisted settings/bookmarks/history/downloads/session/window |
| `next_id: u32` | tab id allocator |
| `sys_dark: bool` | OS dark-mode flag |
| `overlay: bool` | shell currently expanded to full window |
| `devtools_open: bool` | panel visibility |
| `devtools_h: f64` | bottom-dock panel height |
| `devtools_w: f64` | right-dock panel width |
| `win_save_pending: bool` | debounce flag for window-geometry persistence |

A `Tab` carries `id, label, url, title, loading, pinned, icon, internal, live,
zoom_idx, can_back, can_fwd, expect_internal, nav_pos, nav_total, pending_nav` and an
opaque per-tab `token` (see §12).

### 5.4 IPC surface

The shell calls Rust through **40 `#[tauri::command]`s** declared in
[`commands.rs`](src-tauri/src/commands.rs), registered in
[`main.rs`](src-tauri/src/main.rs) via `generate_handler!`:

- **read state** — `get_initial_state`, `get_tabs`, `get_settings`, `get_bookmarks`,
  `get_history`, `get_downloads`, `get_top_sites`
- **navigation** — `navigate`, `new_tab`, `activate`, `go_back`, `go_forward`,
  `reload`, `stop`, `internal_nav`
- **tabs** — `close_tab_cmd`, `toggle_pin`, `move_tab`, `duplicate_tab`,
  `close_others`, `close_to_right`, `reopen_tab_cmd`
- **UI** — `zoom`, `omnibox_suggest`, `find_query`, `find_move`, `find_close`,
  `chrome_overlay`, `sys_theme_changed`
- **devtools** — `toggle_devtools`, `set_devtools_size`, `set_devtools_side`,
  `devtools_drag_end`
- **data** — `add_bookmark`, `remove_bookmark`, `update_bookmark`, `delete_history`,
  `clear_history`, `set_settings`, `open_downloads_folder`

Rust pushes state back with Tauri events: `tabs-state`, `omnibox-state`,
`bookmarks-changed`, `theme-changed`, `sys-theme`, `find-result`, `focus-omnibox`,
`open-findbar`, `dt-drag`.

---

## 6. Build and run

Prerequisites: Rust toolchain, and Microsoft Edge WebView2 Runtime (preinstalled on
current Windows 10/11).

```powershell
cd src-tauri
cargo build --release      # produces target\release\nanobrowser.exe + nanoagent.exe
```

Then either double-click **`start.bat`** (prefers the release binary, falls back to
debug), or run the executable directly.

Debug build:

```powershell
cargo build                # target\debug\nanobrowser.exe
```

---

## 7. Runtime data

All paths resolve from the executable location, and env overrides exist for tests:

| What | Path | Env override |
| --- | --- | --- |
| Data root | `<project>\data` | `NANO_DATA_DIR` |
| Persisted profile | `<project>\data\profile.json` | — |
| WebView2 user-data folder (GUI) | `<project>\data\webview` | — |
| Default downloads | `<project>\Downloads` | — |
| Automation profile | `<project>\data\automation` | `NANOBROWSER_AGENT_DIR` |
| Agent data root | `<project>\data` | `NANOBROWSER_DATA_DIR` |

### `profile.json`

```jsonc
{
  "settings": {
    "searchEngine": "bing",        // bing | baidu | google
    "theme": "system",             // system | light | dark
    "homepage": "nano://newtab",
    "bookmarksBar": true,
    "startup": "session",          // session | ntp
    "downloadsDir": "",            // empty -> default downloads dir
    "devtoolsSide": "bottom"       // bottom | right
  },
  "bookmarks": [ { "url": "...", "title": "...", "added": 0, "icon": "..." } ],
  "history":   [ { "url": "...", "title": "...", "time": 0 } ],   // capped at 500
  "downloads": [ { "url": "...", "name": "...", "path": "...", "time": 0,
                   "status": "done" } ],                          // capped at 100
  "session":   { "tabs": ["..."], "active": 0 },
  "window":    { "x": 0, "y": 0, "w": 1280, "h": 800, "maximized": false }
}
```

Saves are **atomic**: the JSON is written to `.profile.tmp` and then renamed over
`profile.json`, so a crash mid-write cannot corrupt the profile
([`profile.rs::save_profile`](src-tauri/src/profile.rs)).

---

## 8. Browser features

### Tabs

- drag to reorder; the dragged tab follows the cursor while siblings slide aside (FLIP
  transition), so the strip always previews where the tab will land;
- the click that trails a drag is suppressed deterministically (reset on the next tab
  press, never relying on task-queue timing);
- pin / unpin (pinned tabs are kept left and unsorted past);
- middle-click closes; per-tab context menu (pin, duplicate, close, close others,
  close to the right);
- overflow handling so tabs are never hidden under the toolbar buttons;
- `closed` stack + `Ctrl+Shift+T` to reopen.

### Omnibox

Input is parsed by [`tabs.rs::parse_input`](src-tauri/src/tabs.rs):

1. an internal page (`nano://…`) → used verbatim;
2. something that "looks like a URL" → prefixed with `https://` if needed
   (a host must be `localhost`, an IP, or contain a dot);
3. otherwise → a search on the configured engine.

Suggestions come from `omnibox_suggest` (history + bookmark matches).

### Navigation

Per-tab back/forward stacks, `can_back` / `can_fwd`, loading state (the reload button
morphs into a stop button), all reflected into `omnibox-state`.

### Bookmarks, history, downloads

- Bookmarks have a real `icon` field captured from the page (see §12 for the favicon
  validation rules).
- History is capped at 500 entries, downloads at 100.
- Downloads use a **collision-safe, race-free** naming scheme: the target file is
  claimed with an exclusive `create_new` open, and on collision the name becomes
  `report (1).pdf`, `report (2).pdf`, … (`unique_path` / `reserve` in
  [`tabs.rs`](src-tauri/src/tabs.rs)). Two concurrent downloads of the same URL can
  therefore never claim the same path.

### Internal pages (`nano://`)

| URL | File | Title |
| --- | --- | --- |
| `nano://newtab` | `ntp.html` | 新标签页 |
| `nano://history` | `history.html` | 历史记录 |
| `nano://bookmarks` | `bookmarks.html` | 书签管理 |
| `nano://downloads` | `downloads.html` | 下载内容 |
| `nano://settings` | `settings.html` | 设置 |
| `nano://about` | `about.html` | 关于 NanoBrowser |

These run in tab webviews but under the **local** capability (full IPC), unlike remote
pages.

### Find in page

An injected script highlights matches; the shell drives `find_query` / `find_move` /
`find_close` and shows a match counter (`find-result` event).

### Zoom

`ZOOM_STEPS` is a fixed 15-step ladder; `Ctrl+±/0` map to `ctrl-zoom-in/out/reset`.

### Themes

`system | light | dark`, resolved against the OS dark-mode flag (`sys-theme` event) and
applied by setting `data-theme` on the document element.

### Window chrome

Custom titlebar with minimize / maximize-restore / close, drag region, and F11
fullscreen. Icons in the window controls are injected with their CSS classes carried
through, which is what keeps the fullscreen "restore" glyph correctly sized.

---

## 9. Keyboard shortcuts

Shortcuts pressed inside a **page** are reported by the injected script
([`inject.rs`](src-tauri/src/inject.rs)) and handled in
[`tabs.rs::handle_shortcut`](src-tauri/src/tabs.rs); shortcuts pressed while the **shell**
is focused are handled directly in `shell.js`.

| Shortcut | Action |
| --- | --- |
| `Ctrl+T` | New tab |
| `Ctrl+W` | Close tab |
| `Ctrl+Shift+T` | Reopen last closed tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab |
| `Ctrl+1` … `Ctrl+8` | Select tab by index |
| `Ctrl+9` | Select last tab |
| `Ctrl+L` | Focus the address bar |
| `Ctrl+F` | Find in page |
| `Ctrl+D` | Bookmark the current page |
| `Ctrl+H` | History |
| `Ctrl+J` | Downloads |
| `Ctrl+Shift+O` | Bookmarks manager |
| `Alt+←` / `Alt+→` | Back / forward |
| `Ctrl++` / `Ctrl+-` | Zoom in / out |
| `Ctrl+0` | Reset zoom |
| `F5` | Reload (left to the engine) |
| `F11` | Toggle fullscreen |
| `F12` / `Ctrl+Shift+I` | Toggle the docked DevTools panel |

Editing keys (`Ctrl+C/V/X/A`) and `Ctrl+R` / `Ctrl+P` are deliberately passed through to
the engine. `Ctrl+Shift+1…9` is also passed through to the page.

---

## 10. Docked DevTools panel

This is the most intricate part of the project, so it gets its own section.

### 10.1 Why the panel exists at all

WebView2 ships the real DevTools frontend but only exposes it through
`OpenDevToolsWindow`, which pops out a **separate** window. To get Chrome's docked panel
instead, the frontend is loaded into a **second child webview inside the same window**.

Two alternative routes were tried and rejected:

- **`--remote-debugging-port`** — infeasible: only the shell webview ends up exposed as a
  CDP target; page webviews cannot be attached to by the official frontend.
- **A hand-built minimal panel** — rejected in favour of the official frontend so the
  full Elements / Console / Network / Sources experience is available.

The chosen route: **official frontend + a loopback websocket bridge written in-process**.

### 10.2 The URL

```
devtools://devtools/bundled/inspector.html?ws=127.0.0.1:PORT/devtools/page/<session-id>#nanoSide=bottom|right
```

- The frontend is loaded from the `devtools://devtools/bundled/` bundle WebView2 already
  provides.
- It speaks CDP over a **loopback websocket** served by this process.
- The dock side travels in the **fragment** (`#nanoSide=…`), *not* the query string.

> **Critical, non-obvious constraint.** Appending a second query key after `?ws=` (e.g.
> `&nanoSide=right`) makes WebView2's `devtools://` handler fail the load outright and
> show Chromium's *"This page can't be reached"* error page. A fragment is resolved by
> the browser and never reaches the handler, so it is safe. See §14.

### 10.3 The loopback bridge — and why it is not a debugging port

`devtools.rs` runs a hand-written websocket **server** (SHA-1 handshake + base64 accept
key + text/close frame codec — no external crates):

- it binds a random **loopback** port;
- it only accepts the single session id that this process minted for the panel's own URL;
- every CDP request is relayed to a page webview through
  `ICoreWebView2::CallDevToolsProtocolMethod`.

A page that scanned `localhost` would find a listener that answers nothing without that
id, and the id lives only as long as the panel does. Nothing outside the process can
attach to either browser.

### 10.4 Events

`Schema.getDomains` returns 35 domains but **zero** events, so events cannot be
discovered dynamically. Instead a **static event-name table** is used and subscriptions
are registered **lazily per domain** as the frontend enables them. Events whose
subscription fails are skipped individually rather than dropping the whole domain.
`Page.startScreencast` is not implemented by WebView2; such calls are returned to the
frontend as CDP errors instead of breaking the bridge.

### 10.5 Geometry

All sizing is computed in
[`devtools.rs::split`](src-tauri/src/devtools.rs), which returns the page rectangle and
the panel rectangle:

| Constant | Value | Meaning |
| --- | --- | --- |
| `MIN_H` | 96 | smallest bottom-dock panel height |
| `DEFAULT_H` | 320 | first-open bottom-dock height |
| `MIN_PAGE_H` | 96 | smallest page height left |
| `MIN_W` | 240 | smallest right-dock panel width |
| `DEFAULT_W` | 480 | first-open right-dock width |
| `MIN_PAGE_W` | 240 | smallest page width left |

- **Bottom dock** — page is `(content_w, content_h − h)`; panel is
  `(0, shell_h + page_h, content_w, h)`.
- **Right dock** — page is `(content_w − w, content_h)`; panel is
  `(page_w, shell_h, w, content_h)`.

`clamp_axis` keeps both the panel and the page above their minimums.

### 10.6 The grip / resize

The injected `GRIP_JS` installs a drag strip on the panel's **page-facing edge**:

- bottom dock → a horizontal strip along the **top** edge, `cursor: ns-resize`;
- right dock → a vertical strip along the **left** edge, `cursor: ew-resize`.

Because a child webview cannot see pointer movement outside its own bounds, dragging is
handled by the shell: on mouse-down the panel asks Rust to `set_overlay(true)` (shell
grown to the whole window, on top), and the shell's full-window catcher (`#dt-grab`)
forwards `clientX` / `clientY` to `set_devtools_size`. Rust then computes:

```
bottom:  devtools_h = clamp(window_h − clientY)
right:   devtools_w = clamp(window_w − clientX)
```

On mouse-up the catcher is removed and the overlay restored.

### 10.7 Panel ↔ native signalling

The panel is a `devtools://` document, where **neither** Tauri IPC **nor** WebView2's
host object (`window.chrome.webview`) exists. So the panel talks to Rust through a popup
to a host that cannot exist:

```js
window.open('https://nano-dt.invalid/<action>', '_blank');
```

`on_new_window` sees the request, reads the action off the path and returns
`NewWindowResponse::Deny`. Nothing is ever navigated to.

### 10.8 Lifecycle

- `F12`, `Ctrl+Shift+I` and the main menu all call `toggle_devtools`
  → [`devtools::toggle_for`](src-tauri/src/devtools.rs).
- `activate` / `create_tab` call `devtools::retarget` so the panel follows the active
  tab.
- `close_tab` calls `devtools::forget` to drop the protocol state tied to a dead tab.
- Pressing `F12` **inside the panel** closes it.
- The dock side is persisted in `settings.devtoolsSide`; switching sides reloads the
  panel so the grip ends up on the correct edge.
- Every webview — shell, page tabs *and* the panel — is created with `.devtools(false)`,
  which disables the native DevTools popup while leaving `CallDevToolsProtocolMethod`
  fully functional.

---

## 11. Headless automation agent (`nanoagent`)

`nanoagent` is the *same* browser engine in a window that is **never shown**, driven by
AI tooling.

Design properties:

- The window is built with `visible(false)`, `skip_taskbar(true)`, `decorations(false)`
  and its **own** WebView2 user-data folder (`data\automation`), completely isolated from
  the GUI's profile — so automation login state and the human's cookies never mix.
- Control goes through the **in-process** CDP bridge: no debugging port.
- Instances start a **fresh** browser per one-shot invocation; the MCP mode keeps one
  warm page across many tool calls.

### 11.1 One-shot CLI

```
nanoagent --mcp                              speak MCP over stdio (long-lived session)
nanoagent <command> [args...]                run one action and exit

navigate <url>                               open a URL, print {url,title}
read <url> [selector] [--html]               print page text (or HTML)
shot <url> <out.png>                         save a PNG screenshot
cookies <url>                                list the profile's cookies for that page
traffic <url> [limit]                        print recorded requests (secrets redacted)
eval <url> <js>                              evaluate JavaScript, print its value
click <url> <selector>                       click with a trusted mouse event
fill <url> <selector> <value>                type into an input
wait <url> <selector> [timeout_ms]           wait for an element to appear
```

### 11.2 MCP server over stdio

`--mcp` speaks JSON-RPC 2.0 on stdin/stdout, with `tools/list` and `tools/call`
(`serverInfo.name = "nanobrowser-agent"`). Tools:

| Tool | Notes |
| --- | --- |
| `browser_navigate` | open a URL, return `{url,title}` |
| `browser_read` | page text or HTML, optional selector |
| `browser_click` | synthesizes a **trusted** mouse event |
| `browser_fill` | `Input.insertText` into a selector |
| `browser_wait_for` | poll until a selector exists |
| `browser_screenshot` | PNG bytes |
| `browser_cookies` | cookies of the automation profile |
| `browser_traffic` | recorded requests, redacted |
| `browser_eval` | evaluate JS, return its value |

### 11.3 Implementation notes

- **Trusted input.** `click` uses `Input.dispatchMouseEvent` and `fill` uses
  `Input.insertText`, so pages see genuine user input, not scripted DOM writes.
- **Screenshots** go through `Page.startScreencast`: the hidden window still composites,
  but WebView2 does not implement `Page.captureScreenshot`. If the hidden window ever
  stops compositing, the documented fallback is to move the window off-screen rather than
  set `visible(false)`.
- **No blocking the UI thread** while waiting for CDP replies (that deadlocks) — replies
  land in shared state and a worker thread waits on a condvar (see §13).
- **Readiness.** The worker polls `is_ready()` up to 600 × 50 ms before issuing CDP,
  because the webview needs a running event loop before it answers.

---

## 12. Security model

### 12.1 Page-event origin validation

Every tab webview's init script embeds an **opaque random token** minted by Rust
(`new_token()`, 24 hex chars). A page cannot forge an event for another tab because it
does not know that tab's token; Rust resolves the sender with `tab_id_by_token` and
**never** trusts a tab id supplied by the page.

### 12.2 Favicon handling

Favicons are reported by pages and displayed by the trusted shell, so the value is
treated as hostile: it is HTML-escaped before insertion and an `<img onerror>` swaps to
a built-in globe glyph. A page can therefore never get a URL of its choosing injected
into the shell.

### 12.3 Capability scoping (per-webview permissions)

Tauri capabilities are scoped per webview label (see `capabilities/`):

| Capability | Applies to | Grants |
| --- | --- | --- |
| `shell` | `"shell"` | `core:default` + window control/query permissions |
| `tab-local` | `tab-*` running internal pages | `core:default` + `core:event:allow-emit` → full app IPC |
| `tab-remote` | `tab-*` on external URLs | **only** `core:event:allow-emit` — remote pages may *report UI events*, never invoke commands |

The remote capability must cover **all ports** (`https://*:*`, `http://*:*`, plus
`https://*/*`). If a site on a non-default port is not matched, the injected script
silently stops working (shortcuts, hover status, find highlight, favicon reporting all
break at once).

### 12.4 No debugging sockets

Neither binary ever passes `--remote-debugging-port`. All CDP traffic goes through
`ICoreWebView2::CallDevToolsProtocolMethod` inside the process. The GUI's DevTools
websocket bridge binds loopback and is gated by a per-session id (§10.3).

### 12.5 Network-traffic redaction

The agent's traffic capture redacts sensitive material **at capture time**, so a secret
never reaches the log buffer at all. Redaction cannot be turned off:

- 18 sensitive key substrings (`password, passwd, pwd, pass, token, secret, apikey,
  api_key, api-key, auth, cookie, session, csrf, credit, card, cvv, ssn, otp`) →
  `***` in both JSON bodies and header maps;
- `Authorization` / `Cookie` headers and form bodies are covered by the same rules;
- bodies are truncated (`MAX_BODY = 4096`, `MAX_VALUE = 512`), and the buffer is capped
  at `MAX_ENTRIES = 500`;
- the log is **memory-only** — nothing is written to disk.

A redirect that reuses the same CDP `requestId` would overwrite the original record, so
entries are keyed by a **redirect-chain-derived unique key** rather than the raw id.

### 12.6 Profile isolation

The automation browser uses its own WebView2 data directory, distinct from the GUI's.
Cookies and login state do not cross the boundary in either direction.

---

## 13. The in-process CDP kernel

`cdp.rs` is shared by both binaries. Two properties of
`ICoreWebView2::CallDevToolsProtocolMethod` shape the whole design:

1. **There is no `{"id": …}` envelope.** The completion callback receives only the
   method's own result object, so replies are correlated by a **sequence number** baked
   into each per-call handler.
2. **The callback runs on the WebView2 (UI) thread.** Waiting for it there would
   deadlock. Every call is therefore **fire-and-forget**, and the *caller* — always a
   worker thread, never the UI thread — parks on a `Condvar` until the reply lands.

On top of that, two call styles exist:

| API | Style | Used by |
| --- | --- | --- |
| `call` / `call_raw` / `call_within` | blocks for the reply (default 30 s) | the automation agent |
| `send` | hands the reply to a callback | the DevTools websocket bridge (must never block; it answers on its own socket thread) |

Other details:

- `on_event(event, cb)` subscribes through
  `GetDevToolsProtocolEventReceiver`; once registered, WebView2 owns the subscription
  object, so the callback lives as long as the webview does.
- `with_webview` must be called **off** the main thread.
- An empty reply (methods that return nothing) becomes `null`.
- `Cdp::webview()` resolves either a child webview (`"tab-<id>"`, `"devtools"`) or a
  `WebviewWindow` (`"nanoagent"`), so one implementation serves both binaries.

---

## 14. Design notes and non-obvious constraints

A distilled list of the traps this codebase has already paid for. Much of it is
counter-intuitive and worth reading before changing the corresponding area.

**Layout / z-order**

1. With each tab as its own HWND, a page webview created *after* the shell is stacked
   above it and clips menus/suggestions. Fix: grow the shell to the full window and
   raise it while a menu is open; restore on close; re-raise after tab create/activate/close.
2. A child webview cannot see pointer movement outside its own bounds, so panel resizing
   must route the cursor through a full-window catcher in the shell webview.
3. Window-control icons are injected by JS; the SVG's CSS **class must be carried
   through**, or class-sized glyphs (e.g. the fullscreen restore icon) render at their
   default oversized dimensions.

**Concurrency / downloads**

4. Closing tabs by index concurrently deletes the wrong neighbour; re-look-up the tab
   under the lock before closing.
5. Same-URL concurrent downloads collapse if matched by URL alone; claim the file with
   an atomic `create_new` placeholder instead.

**CDP**

6. Waiting for a CDP reply on the UI thread (a hidden window in particular) deadlocks.
   Use a callback writing into shared state, plus a worker thread polling/waiting.
7. A redirect reusing the same `requestId` overwrites the original request record and
   loses its body; key by a redirect-chain-derived unique key.
8. `--remote-debugging-port` is a dead end: only the shell webview becomes a CDP target.
9. `Schema.getDomains` lists domains but returns no event names; a static event table
   with lazy per-domain registration is required.
10. `AreDevToolsEnabled = false` (`.devtools(false)`) does **not** affect
    `CallDevToolsProtocolMethod`, so the native popup can be killed while keeping full CDP.

**Docked DevTools**

11. `devtools://` is a privileged page where `window.chrome.webview` and Tauri IPC are
    both unavailable; `window.open` + `on_new_window → Deny` is the only signalling
    channel.
12. Adding a **second query key** after `?ws=` breaks the `devtools://` load entirely —
    put extra data in the **fragment**. When testing this, do not trust
    `webview.url()` (an error page still reports the requested URL); the reliable signals
    are whether the websocket bridge got a client and how many domains it registered.
13. The grip lives on the edge facing the page and its cursor follows the dock axis; the
    drag math is `panel_h = window_h − clientY` (bottom) or
    `panel_w = window_w − clientX` (right).

**Capabilities**

14. A site on a non-default port that is not matched by the capability declaration loses
    *every* injected feature at once (shortcuts, hover status, find highlight, favicon).

---

## 15. Known limitations

- **History and the new-tab "top sites" still guess favicons** at `scheme://host/favicon.ico`,
  while bookmarks store the real icon reported by the page. Unifying them on the
  bookmark logic is a known follow-up.
- The docked panel supports **bottom** and **right** docks only (no left / detached
  window).
- Windows-only: the whole stack is built on WebView2 COM interop.
- No packaging/release pipeline is committed beyond the `nsis` bundle target declared in
  `tauri.conf.json`.

---

## 16. License

Not specified in this repository. Add a `LICENSE` file before distributing.
