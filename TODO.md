# TODO

Most remaining items are platform work targeted at 1.1+ (Windows/Linux). The
settings window and in-app settings button shipped in 1.0.

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

## Hide Dock/taskbar icon when the main window isn't open

Today closing to tray (or "Start in Tray") hides the window but the app keeps
a Dock icon (macOS) / taskbar presence (Windows) the whole time. It should
disappear whenever the main window is hidden and reappear when it's shown —
tray-only should mean tray-only.

- **macOS:** switch `tauri::ActivationPolicy` via `app.set_activation_policy(...)`
  — `Accessory` while the window is hidden (no Dock icon, no Cmd-Tab entry),
  `Regular` when it's shown. Needs to run at every place the window is
  hidden/shown, not just once at startup:
  - the `CloseRequested` handler in `on_window_event` (`src-tauri/src/lib.rs`)
    that currently just calls `window.hide()`.
  - the tray "show" menu handler and the tray left-click toggle in
    `build_tray`, both of which call `window.show()` / `window.hide()`.
  - startup: if `settings.start_in_tray` is true the window is built
    invisible, so the policy should start as `Accessory` too, set right after
    `build_tray(app)?`.
- **Windows:** hidden windows are usually already absent from the taskbar, so
  this may already work there — verify first. If not, `WebviewWindowBuilder`
  has `.skip_taskbar(bool)` at creation, and `window.set_skip_taskbar(bool)`
  can presumably toggle it at the same hide/show call sites as macOS. Confirm
  the exact runtime behavior on a real Windows machine before assuming either
  way.
- Linux: not applicable in the same way (no dock/taskbar concept is universal
  across DEs); lower priority, revisit if it comes up.

## Windows: status

- DONE: auto-grant of WebView2 microphone/camera `PermissionRequested`
  (`grant_media_permissions` in `src-tauri/src/lib.rs`, `cfg(windows)`).
- DONE: compiles & links on Windows (ARM64, `aarch64-pc-windows-msvc`) via
  `npm run dev`.
- STILL TODO: confirm mic/camera actually work in a call on Windows at runtime.
  Requires the OS toggle *Settings → Privacy → Microphone → "Let desktop apps
  access your microphone"* enabled. No manifest/entitlement needed.
