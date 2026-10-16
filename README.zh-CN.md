# NanoBrowser

> 基于 Tauri 2 + WebView2 的极致轻量、Chrome 风格桌面浏览器 —— 并为 AI 工具附带一个可通过 MCP 驱动的无头自动化浏览器。

中文 · [English](README.md)

---

## 目录

1. [项目简介](#1-项目简介)
2. [核心亮点](#2-核心亮点)
3. [技术栈](#3-技术栈)
4. [仓库结构](#4-仓库结构)
5. [架构设计](#5-架构设计)
6. [构建与运行](#6-构建与运行)
7. [运行时数据](#7-运行时数据)
8. [浏览器功能](#8-浏览器功能)
9. [快捷键](#9-快捷键)
10. [内嵌停靠 DevTools 面板](#10-内嵌停靠-devtools-面板)
11. [无头自动化浏览器 `nanoagent`](#11-无头自动化浏览器-nanoagent)
12. [安全模型](#12-安全模型)
13. [进程内 CDP 内核](#13-进程内-cdp-内核)
14. [设计权衡与非直觉约束](#14-设计权衡与非直觉约束)
15. [已知限制](#15-已知限制)
16. [许可证](#16-许可证)

---

## 1. 项目简介

NanoBrowser 是一个轻量级桌面浏览器。它**不自带渲染引擎**，而是驱动 Windows 上已安装的 WebView2（Chromium/Edge）引擎，这正是体积小、资源占用低的原因。

同一个 workspace 产出两个二进制：

| 二进制 | 用途 | 体积（release，约） |
| --- | --- | --- |
| `nanobrowser.exe` | 可见的 GUI 浏览器 | **4.52 MB** |
| `nanoagent.exe` | 无头自动化浏览器（CLI + MCP） | **4.24 MB** |

两者共用同一套进程内 DevTools 协议桥（`nanobrowser` 库 crate）。

项目在以下几件事上刻意如此：

- **一切都在 D 盘。** 运行时数据、缓存、下载、WebView2 用户数据目录全部位于项目目录下，绝不写入系统盘。
- **绝不开启调试端口。** GUI 与 agent 都不会传 `--remote-debugging-port`。CDP 通过 `ICoreWebView2::CallDevToolsProtocolMethod` 在**进程内**通信。
- **难点不引外部依赖。** DevTools 的 websocket 桥是手写的（SHA-1 握手 + 帧编解码），没有引入 `tungstenite`。

---

## 2. 核心亮点

- **Chrome 风格标签条** —— 拖拽重排（带 FLIP 动画）、固定/取消固定、中键关闭、每个标签的右键菜单、溢出处理。
- **内嵌停靠 DevTools** —— 把**官方** DevTools 前端（`devtools://devtools/bundled/`）加载进窗口内的子 webview，可停靠在**底部**或**右侧**，边缘可拖拽改尺寸。没有独立弹窗。
- **完整浏览器外壳** —— 带建议的地址栏、书签栏、页内查找、下载管理、历史、缩放、浅色/深色/跟随系统主题、原生窗口控制按钮。
- **`nano://` 内置页面**。
- **无头 agent** —— 面向 AI 工具，既提供一次性 CLI 命令，也提供基于 stdio 的 MCP server，并带**默认脱敏**的网络报文捕获、可信输入合成与页面截图。

---

## 3. 技术栈

| 层次 | 选型 | 版本 |
| --- | --- | --- |
| 应用框架 | Tauri | 2.12.4 |
| WebView 抽象 | wry | 0.57.0 |
| 引擎 | Microsoft Edge WebView2（Chromium） | 系统自带 |
| WebView2 COM 绑定 | `webview2-com` | 0.39.1 |
| Win32 绑定 | `windows` | 0.62.2 |
| 序列化 | `serde` / `serde_json` | 1.0.154 |
| 语言 | Rust | edition 2021 |

需要留意的 Cargo features：

```toml
tauri = { version = "2", features = ["unstable", "devtools"] }
```

`unstable` 提供子 webview 相关 API（`add_child`、`get_webview`、每个 webview 的 `devtools(false)`），`devtools` 提供协议桥所需的 `ICoreWebView2` 互操作。

release profile 为体积而调：

```toml
[profile.release]
opt-level = "s"   # 以体积优先优化
lto = true        # 链接时优化
codegen-units = 1
strip = true      # 剥离符号
panic = "abort"   # 去掉 unwind 表
```

crates 源通过 [`.cargo/config.toml`](.cargo/config.toml) 里的 `rsproxy` 镜像。

---

## 4. 仓库结构

```
NanoBrowser/
├─ start.bat                       # 启动器：优先 release，回退 debug
├─ .cargo/config.toml              # rsproxy 源镜像
├─ ui/                             # 前端（纯 HTML/CSS/JS，无打包器）
│  ├─ index.html                   # 可信外壳：标签条、工具栏、SVG 图标 sprite
│  ├─ shell.js / shell.css         # 外壳逻辑与样式
│  ├─ internal.css / internal.js   # 内置页面共用样式/脚本
│  ├─ ntp.html|js                  # nano://newtab   新标签页（常用网站）
│  ├─ history.html|js              # nano://history
│  ├─ bookmarks.html|js            # nano://bookmarks
│  ├─ downloads.html|js            # nano://downloads
│  ├─ settings.html|js             # nano://settings
│  └─ about.html                   # nano://about
└─ src-tauri/
   ├─ Cargo.toml / Cargo.lock
   ├─ tauri.conf.json              # productName、frontendDist=../ui、withGlobalTauri
   ├─ build.rs
   ├─ capabilities/                # 按 webview 分级的权限声明（见 §12）
   │  ├─ default.json              # "shell" 外壳 webview
   │  └─ tabs.json                 # "tab-*"：本地(nano://) 与远程(http/https) 两套
   ├─ icons/                       # icon.ico / icon.png + _gen_icon.ps1
   └─ src/
      ├─ lib.rs                    # 库 crate：`pub mod cdp`
      ├─ main.rs                   # GUI 二进制：窗口 + 外壳 webview + 接线
      ├─ cdp.rs                    # 进程内 CDP 桥（两个二进制共用）
      ├─ state.rs                  # Settings / Profile / AppState / Tab / 常量
      ├─ profile.rs                # 数据目录、profile.json 读写与原子落盘
      ├─ tabs.rs                   # 标签生命周期、布局、下载、快捷键
      ├─ inject.rs                 # 每个标签的注入脚本（快捷键、favicon、查找）
      ├─ commands.rs               # `#[tauri::command]` 命令面
      ├─ devtools.rs               # 内嵌停靠面板 + websocket 桥
      └─ bin/nanoagent/
         ├─ main.rs                # 隐藏窗口、模式分发（--mcp 与一次性命令）
         ├─ browser.rs             # navigate/read/click/fill/screenshot/cookies/wait
         ├─ cli.rs                 # 一次性命令前端与 --help
         ├─ mcp.rs                 # 基于 stdio 的 MCP server（tools/list、tools/call）
         ├─ traffic.rs             # Network.* 捕获，捕获时即脱敏
         └─ paths.rs               # 数据目录与自动化 profile 目录
```

---

## 5. 架构设计

### 5.1 webview 拓扑：同一窗口内的兄弟 HWND

所有内容都在一个 OS 窗口里。窗口内最多有三类**子 webview**，彼此是兄弟 HWND：

```
┌─ 主窗口 ────────────────────────────────────────────────────┐
│  外壳 webview（label "shell"） ← y: 0 .. shell_h             │
│    标签条(40) + 工具栏(44) + 书签栏(28，可选)                │
├──────────────────────────┬──────────────────────────────────┤
│  页面 webview            │  DevTools 面板 webview           │
│  （label "tab-<id>"）    │  （label "devtools"）            │
│  每个标签一个，仅当前    │  仅在打开时存在；底部或右侧      │
│  活动的可见              │                                  │
└──────────────────────────┴──────────────────────────────────┘
```

- **shell** —— 可信浏览器外壳（`ui/index.html`），承载全部 IPC 命令。
- **tab-`<id>`** —— 每个标签一个子 webview，惰性创建，切换时显隐。它们共享**同一个** WebView2 用户数据目录，因此 Cookie 与登录态跨标签页互通。
- **devtools** —— 停靠面板，首次打开时创建（见 §10）。

布局在 Rust 侧计算（[`tabs.rs::layout`](src-tauri/src/tabs.rs)），再通过各 webview 的 `set_position` / `set_size` 应用。

`shell_h` 由外壳各段高度推导：

```
shell_h = STRIP_H(40) + TOOLBAR_H(44) + (书签栏 ? BM_H(28) : 0)
```

即无书签栏时 **84** 逻辑像素，有书签栏时 **112**。页面区域为 `shell_h` 以下的全部空间。

### 5.2 z-order 与「overlay」机制

因为页面 webview 是在外壳**之后**创建的，它在外壳 HWND **之上**。这会裁掉任何超出外壳条带高度的外壳 UI —— 主菜单、地址栏建议下拉、标签右键菜单。

解决方案（`chrome_overlay` 命令 → [`tabs.rs::set_overlay`](src-tauri/src/tabs.rs)）：

1. 菜单/建议下拉打开期间，把外壳 webview 放大到**整窗**并用 `SetWindowPos(HWND_TOP)` 置顶；
2. 关闭时把外壳还原为 `shell_h` 高度；
3. 任何标签的新建/激活/关闭之后，`ensure_shell_top` 会重新确保 z-order，避免外壳被新创建的页面 webview 压到下方。

### 5.3 状态模型

`AppState`（由单个 `Mutex` 保护，交给 Tauri 托管）：

| 字段 | 含义 |
| --- | --- |
| `tabs: Vec<Tab>` | 当前标签 |
| `active: Option<u32>` | 活动标签 id |
| `closed: Vec<(String,String)>` | （url, title）栈，供 Ctrl+Shift+T 恢复 |
| `shell_h: f64` | 外壳高度（见上） |
| `profile: Profile` | 持久化的 settings/bookmarks/history/downloads/session/window |
| `next_id: u32` | 标签 id 分配器 |
| `sys_dark: bool` | 系统深色模式标志 |
| `overlay: bool` | 外壳是否已放大到整窗 |
| `devtools_open: bool` | 面板是否打开 |
| `devtools_h: f64` | 底部停靠时的面板高度 |
| `devtools_w: f64` | 右侧停靠时的面板宽度 |
| `win_save_pending: bool` | 窗口几何持久化的防抖标志 |

`Tab` 携带 `id, label, url, title, loading, pinned, icon, internal, live, zoom_idx,
can_back, can_fwd, expect_internal, nav_pos, nav_total, pending_nav`，以及一个不透明的
每标签 `token`（见 §12）。

### 5.4 IPC 命令面

外壳通过 [`commands.rs`](src-tauri/src/commands.rs) 中的 **40 个 `#[tauri::command]`**
调用 Rust，在 [`main.rs`](src-tauri/src/main.rs) 里用 `generate_handler!` 注册：

- **读取状态** —— `get_initial_state`、`get_tabs`、`get_settings`、`get_bookmarks`、
  `get_history`、`get_downloads`、`get_top_sites`
- **导航** —— `navigate`、`new_tab`、`activate`、`go_back`、`go_forward`、`reload`、
  `stop`、`internal_nav`
- **标签** —— `close_tab_cmd`、`toggle_pin`、`move_tab`、`duplicate_tab`、
  `close_others`、`close_to_right`、`reopen_tab_cmd`
- **UI** —— `zoom`、`omnibox_suggest`、`find_query`、`find_move`、`find_close`、
  `chrome_overlay`、`sys_theme_changed`
- **DevTools** —— `toggle_devtools`、`set_devtools_size`、`set_devtools_side`、
  `devtools_drag_end`
- **数据** —— `add_bookmark`、`remove_bookmark`、`update_bookmark`、`delete_history`、
  `clear_history`、`set_settings`、`open_downloads_folder`

Rust 通过 Tauri 事件把状态推回前端：`tabs-state`、`omnibox-state`、
`bookmarks-changed`、`theme-changed`、`sys-theme`、`find-result`、`focus-omnibox`、
`open-findbar`、`dt-drag`。

---

## 6. 构建与运行

前置条件：Rust 工具链，以及 Microsoft Edge WebView2 Runtime（当前 Windows 10/11 已预装）。

```powershell
cd src-tauri
cargo build --release      # 产出 target\release\nanobrowser.exe 与 nanoagent.exe
```

随后双击 **`start.bat`**（优先 release，失败回退 debug），或直接运行可执行文件。

调试构建：

```powershell
cargo build                # target\debug\nanobrowser.exe
```

---

## 7. 运行时数据

所有路径都从可执行文件位置推导，测试时可用环境变量覆盖：

| 内容 | 路径 | 环境变量覆盖 |
| --- | --- | --- |
| 数据根目录 | `<项目>\data` | `NANO_DATA_DIR` |
| 持久化 profile | `<项目>\data\profile.json` | — |
| GUI 的 WebView2 用户数据目录 | `<项目>\data\webview` | — |
| 默认下载目录 | `<项目>\Downloads` | — |
| 自动化 profile | `<项目>\data\automation` | `NANOBROWSER_AGENT_DIR` |
| agent 数据根目录 | `<项目>\data` | `NANOBROWSER_DATA_DIR` |

### `profile.json`

```jsonc
{
  "settings": {
    "searchEngine": "bing",        // bing | baidu | google
    "theme": "system",             // system | light | dark
    "homepage": "nano://newtab",
    "bookmarksBar": true,
    "startup": "session",          // session | ntp
    "downloadsDir": "",            // 为空则用默认下载目录
    "devtoolsSide": "bottom"       // bottom | right
  },
  "bookmarks": [ { "url": "...", "title": "...", "added": 0, "icon": "..." } ],
  "history":   [ { "url": "...", "title": "...", "time": 0 } ],   // 上限 500
  "downloads": [ { "url": "...", "name": "...", "path": "...", "time": 0,
                   "status": "done" } ],                          // 上限 100
  "session":   { "tabs": ["..."], "active": 0 },
  "window":    { "x": 0, "y": 0, "w": 1280, "h": 800, "maximized": false }
}
```

落盘是**原子的**：JSON 先写入 `.profile.tmp` 再改名覆盖 `profile.json`，因此写入过程中崩溃不会损坏 profile（[`profile.rs::save_profile`](src-tauri/src/profile.rs)）。

---

## 8. 浏览器功能

### 标签页

- 拖拽重排：被拖标签跟随光标，兄弟标签做位移动画（FLIP），因此标签条始终预览落点；
- 拖拽尾随的点击被**确定性抑制**（在下一个标签按下时复位，不依赖任务队列时序）；
- 固定/取消固定（固定标签保持在左侧，且不会被拖排序跨过）；
- 中键关闭；每标签右键菜单（固定、复制、关闭、关闭其他、关闭右侧）；
- 溢出处理，保证标签不会被工具栏按钮遮住；
- `closed` 栈 + `Ctrl+Shift+T` 恢复。

### 地址栏

输入由 [`tabs.rs::parse_input`](src-tauri/src/tabs.rs) 解析：

1. 内置页面（`nano://…`）→ 原样使用；
2. “看起来像 URL” → 必要时补 `https://`（主机须为 `localhost`、IP，或含点）；
3. 否则 → 用当前配置的搜索引擎搜索。

建议项来自 `omnibox_suggest`（历史 + 书签匹配）。

### 导航

每个标签独立的 forward/back 栈、`can_back` / `can_fwd`、加载态（刷新按钮会变成停止按钮），全部反映到 `omnibox-state`。

### 书签、历史、下载

- 书签带真实的 `icon` 字段，来自页面上报（favicon 校验规则见 §12）。
- 历史上限 500 条，下载记录上限 100 条。
- 下载采用**防冲突、无竞态**的命名方案：先用 `create_new` 独占占位目标文件，冲突时改名为 `report (1).pdf`、`report (2).pdf`……（[`tabs.rs`](src-tauri/src/tabs.rs) 里的 `unique_path` / `reserve`）。因此两个并发的同 URL 下载绝不会抢占同一路径。

### 内置页面（`nano://`）

| URL | 文件 | 标题 |
| --- | --- | --- |
| `nano://newtab` | `ntp.html` | 新标签页 |
| `nano://history` | `history.html` | 历史记录 |
| `nano://bookmarks` | `bookmarks.html` | 书签管理 |
| `nano://downloads` | `downloads.html` | 下载内容 |
| `nano://settings` | `settings.html` | 设置 |
| `nano://about` | `about.html` | 关于 NanoBrowser |

它们运行在标签 webview 中，但归属**本地**能力（完整 IPC），与远程页面不同。

### 页内查找

注入脚本负责高亮命中；外壳驱动 `find_query` / `find_move` / `find_close`，并显示命中计数（`find-result` 事件）。

### 缩放

`ZOOM_STEPS` 是固定的 15 级阶梯；`Ctrl+±/0` 映射到 `ctrl-zoom-in/out/reset`。

### 主题

`system | light | dark`，结合系统深色标志（`sys-theme` 事件）解析，通过给 document 元素设置 `data-theme` 生效。

### 窗口外壳

自定义标题栏，含最小化/最大化还原/关闭、拖拽区域，以及 F11 全屏。窗口控制按钮的图标在注入时**透传 CSS class**，这正是全屏“还原”图标尺寸正确的原因。

---

## 9. 快捷键

在**页面内**按下的快捷键由注入脚本上报（[`inject.rs`](src-tauri/src/inject.rs)），
在 [`tabs.rs::handle_shortcut`](src-tauri/src/tabs.rs) 处理；**外壳**聚焦时的快捷键
直接由 `shell.js` 处理。

| 快捷键 | 行为 |
| --- | --- |
| `Ctrl+T` | 新建标签页 |
| `Ctrl+W` | 关闭标签页 |
| `Ctrl+Shift+T` | 恢复最近关闭的标签页 |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | 下一个 / 上一个标签页 |
| `Ctrl+1` … `Ctrl+8` | 按序号选择标签页 |
| `Ctrl+9` | 选择最后一个标签页 |
| `Ctrl+L` | 聚焦地址栏 |
| `Ctrl+F` | 页内查找 |
| `Ctrl+D` | 收藏当前页面 |
| `Ctrl+H` | 历史记录 |
| `Ctrl+J` | 下载内容 |
| `Ctrl+Shift+O` | 书签管理 |
| `Alt+←` / `Alt+→` | 后退 / 前进 |
| `Ctrl++` / `Ctrl+-` | 放大 / 缩小 |
| `Ctrl+0` | 重置缩放 |
| `F5` | 刷新（交给引擎） |
| `F11` | 切换全屏 |
| `F12` / `Ctrl+Shift+I` | 切换内嵌 DevTools 面板 |

编辑类按键（`Ctrl+C/V/X/A`）以及 `Ctrl+R` / `Ctrl+P` 刻意透传给引擎。
`Ctrl+Shift+1…9` 同样透传给页面。

---

## 10. 内嵌停靠 DevTools 面板

这是项目中最精巧的部分，单独成节。

### 10.1 为什么需要这个面板

WebView2 自带真正的 DevTools 前端，但只通过 `OpenDevToolsWindow` 暴露，那会弹出**独立**窗口。为了得到 Chrome 那样的内嵌停靠面板，前端被加载进**同一窗口内的第二个子 webview**。

两条替代路线已尝试并否决：

- **`--remote-debugging-port`** —— 不可行：只有外壳 webview 会成为 CDP 目标，页面 webview 无法被官方前端附加。
- **自建精简面板** —— 放弃，改用官方前端以获得完整的 Elements / Console / Network / Sources 体验。

最终路线：**官方前端 + 进程内手写的 loopback websocket 桥**。

### 10.2 URL

```
devtools://devtools/bundled/inspector.html?ws=127.0.0.1:PORT/devtools/page/<会话id>#nanoSide=bottom|right
```

- 前端从 WebView2 自带的 `devtools://devtools/bundled/` 资源加载；
- 它通过本进程提供的**loopback websocket** 说 CDP；
- 停靠侧通过 **fragment**（`#nanoSide=…`）传递，而**不是** query string。

> **关键非直觉约束。** 在 `?ws=` 之后再追加第二个 query key（如 `&nanoSide=right`）会让
> WebView2 的 `devtools://` 处理器直接加载失败，并显示 Chromium 的「无法访问此页面」错误页。
> fragment 由浏览器解析、不会到达该处理器，因此安全。详见 §14。

### 10.3 loopback 桥 —— 以及它为什么不是调试端口

`devtools.rs` 运行一个手写的 websocket **服务端**（SHA-1 握手 + base64 accept key + 文本/关闭帧编解码，不引外部 crate）：

- 绑定一个随机的 **loopback** 端口；
- 只接受本进程为面板自身 URL 铸造的那一个会话 id；
- 每个 CDP 请求都通过 `ICoreWebView2::CallDevToolsProtocolMethod` 转发给某个页面 webview。

扫描 `localhost` 的页面会发现一个监听器，但没有那个 id 就什么也答不出来，而该 id 的生命周期仅限于面板存在期间。进程外部无法附加到任一浏览器。

### 10.4 事件

`Schema.getDomains` 返回 35 个 domain，但**事件数为 0**，因此事件无法动态发现。取而代之的是一张**静态事件名表**，并按前端启用情况**按 domain 懒注册**。个别事件订阅失败时跳过该项，而不是丢掉整个 domain。`Page.startScreencast` 在 WebView2 中未实现；此类调用会作为 CDP error 回给前端，而不是中断桥。

### 10.5 几何计算

所有尺寸在 [`devtools.rs::split`](src-tauri/src/devtools.rs) 中计算，返回页面矩形与面板矩形：

| 常量 | 值 | 含义 |
| --- | --- | --- |
| `MIN_H` | 96 | 底部停靠面板的最小高度 |
| `DEFAULT_H` | 320 | 底部停靠的首次打开高度 |
| `MIN_PAGE_H` | 96 | 留给页面的最小高度 |
| `MIN_W` | 240 | 右侧停靠面板的最小宽度 |
| `DEFAULT_W` | 480 | 右侧停靠的首次打开宽度 |
| `MIN_PAGE_W` | 240 | 留给页面的最小宽度 |

- **底部停靠** —— 页面为 `(content_w, content_h − h)`；面板为 `(0, shell_h + page_h, content_w, h)`。
- **右侧停靠** —— 页面为 `(content_w − w, content_h)`；面板为 `(page_w, shell_h, w, content_h)`。

`clamp_axis` 保证面板与页面都不低于各自最小值。

### 10.6 拖拽条 / 改尺寸

注入的 `GRIP_JS` 在面板**朝向页面的一侧**安装拖拽条：

- 底部停靠 → 沿**顶部**边缘的横条，`cursor: ns-resize`；
- 右侧停靠 → 沿**左侧**边缘的竖条，`cursor: ew-resize`。

由于子 webview 看不到自身边界之外的指针移动，拖拽交由外壳处理：按下鼠标时面板请求 Rust 执行 `set_overlay(true)`（外壳放大到整窗并置顶），随后外壳的全窗 catcher（`#dt-grab`）把 `clientX` / `clientY` 转发给 `set_devtools_size`。Rust 据此计算：

```
底部：devtools_h = clamp(窗口高 − clientY)
右侧：devtools_w = clamp(窗口宽 − clientX)
```

松开鼠标时移除 catcher 并还原 overlay。

### 10.7 面板 ↔ 原生的信号通道

面板是 `devtools://` 文档，其中 Tauri IPC 与 WebView2 的 host object（`window.chrome.webview`）**都不存在**。因此面板通过「弹窗到一个不可能存在的主机」与 Rust 通信：

```js
window.open('https://nano-dt.invalid/<action>', '_blank');
```

`on_new_window` 会看到该请求，从路径读出动作并返回 `NewWindowResponse::Deny`。不会有任何导航发生。

### 10.8 生命周期

- `F12`、`Ctrl+Shift+I` 与主菜单都调用 `toggle_devtools` → [`devtools::toggle_for`](src-tauri/src/devtools.rs)。
- `activate` / `create_tab` 调用 `devtools::retarget`，让面板跟随活动标签。
- `close_tab` 调用 `devtools::forget`，清理与死标签绑定的协议状态。
- 在**面板内**按 `F12` 会关闭面板。
- 停靠侧持久化在 `settings.devtoolsSide`；切换停靠侧会重新加载面板，使拖拽条落在正确的边缘。
- 所有 webview —— 外壳、页面标签**以及**面板 —— 都以 `.devtools(false)` 创建，这关掉了原生 DevTools 弹窗，同时保留 `CallDevToolsProtocolMethod` 的完整能力。

---

## 11. 无头自动化浏览器 `nanoagent`

`nanoagent` 是**同一个**浏览器引擎，装在一个**从不显示**的窗口里，由 AI 工具驱动。

设计要点：

- 窗口以 `visible(false)`、`skip_taskbar(true)`、`decorations(false)` 构建，并使用**独立**的 WebView2 用户数据目录（`data\automation`），与 GUI 的 profile 完全隔离 —— 自动化的登录态与人的 Cookie 互不污染。
- 控制走**进程内** CDP 桥：不开放调试端口。
- 一次性调用每次启动全新浏览器；MCP 模式在多次工具调用间保持一个常驻页面。

### 11.1 一次性 CLI

```
nanoagent --mcp                              通过 stdio 说 MCP（长驻会话）
nanoagent <command> [args...]                执行一个动作后退出

navigate <url>                               打开 URL，输出 {url,title}
read <url> [selector] [--html]               输出页面文本（或 HTML）
shot <url> <out.png>                         保存 PNG 截图
cookies <url>                                列出该 profile 在该页面的 Cookie
traffic <url> [limit]                        输出记录的请求（密钥已脱敏）
eval <url> <js>                              执行 JavaScript 并输出结果值
click <url> <selector>                       用可信鼠标事件点击
fill <url> <selector> <value>                向输入框输入
wait <url> <selector> [timeout_ms]           等待元素出现
```

### 11.2 基于 stdio 的 MCP server

`--mcp` 在 stdin/stdout 上说 JSON-RPC 2.0，支持 `tools/list` 与 `tools/call`
（`serverInfo.name = "nanobrowser-agent"`）。工具：

| 工具 | 说明 |
| --- | --- |
| `browser_navigate` | 打开 URL，返回 `{url,title}` |
| `browser_read` | 页面文本或 HTML，可选 selector |
| `browser_click` | 合成**可信**鼠标事件 |
| `browser_fill` | 以 `Input.insertText` 输入到 selector |
| `browser_wait_for` | 轮询直到 selector 出现 |
| `browser_screenshot` | PNG 字节 |
| `browser_cookies` | 自动化 profile 的 Cookie |
| `browser_traffic` | 记录的请求，已脱敏 |
| `browser_eval` | 执行 JS 并返回其值 |

### 11.3 实现要点

- **可信输入。** `click` 用 `Input.dispatchMouseEvent`，`fill` 用 `Input.insertText`，因此页面看到的是真实用户输入，而不是脚本化的 DOM 写入。
- **截图** 走 `Page.startScreencast`：隐藏窗口仍会合成画面，但 WebView2 未实现 `Page.captureScreenshot`。若隐藏窗口一旦停止合成，已记录的兜底方案是把窗口移到屏幕外，而不是设 `visible(false)`。
- **绝不阻塞 UI 线程** 等待 CDP 回复（会死锁）—— 回复写入共享状态，由 worker 线程在条件变量上等待（见 §13）。
- **就绪判定。** worker 在发 CDP 前以 600 × 50 ms 轮询 `is_ready()`，因为 webview 需要有运行中的事件循环才会应答。

---

## 12. 安全模型

### 12.1 页面事件来源校验

每个标签 webview 的注入脚本都嵌入了由 Rust 铸造的**不透明随机 token**（`new_token()`，24 个十六进制字符）。页面无法伪造另一个标签的事件，因为它不知道那个标签的 token；Rust 用 `tab_id_by_token` 反查发送者，**绝不**信任页面提交的标签 id。

### 12.2 favicon 处理

favicon 由页面上报、由可信外壳显示，因此该值按敌意内容对待：插入前先做 HTML 转义，并用 `<img onerror>` 回退到内置的地球图标。页面因此永远无法把自选的 URL 注入外壳。

### 12.3 能力分级（按 webview 授权）

Tauri capability 按 webview label 分级（见 `capabilities/`）：

| Capability | 适用 | 授予 |
| --- | --- | --- |
| `shell` | `"shell"` | `core:default` + 窗口控制/查询权限 |
| `tab-local` | 运行内置页面的 `tab-*` | `core:default` + `core:event:allow-emit` → 完整应用 IPC |
| `tab-remote` | 外部 URL 的 `tab-*` | **仅** `core:event:allow-emit` —— 远程页面只能*上报 UI 事件*，绝不能调用命令 |

远程能力必须覆盖**所有端口**（`https://*:*`、`http://*:*`，以及 `https://*/*`）。若某非默认端口的站点未被匹配，注入脚本会**静默失效**（快捷键、悬停状态、查找高亮、favicon 上报会同时全部异常）。

### 12.4 不使用调试端口

两个二进制都不传 `--remote-debugging-port`。所有 CDP 流量都在进程内通过 `ICoreWebView2::CallDevToolsProtocolMethod` 完成。GUI 的 DevTools websocket 桥绑定 loopback，并由每会话 id 把关（§10.3）。

### 12.5 网络报文脱敏

agent 的报文捕获在**捕获时**就脱敏，因此密钥根本不会进入日志缓冲区。脱敏不可关闭：

- 18 个敏感键子串（`password, passwd, pwd, pass, token, secret, apikey, api_key,
  api-key, auth, cookie, session, csrf, credit, card, cvv, ssn, otp`）→ JSON body 与
  header 映射中统一替换为 `***`；
- `Authorization` / `Cookie` 头以及表单 body 受同一规则覆盖；
- body 会截断（`MAX_BODY = 4096`、`MAX_VALUE = 512`），缓冲区上限 `MAX_ENTRIES = 500`；
- 日志**仅存内存** —— 不落盘。

重定向复用同一 CDP `requestId` 会覆盖原始记录，因此条目按**重定向链派生的唯一 key** 存储，而不是原始 id。

### 12.6 profile 隔离

自动化浏览器使用自己的 WebView2 数据目录，与 GUI 的不同。Cookie 与登录态在两个方向都不越界。

---

## 13. 进程内 CDP 内核

`cdp.rs` 由两个二进制共用。`ICoreWebView2::CallDevToolsProtocolMethod` 的两个特性塑造了整体设计：

1. **没有 `{"id": …}` 外层信封。** 完成回调只拿到方法自身的结果对象，因此回复靠每个调用处理器中的**序号**来配对。
2. **回调运行在 WebView2（UI）线程。** 在那里等待会死锁。因此每次调用都是**fire-and-forget**，而*调用方* —— 永远是 worker 线程，绝不是 UI 线程 —— 在 `Condvar` 上等待回复落地。

在此之上有两种调用风格：

| API | 风格 | 使用方 |
| --- | --- | --- |
| `call` / `call_raw` / `call_within` | 阻塞等待回复（默认 30 秒） | 自动化 agent |
| `send` | 把回复交给回调 | DevTools websocket 桥（绝不能阻塞；它在自己的 socket 线程上应答） |

其它细节：

- `on_event(event, cb)` 通过 `GetDevToolsProtocolEventReceiver` 订阅；注册后订阅对象归 WebView2 所有，回调与 webview 同生命周期。
- `with_webview` 必须在**非**主线程调用。
- 空回复（无返回值的方法）归一为 `null`。
- `Cdp::webview()` 既可解析子 webview（`"tab-<id>"`、`"devtools"`），也可解析 `WebviewWindow`（`"nanoagent"`），因此一套实现同时服务两个二进制。

---

## 14. 设计权衡与非直觉约束

这份清单是本代码库已经踩过并付出代价的坑。多数反直觉，改动相关区域前值得一读。

**布局 / z-order**

1. 每个标签是独立 HWND 时，在外壳**之后**创建的页面 webview 会压在外壳之上，裁掉菜单/建议。方案：菜单打开期间把外壳放大到整窗并置顶，关闭时还原，且在标签新建/激活/关闭后重新置顶。
2. 子 webview 看不到自身边界外的指针移动，因此面板改尺寸必须由外壳 webview 的全窗 catcher 转发光标。
3. 窗口控制图标由 JS 注入；SVG 的 CSS **class 必须透传**，否则按 class 定尺寸的字形（如全屏还原图标）会以默认过大尺寸渲染。

**并发 / 下载**

4. 按索引并发关闭标签会误删相邻标签；需在持锁状态下重新查找标签再执行关闭。
5. 同 URL 并发下载若仅按 URL 匹配会导致条目覆盖；需用原子 `create_new` 占位机制。

**CDP**

6. 在 UI 线程等待 CDP 回复（隐藏窗口尤其）会死锁。应使用回调写入共享状态，由 worker 线程轮询/等待。
7. 重定向复用同一 `requestId` 会覆盖原始请求记录并丢失其 body；需按重定向链派生唯一 key。
8. `--remote-debugging-port` 是死路：只有外壳 webview 会成为 CDP 目标。
9. `Schema.getDomains` 只列出 domain，不返回事件名；必须用静态事件表 + 按 domain 懒注册。
10. `AreDevToolsEnabled = false`（`.devtools(false)`）**不影响** `CallDevToolsProtocolMethod`，因此可以关掉原生弹窗同时保留完整 CDP。

**内嵌 DevTools**

11. `devtools://` 是特权页面，`window.chrome.webview` 与 Tauri IPC 都不可用；`window.open` + `on_new_window → Deny` 是唯一的信号通道。
12. 在 `?ws=` 之后加**第二个 query key** 会让 `devtools://` 整体加载失败 —— 附加信息必须放进 **fragment**。验证此类问题不能只看 `webview.url()`（错误页仍返回请求的 URL）；可靠信号是 websocket 桥是否收到客户端、以及注册了多少 domain。
13. 拖拽条位于朝向页面的边缘，光标随停靠轴变化；拖拽换算为 `panel_h = 窗口高 − clientY`（底部）或 `panel_w = 窗口宽 − clientX`（右侧）。

**能力声明**

14. 能力声明未匹配的非默认端口站点会一次性丢失**全部**注入功能（快捷键、悬停状态、查找高亮、favicon）。

---

## 15. 已知限制

- **历史记录与新标签页「常用网站」仍按 `scheme://host/favicon.ico` 猜测 favicon**，而书签存的是页面上报的真实图标。统一到书签那套逻辑是已知的后续项。
- 内嵌面板仅支持**底部**与**右侧**停靠（无左侧 / 独立窗口）。
- 仅限 Windows：整套栈建立在 WebView2 COM 互操作之上。
- 除 `tauri.conf.json` 中声明的 `nsis` 打包目标外，仓库暂无打包/发布流水线。

---

## 16. 许可证

本仓库未指定许可证。分发前请补充 `LICENSE` 文件。
