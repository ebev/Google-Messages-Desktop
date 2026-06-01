# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.1] - 2026-05-31

### Added
- External links (e.g. the Google Fi account button → `fi.google.com/account`)
  now open in the OS default browser instead of being silently dropped. Links to
  `messages.google.com` still navigate in-window; everything else is handed off
  via `tauri-plugin-opener`.

### Changed
- Replaced the deprecated `tauri-plugin-shell` open path with
  `tauri-plugin-opener`.

### Fixed
- `scripts/setup-dev.ps1` now detects the actual MSVC C++ toolset via `vswhere`
  and adds the "Desktop development with C++" workload when missing, instead of
  skipping it whenever the Build Tools package merely exists. This was the cause
  of `error: linker 'link.exe' not found` on Windows.

## [0.4.0] - 2026-05-31

### Added
- Tray settings as checkable menu items:
  - **Launch at Login** — toggles the OS login item via `tauri-plugin-autostart`.
  - **Start in Tray** — launches with the window hidden; persisted to
    `settings.json` in the app config dir.
- `Settings` struct + JSON load/save helpers for user preferences.
- Project docs: `README.md`, `CHANGELOG.md`, and `TODO.md` (tracks the Linux
  audio work, Windows build verification, and a future proper settings window).
- Developer environment setup scripts: `scripts/setup-dev.sh` (macOS/Linux) and
  `scripts/setup-dev.ps1` (Windows) — install Rust, Node.js, and the platform
  build prerequisites, then run `npm install`.

### Changed
- Load `messages.google.com/web/` directly (the canonical entry) instead of the
  `/web/authentication` pairing sub-route — when already paired this lands on the
  cached conversation list and skips an extra redirect on startup.
- Real app icon (blue chat bubble) replacing the blank blue square, regenerated
  for all platforms. The menu-bar tray now uses a dedicated transparent bubble
  glyph so `icon_as_template` renders a proper silhouette instead of a solid
  square. Removed the stale, unused root `icons/` directory.

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
