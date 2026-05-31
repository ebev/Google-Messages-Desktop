# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] - 2026-05-31

### Added
- Tray settings as checkable menu items:
  - **Launch at Login** — toggles the OS login item via `tauri-plugin-autostart`.
  - **Start in Tray** — launches with the window hidden; persisted to
    `settings.json` in the app config dir.
- `Settings` struct + JSON load/save helpers for user preferences.
- Project docs: `README.md`, `CHANGELOG.md`, and `TODO.md` (tracks the Linux
  audio work, Windows build verification, and a future proper settings window).

### Notes
- Cold-start "reloads everything" is the Messages SPA re-initializing and
  re-syncing with the phone, not a missing cache — the HTTP `NetworkCache` and
  service-worker registration already persist. Close-to-tray keeps the process
  warm so reopening is instant; "Start in Tray" + "Launch at Login" minimize
  cold starts in daily use.

## [0.3.0] - 2026-05-31

### Added
- Microphone/camera support for calls:
  - **macOS:** `NSMicrophoneUsageDescription` / `NSCameraUsageDescription` in
    `Info.plist`, plus the plist embedded directly into the binary
    (`__TEXT,__info_plist` via `build.rs`) so capture also works under
    `tauri dev` / the unbundled binary. `Entitlements.plist` added for
    hardened-runtime release signing.
  - **Windows:** auto-grant of WebView2 microphone/camera `PermissionRequested`
    events so calls aren't prompted every session (`cfg(windows)`).

### Notes
- WebKit (macOS/Linux) already auto-grants the page's media request via `wry`;
  the blocker was the OS-level usage descriptions.
- Linux call audio still requires work — see `TODO.md`.

## [0.2.0] - 2026-05-31

### Added
- Google Fi **Calls** and **Voicemail** support: their `target="_blank"` links
  (and `window.open`) to `messages.google.com` are intercepted and navigated
  in the same window, so they behave as tabs instead of being dropped.

### Changed
- Present a Safari user-agent on macOS/Linux (the embedded WebKit UA omits the
  `Version/… Safari/…` token, which made the Fi calls app reject it as an
  unsupported browser). Windows keeps WebView2's native, always-current Edge UA.

## [0.1.0] - 2026-05-31

### Added
- Initial Tauri-based, non-Electron desktop wrapper for Google Messages.
- Native window loading `messages.google.com`.
- System tray icon: left-click toggles the window, right-click menu with
  **Show / Quit**.
- Close-to-tray instead of quitting; single-instance enforcement.
- System notifications, even when the window is hidden.
- Follows the system light/dark theme.
- Unread-count surfaced via the tray tooltip.

### Fixed
- Startup crash (`SIGABRT` in `did_finish_launching`) caused by a duplicate
  `main` window declared in both `tauri.conf.json` and code.
- Two tray icons / unresponsive right-click, caused by the tray being declared
  in both `tauri.conf.json` and code.
