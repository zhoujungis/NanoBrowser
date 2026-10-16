@echo off
rem NanoBrowser 启动器：优先使用 release 版本
set EXE_RELEASE=%~dp0src-tauri\target\release\nanobrowser.exe
set EXE_DEBUG=%~dp0src-tauri\target\debug\nanobrowser.exe
if exist "%EXE_RELEASE%" (
  start "" "%EXE_RELEASE%"
) else if exist "%EXE_DEBUG%" (
  start "" "%EXE_DEBUG%"
) else (
  echo [NanoBrowser] 尚未构建，请先运行: cd src-tauri ^&^& cargo build --release
  pause
)
