# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.0.0] - 2026-07-06

### Added
- Per-architecture build scripts: `scripts/build.sh` (macOS Apple Silicon;
  Linux x86_64/aarch64, cross-compiling as needed) and `scripts/build.ps1`
  (Windows x64/ARM64, cross-compiling via the MSVC toolchain). Each targets
  the architectures relevant to its OS — macOS ships Apple Silicon only,
  Windows and Linux ship both x86_64 and aarch64.
- `scripts/setup-dev.sh`/`.ps1` now also install what those build scripts
  need: the extra Rust targets, both MSVC VC++ Tools toolsets on Windows, and
  (best-effort, apt-based distros only) the Linux cross C toolchain +
  target-arch WebKitGTK dev libraries.

### Fixed
- IPC from the Messages page itself had **never worked**: every `invoke` from
  the main window was rejected with `"not allowed. Plugin not found"`, so the
  tray unread badge and the external-link handoff were silently dead (the root
  cause the earlier click/link diagnostics were chasing). Two things were
  required:
  - the app's own commands must be declared in the ACL manifest
    (`tauri_build::AppManifest::commands` in `build.rs`) and granted as
    `allow-*` permissions in `capabilities/default.json`;
  - `https://messages.google.com/*` must be listed as a `remote` origin there
    — Tauri denies IPC from remote domains by default (note: an origin without
    the `/*` path wildcard does not match).
- Debug builds now log command invocations (`[ipc] …`) to stderr so a broken
  IPC path is visible in `npm run dev` instead of failing silently.

### Added
- Dedicated **Settings window** (local `frontend/settings.html`, opened from the
  tray menu or the new floating gear button injected into the Messages web UI):
  - **Launch at Login** and **Start in Tray** (moved out of the tray menu).
  - **Show notifications** toggle — the injected `Notification` wrapper now
    honors it (an inert stand-in is returned when off), applied live on save.
  - **Theme** override (System / Light / Dark) — applied live to all windows
    and on startup.
  - **Start URL** override with http(s) validation.
- Main window **size memory**: the window's size is saved when it closes to
  tray or the app quits, and restored on next launch.

### Changed
- Tray menu simplified to **Show / Settings… / Quit**; the checkable
  "Launch at Login" / "Start in Tray" items were replaced by the settings
  window so their check state can't drift from `settings.json`.
- `show_notifications` now defaults to **on** for fresh installs and for
  `settings.json` files written before the field existed.

### Removed
- Temporary click/link diagnostics from the injected script and
  `open_external` (added while tracking down the Account-button behavior).
- Invalid `commands` block in `capabilities/default.json` (not part of the
  capability schema; app commands aren't ACL-gated and it was silently
  ignored).

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
- Tray icon is now visible on Windows/Linux dark taskbars: those platforms don't
  support macOS-style template icons, so the black silhouette was invisible on a
  dark taskbar. They now use a colored bubble glyph; macOS keeps the monochrome
  template that tints for light/dark menu bars.
- `scripts/setup-dev.ps1` now detects the actual MSVC C++ toolset via `vswhere`
  and adds the "Desktop development with C++" workload when missing, instead of
  skipping it whenever the Build Tools package merely exists. This was the cause
  of `error: linker 'link.exe' not found` on Windows.

### Changed
- Setup scripts now end with a verification pass: print tool versions and, on
  Windows, confirm the MSVC linker matching the active Rust host triple is
  present (ARM64 vs x64 are separate components) — surfacing a missing toolset
  before the first build instead of as a cryptic linker error.

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
