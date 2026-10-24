# Google Messages for Desktop

A lightweight, cross-platform desktop wrapper for [Google Messages](https://messages.google.com)
built with [Tauri](https://tauri.app) — it uses the OS-native webview instead of
bundling Chromium, so the app stays small.

## Features

- Native window wrapping `messages.google.com`
- System tray icon — left-click toggles the window, right-click for the menu
- Settings window (tray menu → **Settings…**, or the floating gear in the app):
  **Launch at Login**, **Start in Tray**, notifications toggle, theme override
  (System / Light / Dark), and a custom start URL
- Closes to tray instead of quitting (hiding the macOS Dock icon while in the
  tray); single-instance enforced; window size remembered across launches
- System notifications, even when the window is hidden
- Follows the system light/dark theme (or force light/dark in Settings)
- Unread-count surfaced via the tray tooltip
- Google Fi **Calls** / **Voicemail** tabs work in-window (handled around their
  `target="_blank"` links), with a Safari user-agent so they aren't rejected as
  an unsupported browser
- Microphone/camera access for calls (macOS via embedded `Info.plist`; Windows
  via WebView2 permission auto-grant; Linux via WebKitGTK media-stream/WebRTC
  and permission auto-grant)

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

For a native (host-architecture) build:

```bash
npm run build
```

The bundled app is written to `src-tauri/target/release/bundle/`.

### Cross-architecture builds

`scripts/build.sh` (macOS/Linux) and `scripts/build.ps1` (Windows) build for a
specific architecture, cross-compiling where needed. Run `scripts/setup-dev.*`
first — it installs the extra Rust targets and, on Linux, the cross toolchain.
Each script only builds for the OS it runs on (there's no cross-OS build; e.g.
Windows `.msi`/`.exe` bundles are built on Windows):

```bash
# macOS (Apple Silicon only — no x86_64-apple-darwin build)
./scripts/build.sh

# Linux — defaults to the host architecture; pass a target to cross-compile
./scripts/build.sh                              # native
./scripts/build.sh x86_64-unknown-linux-gnu
./scripts/build.sh aarch64-unknown-linux-gnu
```

```powershell
# Windows — defaults to the host architecture
.\scripts\build.ps1
.\scripts\build.ps1 -Target x64
.\scripts\build.ps1 -Target arm64
```

Bundles land in `src-tauri/target/<target-triple>/release/bundle/`.

## Platform notes

- **macOS / Linux** present a Safari user-agent; **Windows** keeps WebView2's
  native Edge user-agent.
- **Linux** calls require a working PipeWire/PulseAudio stack at runtime.
