# TODO

Remaining items are platform work targeted at 1.1+ (Windows/Linux). The
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

## Windows: status

- DONE: auto-grant of WebView2 microphone/camera `PermissionRequested`
  (`grant_media_permissions` in `src-tauri/src/lib.rs`, `cfg(windows)`).
- DONE: compiles & links on Windows (ARM64, `aarch64-pc-windows-msvc`) via
  `npm run dev`.
- STILL TODO: confirm mic/camera actually work in a call on Windows at runtime.
  Requires the OS toggle *Settings → Privacy → Microphone → "Let desktop apps
  access your microphone"* enabled. No manifest/entitlement needed.
