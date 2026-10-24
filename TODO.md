# TODO

Remaining platform verification items (Windows/Linux):

## Linux: runtime verification

- DONE: `webkit2gtk` (`=2.0.2`, matching `wry` 0.55) added as a Linux-only
  dependency in `src-tauri/Cargo.toml`; `grant_media_permissions` in
  `src-tauri/src/lib.rs` (`cfg(target_os = "linux")`) enables
  `media-stream` + `webrtc` settings and auto-allows `UserMediaPermissionRequest`.
- STILL TODO: compile and verify mic/camera in a live call on a Linux machine
  with a working PipeWire/PulseAudio stack.

## Windows: runtime verification

- DONE: auto-grant of WebView2 microphone/camera `PermissionRequested`
  (`grant_media_permissions` in `src-tauri/src/lib.rs`, `cfg(windows)`).
- DONE: compiles & links on Windows (ARM64, `aarch64-pc-windows-msvc`) via
  `npm run dev`.
- STILL TODO: confirm mic/camera actually work in a call on Windows at runtime
  (requires *Settings → Privacy → Microphone → "Let desktop apps access your
  microphone"* enabled) and verify taskbar behavior when the window is hidden
  to the tray.
