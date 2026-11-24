# TODO

## v1.6 — Node upgrade, copy-on-demand codes, drag-and-drop attachments

Goal: modernise the toolchain (the CI pins Node 22, two LTS lines behind 24/26),
match Texty's copy-on-demand verification codes from notifications, and accept
files dropped onto the window.

Status: scoped 2026-11-24; not started. Each item ends with its own device check.

### 1. Node upgrade (CI, then local)
- [ ] **Bump the test runner:** `actions/setup-node@v4` + `node-version: 22` →
  `@v7` + `24` in `ci.yml`. 24 is the mature LTS; 26 went LTS 2026-10-28 and is
  a month old — revisit after 26.1. Also `actions/checkout@v4` → `@v7` on all
  workflows (that v4 pin is what emits the "Node.js 20 deprecated" warning —
  it is the action's own runtime, not ours).
- [ ] **Prove the suites on 24:** page-script tests + packaging legs green in CI.
- [ ] **Local runtime (22.23.4 on this box) is a host decision** — not changed
  by this item; bump separately if wanted (dnf/nodejs-24).
- [ ] Update README "Prerequisites" if it names a Node version.

### 2. Auto code copy (copy-on-demand)
- [ ] **Detection:** match verification codes in notification bodies — bare
  4–8-digit codes and `G-123456`-style prefixed codes (confirm patterns against
  real sign-in texts). Lives in Rust beside the notification builders.
- [ ] **Action:** a "Copy code" button on the native notification when a code
  matches — macOS UNNotification action, Windows toast button, Linux
  notify-rust action. Copy to clipboard without opening the window.
- [ ] **Tests:** detector unit tests; a notification test asserting the action
  appears only when a code matches, and that the copied text is the code alone.
- [ ] Device check: copy a real sign-in code from the toast on macOS + Windows.

### 3. Drag-and-drop attachments
- **Mechanism gate — probe first:** does messages.google.com attach a file
  dropped onto the composer in a plain browser? If YES → set
  `dragDropEnabled: false` on the main window so the webview receives HTML5
  drag events (one config line, the page does the work). If NO → keep it
  enabled and handle `DragDropEvent` in `lib.rs`: read the paths, then feed
  the page's attachment input from `init_script.js` (DataTransfer + `change`
  dispatch). Do not build the Rust path until the probe says it is needed.
- [ ] **Multi-file drops** accepted; document the chosen mechanism here.
- [ ] Device check: Finder drop → attaches (macOS), Explorer drop → attaches
  (Windows), and a drop while the window is in the tray behaves sanely.

---

## v1.5 — Native Windows Toasts & Quick Reply (Windows side) — IMPLEMENTED, NOT YET RUN ON WINDOWS

Goal: replace WebView2's web `Notification` on Windows with native toast
notifications sent from Rust, so Windows gets the same reply field, quick-action
emoji and "Mark as read" button that the macOS/Linux prototype has, and so all
three platforms go through one notification path.

Status: the code is written (`windows_native` in `lib.rs`) and type-checks and
passes clippy for the Windows target, but it has never been run on Windows.
Ticked items are implemented, not verified. Items marked **(confirm)** are
from memory and still need checking on a real machine.

Type-checking for Windows on the Mac works with a stand-in `llvm-rc` on `PATH`
(a script that only creates the output file given after `/fo`), then
`cargo check --target x86_64-pc-windows-msvc`. It cannot link or run.

### 1. Rust toast backend (`cfg(windows)`)
- [x] **Implement the three platform functions natively:** `send_os_notification`, `close_os_notification` and `clear_os_notifications` in `lib.rs` currently eval into the page (`__gmShowWebviewNotification` etc.). Replace them with a `windows_native` module, mirroring `macos_native`.
- [x] **Choose the API:** use the `windows` crate directly (`Windows.UI.Notifications` + `Windows.Data.Xml.Dom`) and build the toast XML by hand. `tauri-winrt-notification` is already in `Cargo.lock` (via `notify-rust`) and would be less code, but it is not known to support text input **(confirm)**; the reply field is the reason for this work, so do not depend on it unless it does.
- [x] **Pin the `windows` crate to the version Tauri already pulls in** (0.61 in the current lockfile) so it is not compiled twice, and enable only the features needed (`UI_Notifications`, `Data_Xml_Dom`, `Foundation`, `Foundation_Collections`, plus registry access for item 2).
- [x] **Keep sent toasts in a map** keyed by our notification id (like `DELIVERED_NOTIFS` / `LINUX_NOTIFS`). The `Activated` handlers only fire while the `ToastNotification` object is alive **(confirm)**, and the map is also what `close` uses.
- [x] **Replace, close, clear:** set `Tag` (a hash of our id, since tags are length-limited **(confirm: 64 chars)**) and `Group` (`messages` or `takeover`). Re-sending the same id replaces the toast. Close with `ToastNotificationManager::History().Remove(tag, group, aumid)`. Clear message toasts with `RemoveGroup("messages", aumid)`, which leaves the takeover toast alone (same rule as macOS/Linux).
- [x] **Clear our toasts on quit** (`quit_app`): toasts left in the notification center after the app exits cannot do anything when clicked, because no COM activator is registered (see item 2).

### 2. App identity (AUMID)
- [x] **Register an AppUserModelID at startup:** an unpackaged app needs one for toasts to show with the right name and icon, and for buttons to work. Write `HKCU\Software\Classes\AppUserModelId\<aumid>` with `DisplayName` and `IconUri` **(confirm this is enough without a Start Menu shortcut on Windows 10 and 11)**. Doing it at startup covers `npm run dev` as well as installed builds.
- [ ] **Use the bundle identifier (`com.googleMessages.app`) as the AUMID** and check it matches what the NSIS/MSI installer assigns to its Start Menu shortcut **(confirm)**, so toasts and the taskbar button belong to the same app.
- [x] **No background (COM) activation:** not needed, because the app is always running in the tray when a notification fires. Decide explicitly rather than by omission; revisit only if toasts must work after the app has exited.

### 3. Toast content and buttons
- [x] **Map `NotificationKind` to toast XML:**
  - `Plain`: title + body, click opens the window.
  - `Takeover`: title + body, buttons "Switch Here" and "No" (same as macOS).
  - `Conversation { quick_action }`: title + body, a text `<input>` with placeholder "Reply…", a "Send" button bound to that input (`hint-inputId`), the quick-action emoji button if configured, and "Mark as read". Toasts allow up to five actions **(confirm)**, so all of these fit.
- [x] **Escape title, body and emoji for XML** before building the document; these come from message content.
- [x] **Sound and Focus Assist:** use the default toast sound; let Windows apply Focus Assist / Do Not Disturb. No custom handling.
- [ ] **Out of scope for the first version:** contact avatars, inline images, and per-conversation grouping in the notification center.

### 4. Handling clicks, replies and buttons
- [x] **Subscribe to `Activated` on each toast** and cast the args to `ToastActivatedEventArgs`: `Arguments()` identifies the button, `UserInput()` carries the reply text. `UserInput` needs Windows 10 version 1903 or later **(confirm)**; on older builds send the toast without the reply field.
- [x] **Route to the existing handlers,** so the page-side logic is shared with macOS/Linux:
  - body click → `handle_notification_click` (extend its `cfg` to Windows)
  - reply text / quick emoji → `handle_notification_action(.., NotificationAction::Reply(text))`
  - "Mark as read" → `NotificationAction::MarkRead`
  - takeover "Switch Here" → `open_and_prompt_takeover`
- [x] **Threading:** the handler runs on a WinRT thread, not the UI thread. `eval_in_main` is safe from there; anything touching windows goes through `run_on_main_thread`.
- [ ] **Bringing the window forward:** check that `show_main_window` from a toast click actually takes focus. Windows restricts background focus changes, though a toast click normally grants the right **(confirm on a real machine)**.
- [x] **`Dismissed` is deliberately not handled:** it also fires when a banner times out into the notification center, where the toast must stay clickable, so toasts stay in the map until they are closed, cleared or clicked.
- [ ] **Real Enter key for replies:** the page ignores simulated keys, so the send uses WebView2's DevTools protocol (`Input.dispatchKeyEvent`). Check it sends with the window hidden in the tray; if the page needs focus, the fallback shows the window and presses again **(confirm on a real machine)**.

### 5. Page script and cleanup
- [x] **Remove the WebView2 notification path from `init_script.js`:** `useWebviewNotification`, `OrigNotification`, `requestWebviewNotificationPermission`, the `webviewNotif` branch of `dispatchNotification`, `showWebviewNotification` / `closeWebviewNotification` / `clearWebviewNotifications`, the `fromBackend` guard, and the three `window.__gm*WebviewNotification*` entry points. After this every platform sends `send_native_notification`.
- [x] **Stop auto-granting `COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS`** in `grant_media_permissions`; keep microphone and camera.
- [x] **`GMNotification.permission` / `requestPermission`** can then always report `granted`.
- [x] **Update the tests:** delete the three "on Windows" WebView2 tests in `tests/init_script.test.mjs` and add one asserting Windows now uses the native bridge.

### 6. Verification
- [ ] **CI:** the `windows-latest` job must compile and pass `cargo test`. This is the only build check available without a Windows machine.
- [ ] **Manual, on Windows 10 and 11, in dev and from an installed build:**
  - toast shows the app name and icon
  - a second message in the same conversation replaces the first toast
  - reading the conversation on the phone removes its toast; the takeover toast survives a clear
  - body click opens the right conversation
  - a typed reply, the emoji button and "Mark as read" each work with the window hidden in the tray
  - "Show desktop notifications" off sends nothing
  - quitting the app removes its toasts from the notification center
- [x] **macOS prototype check:** typed reply and the quick-action emoji were confirmed against the real Messages page on macOS; "Mark as read" was not.

---

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
