# TODO

## v1.3 — Background Listener, Notification Reliability & Session Takeover Protection

### 1. Background Listener & Anti-Throttling Keep-Alive — DONE
- [x] **Disable WebView Background Throttling:** Configure `BackgroundThrottlingPolicy::Disabled` on the main window and pass Chromium `--disable-background-timer-throttling --disable-renderer-backgrounding --disable-backgrounding-occluded-windows` flags on Windows WebView2 so background timers and WebSockets stay active when hidden in the tray.
- [x] **macOS App Nap Control:** Hold an `NSProcessInfo` `UserInitiatedAllowingIdleSystemSleep` activity assertion while background keep-alive is active, and release it automatically if the user disables keep-alive or yields the session to another computer.
- [x] **Smart Rust Heartbeat & Watchdog (`start_background_listener`):** Run a 30-second native background heartbeat thread that detects OS sleep/wake jumps, pings the webview (`window.__gmHeartbeat`), inspects connection banners (`"Trying to reach your phone"`, `"No internet connection"`), and performs a silent auto-reload (with a 120s cooldown) only when the page is disconnected or frozen.
- [x] **"Keep connected in background" Setting:** Add `background_keep_alive` (default `true`) to `settings.json` and the Settings UI (`#backgroundKeepAlive`).

### 2. Native Notification Reliability & Unread Fallback — DONE
- [x] **Replace `tauri-plugin-notification` with Native Bridge:** Remove `tauri-plugin-notification` (whose `init-iife.js` overwrote `window.Notification` with a non-EventTarget stub and dropped desktop click handlers) and implement `send_native_notification` using `NSUserNotificationCenter` + a persistent `GmNotificationDelegate` on macOS, `notify-rust` action handlers on Linux, and WebView2's native `Notification` with `COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS` auto-granted on Windows.
- [x] **Full `EventTarget` `GMNotification` Wrapper:** Expose a spec-compliant `window.Notification` wrapper supporting `.onclick`, `.addEventListener('click', ...)`, and `.close()`, plus automatic main-window restore/focus on click.
- [x] **Unread-Count Increase Fallback Notification:** If the unread badge count increases while the window is unfocused/hidden in the tray and Google Messages did not fire a Web `Notification` within 1.5s, synthesize a fallback native notification so background messages are never missed.

### 3. "Google Messages is open on another computer" Protection — DONE
- [x] **Other-Computer Session Detection:** Detect the `"Google Messages is open on another computer"` state (`"Use here"` prompt) in `INIT_SCRIPT`, immediately pause background auto-refresh, and notify the Rust backend (`report_heartbeat` with `other_session: true`).
- [x] **Native Takeover Alert & Modal Prompt:** Fire a native OS notification when another computer takes over the session; clicking the notification (or clicking `"Use here"` in the macOS notification banner) restores the main window and opens an in-app confirmation modal (`"Move session here"` vs. `"No, let app sleep"`).
- [x] **Graceful OS Sleep When Ignored or Declined:** If the user ignores the notification or clicks `"No, let app sleep"`, mark the session yielded (`set_session_yielded(true)`), keep background refresh paused, and release the OS App Nap / anti-sleep assertion so the OS can sleep the app normally until the user returns and resumes.

---

## v1.2 — Native Desktop Polish (Cross-Platform: Windows / macOS / Linux)

### Batch 1: Unread Badges on Taskbar, Dock & System Tray — DONE
- [x] **Windows Taskbar Overlay:** Render a 32×32 red unread badge with bitmap digits (`1`–`99`, `99+`) via `window.set_overlay_icon()` when `count > 0` and clear when `0`.
- [x] **macOS Dock & Linux Launcher Badge:** Set `window.set_badge_count()` and re-apply the cached unread count when restoring the window from tray-only (`Accessory`) mode.
- [x] **System Tray Unread Indicator:** Dynamically composite a top-right unread dot with a cutout halo onto the tray icon (`#EA4335` red on Windows/Linux; template silhouette + menu-bar count title on macOS).

### Batch 2: Notification Click → Restore & Focus Window — DONE
- [x] **Focus Command (`focus_main_window`):** Expose a Tauri command (registered in `build.rs` and `capabilities/default.json`) that calls `unminimize()`, `show()`, and `set_focus()` on the main window (plus restores `ActivationPolicy::Regular` on macOS).
- [x] **Hook `Notification` Click Events:** In `INIT_SCRIPT`, attach a `click` listener to every constructed `Notification` instance so clicking a desktop notification while minimized or hidden in the tray brings the app to the front.

### Batch 3: Full Window State Persistence & Cold-Start Polish — DONE
- [x] **Remember Position (`x`, `y`) & Maximized State:** Extend `WindowSize` in `settings.json` to persist `x`, `y`, and `maximized` alongside `width` and `height`.
- [x] **Multi-Monitor Bounds Validation:** On startup, verify the saved `(x, y)` intersects an active monitor in `app.available_monitors()` before restoring position (falling back to `.center()` if a monitor was unplugged), and restore `.maximized(true)` if it was maximized.
- [x] **Taskbar Cleanup When Hidden:** Call `window.set_skip_taskbar(true)` when hiding to tray (and on startup when `start_in_tray` is enabled) and `window.set_skip_taskbar(false)` when showing on Windows/Linux.
- [x] **Eliminate Dark-Mode White Flash on Cold Start:** Set the native window `.background_color()` before loading `messages.google.com` (`#131314` for Dark, `#ffffff` for Light) so WebView2/WebKit doesn't flash white while initializing.

### Batch 4: Windows WebView2 & Cross-Platform "Web-Feel" Cleanup — DONE
- [x] **Windows WebView2 Polish:** Enable Windows 11 `ScrollBarStyle::FluentOverlay` and suppress browser-only accelerator keys with the injected `keydown` handler in `init_script.js` (Edge `Ctrl+P` print, `F3` find bar, `F7` caret browsing prompt, `Ctrl+J` downloads popup). `browser_accelerator_keys(false)` is not used.
- [x] **Native New-Window & Link Guard (`on_new_window` + Shadow-DOM link routing):** Intercept native new-window requests across all frames (including cross-origin iframes) and route non-Messages links (including links without `target="_blank"` or middle-clicks) to the OS default browser.
- [x] **CSS / DOM Web-Feel Cleanup:** Inject styles/handlers in `INIT_SCRIPT` to disable viewport rubber-band overscroll (`overscroll-behavior: none`) and prevent ghost-dragging of UI images/links.

### Batch 5: Cross-Platform Keyboard Shortcuts (`Ctrl` on Windows/Linux, `Cmd` on macOS) — DONE
- [x] **`Ctrl+,` / `Cmd+,`:** Open the Settings window (plus native macOS App menu bar entry on macOS, without adding a classic Win32 menu bar strip on Windows/Linux).
- [x] **`Ctrl+N` / `Cmd+N`:** Start a new chat (triggers the Google Messages "Start chat" button).
- [x] **`Ctrl+F` or `Ctrl+K` / `Cmd+F` or `Cmd+K`:** Focus the conversation search input.
- [x] **`Ctrl+1`–`9` / `Cmd+1`–`9`:** Jump to the 1st–9th conversation in the sidebar.
- [x] **`Ctrl+=` / `Ctrl+-` / `Ctrl+0` (and `Cmd`):** Zoom in, zoom out, and reset zoom level.
- [x] **`Ctrl+R` or `F5` / `Cmd+R`:** Reload the main window.
- [x] **`Esc` in Settings:** Close the Settings window.

---

## Platform Runtime Verification (Windows / Linux)

### Linux: runtime verification
- DONE: `webkit2gtk` (`=2.0.2`, matching `wry` 0.55) added as a Linux-only
  dependency in `src-tauri/Cargo.toml`; `grant_media_permissions` in
  `src-tauri/src/lib.rs` (`cfg(target_os = "linux")`) enables
  `media-stream` + `webrtc` settings and auto-allows `UserMediaPermissionRequest`.
- STILL TODO: compile and verify mic/camera in a live call on a Linux machine
  with a working PipeWire/PulseAudio stack.

### Windows: runtime verification
- DONE: auto-grant of WebView2 microphone/camera `PermissionRequested`
  (`grant_media_permissions` in `src-tauri/src/lib.rs`, `cfg(windows)`).
- DONE: compiles & links on Windows (ARM64, `aarch64-pc-windows-msvc`) via
  `npm run dev`.
- STILL TODO: confirm mic/camera actually work in a call on Windows at runtime
  (requires *Settings → Privacy → Microphone → "Let desktop apps access your
  microphone"* enabled) and verify taskbar behavior when the window is hidden
  to the tray.
