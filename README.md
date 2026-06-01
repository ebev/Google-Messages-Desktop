# Google Messages for Desktop

A lightweight, cross-platform desktop wrapper for [Google Messages](https://messages.google.com)
built with [Tauri](https://tauri.app) — it uses the OS-native webview instead of
bundling Chromium, so the app stays small.

## Features

- Native window wrapping `messages.google.com`
- System tray icon — left-click toggles the window, right-click for the menu
- Tray settings: **Launch at Login** and **Start in Tray** (start hidden)
- Closes to tray instead of quitting; single-instance enforced
- System notifications, even when the window is hidden
- Follows the system light/dark theme
- Unread-count surfaced via the tray tooltip
- Google Fi **Calls** / **Voicemail** tabs work in-window (handled around their
  `target="_blank"` links), with a Safari user-agent so they aren't rejected as
  an unsupported browser
- Microphone/camera access for calls (macOS via embedded `Info.plist`; Windows
  via WebView2 permission auto-grant)

## Prerequisites

- [Rust](https://rustup.rs)
- [Node.js](https://nodejs.org)
- Platform build tools (Xcode CLT on macOS, C++ Build Tools + WebView2 on
  Windows, WebKitGTK + build deps on Linux)

### Automated setup

One-shot scripts install everything above and run `npm install` (idempotent —
safe to re-run):

```bash
# macOS / Linux
./scripts/setup-dev.sh
```

```powershell
# Windows (run from an elevated PowerShell)
powershell -ExecutionPolicy Bypass -File .\scripts\setup-dev.ps1
```

## Development

```bash
npm install   # if you didn't use the setup script
npm run dev
```

## Build

```bash
npm run build
```

The bundled app is written to `src-tauri/target/release/bundle/`.

## Platform notes

- **macOS / Linux** present a Safari user-agent; **Windows** keeps WebView2's
  native Edge user-agent.
- **Linux** call audio needs additional work — see [TODO.md](TODO.md).
