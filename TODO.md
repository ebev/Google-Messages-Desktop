# TODO

## Linux: call audio/video (mic + camera) support

Calls won't work on Linux out of the box. WebKitGTK gates `getUserMedia` behind
the `enable-media-stream` setting, which **defaults to off**, and `wry` never
enables it — so `navigator.mediaDevices.getUserMedia` is unavailable and the Fi
calls app reports no devices.

**Work required** (all `cfg(target_os = "linux")`-gated so it doesn't touch the
macOS/Windows builds):

- Add `webkit2gtk` as a Linux-only dependency in `src-tauri/Cargo.toml`, version
  matched to the one `wry` pulls in.
- In `setup()`, reach the underlying webview via Tauri's `with_webview()` and:
  - `settings.set_enable_media_stream(true)`
  - `settings.set_enable_webrtc(true)` (if available in the bound version)
  - connect the `permission-request` signal and `.allow()` user-media requests
    (mirrors what `wry` already does for us on macOS via `WKPermissionDecision::Grant`).
- Runtime prerequisite on the machine: working PipeWire/PulseAudio stack.

**Caveat:** must be compiled and verified on an actual Linux box — cannot be
tested from macOS.

## Proper settings window

Current settings ("Launch at Login", "Start in Tray") live as checkable items in
the tray menu — functional but minimal. Replace/supplement with a real settings
window:

- A dedicated webview window (its own label, e.g. `settings`) loading a local
  HTML/JS page from `frontend/` (not the external Messages URL).
- Surface the existing settings plus room to grow (e.g. start URL, window size
  memory, notification toggles, theme override).
- Bridge to Rust via `#[tauri::command]`s for get/set; persist through the
  existing `Settings` struct + `settings.json` (extend as needed).
- Add a "Settings…" item to the tray menu to open/focus the window; remember to
  scope close-to-tray to the `main` label only (the settings window should close
  normally) — same pattern already used in `on_window_event`.
- Give the `settings` window its own capability entry (or widen `windows` in
  `capabilities/default.json`) so its commands are permitted.

## Windows: status

- DONE: auto-grant of WebView2 microphone/camera `PermissionRequested`
  (`grant_media_permissions` in `src-tauri/src/lib.rs`, `cfg(windows)`).
- DONE: compiles & links on Windows (ARM64, `aarch64-pc-windows-msvc`) via
  `npm run dev`.
- STILL TODO: confirm mic/camera actually work in a call on Windows at runtime.
  Requires the OS toggle *Settings → Privacy → Microphone → "Let desktop apps
  access your microphone"* enabled. No manifest/entitlement needed.

## Settings button in Messages web UI

The settings page is reachable from the tray menu. Future enhancement: inject a
small settings button into the Messages web UI via the existing initialization
script so users can open settings without right-clicking the tray icon. Use a
floating gear in the top-right corner to avoid depending on Google's DOM
structure, or attach to a stable app header if one can be identified.
