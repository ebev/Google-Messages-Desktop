# Changelog

All notable changes to this project are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed
- **Drag cursor feedback is back:** dragging files over the window shows the
  "copy" cursor again (the "+" badge on macOS), which 1.6.0 lost when Tauri's
  drag-drop handler was turned off. Drops keep attaching files as in 1.6.0.

### Changed
- **macOS: "Copy code" is the notification's visible button.** On a
  code-bearing message notification without a reply field, the copy button
  is now the notification's action button instead of an item in the
  "Show ▾" menu; clicking the banner body still opens the chat. Notifications
  with a reply field keep Copy code in the menu, because macOS puts the
  Reply button in the action button's place.

## [1.6.0] - 2026-11-23

### Added
- **Copy verification codes from the notification:** a message notification
  whose text carries a code (a standalone 4–8 digit number, or a prefixed code
  like `G-123456`) gets a "Copy code" button on macOS, Windows and Linux.
  Pressing it copies the digits (`123456` for `G-123456`) to the clipboard
  without opening the window.
- **Drag files onto the window to attach them:** files dropped on an open
  chat now reach the Messages page, which attaches them to the composer.
  Tauri's own drag-drop handler is turned off for the main window; before,
  it took the drop and the page only saw the drag.

### Changed
- CI runs the page-script tests on Node 24 (was 22), and the workflows use
  `actions/checkout@v7` / `actions/setup-node@v7`, which drops the
  "Node.js 20 deprecated" warning.

## [1.5.0] - 2026-11-23

### Added
- **Quick reply from notifications (prototype):** message notifications can
  offer a reply field (macOS), a configurable quick-action emoji and "Mark as
  read" (macOS, Linux). The reply is typed and sent through the Messages page
  only after the open conversation is confirmed to match the notification;
  otherwise the window is shown and a "Reply not sent" notification keeps the
  text.
  On macOS the message is sent with a real Return key press from the app,
  because the page ignores clicks and key presses simulated from script; this
  only works in the bundled app, not `npm run dev`. Linux still relies on the
  simulated path and is untested.
- **Recent messages popover (prototype):** left-clicking the tray icon opens
  a small window by the icon listing your five most recent conversations,
  read from the Messages sidebar (including ones where you sent the last
  message or reaction), with their contact photos. The selected
  one has a reply box (Enter sends), the quick-action emoji and "Mark as
  read"; double-click a conversation to open it. The arrow keys move between conversations and Escape or
  clicking elsewhere closes it. Its footer has Settings and Quit buttons. Replies go through the same path as
  notification replies. Right-click opens it too, so it replaces the old tray
  menu; a new "Recent messages from the tray" setting switches back to the
  old show/hide click and right-click menu. macOS and Windows; Linux trays don't report clicks.
  It looks native on each system: a menu bar panel over the system material
  on macOS (with the footer as menu items and ⌘O, ⌘, and ⌘Q), and a
  Windows 11 tray flyout on Windows. It also shows whether Messages is
  connected; when Messages is open on another computer it says "Not
  connected" and offers "Use here" to move the session to this one, and
  replies wait until then. It also shows when you're offline, the phone
  can't be reached, or you aren't signed in. The window fits its content, and says it is loading
  until Messages has shown your conversations.
- **Quick action** setting (Off / 👍 / ❤️ / 🙂) in Settings, with a "+" button
  that opens an emoji picker for any other emoji (pick from a grid, or type
  or paste one).
- **Native Windows toasts (untested on Windows):** Windows notifications are
  now native toasts sent from Rust, with the same reply field, quick-action
  emoji and "Mark as read" button, and "Switch Here" / "No" on the "open on
  another computer" alert. The app registers its AppUserModelID in the
  registry at startup so toasts carry its name and icon, and removes its
  toasts when it quits. Toasts with a reply field stay on screen until
  answered or dismissed. Replies are sent with a real Enter key press through
  WebView2's DevTools protocol.

### Fixed
- The settings gear no longer overlaps the "Sign in" button on the signed-out
  welcome page; it now sits beside it, as it does beside the account avatar.

### Removed
- The WebView2 web-notification path on Windows, and the automatic grant of
  the WebView2 notification permission (microphone and camera are still
  granted for calls).

## [1.4.0] - 2026-11-14

### Fixed
- `cancelAnimationFrame` could cancel an unrelated timer in the page while the
  app was hidden in the tray.
- Clearing message notifications also dismissed the "open on another computer"
  alert on macOS and Linux.
- A new notification could be dismissed almost immediately when another
  conversation was already unread.
- Notifications were cleared several times per read, and repeatedly on Windows
  while the session was open on another computer.
- On Windows, a failed WebView2 notification could loop between the page and
  the backend.
- An invalid start URL in `settings.json` prevented the app from starting; it
  now falls back to the default.
- On macOS and Linux, clicking a notification now opens that conversation
  instead of only focusing the window.
- Linux notifications are closed with a D-Bus `CloseNotification` call instead
  of a replacement notification.

### Changed
- The injected page script moved from a string in `lib.rs` to
  `src-tauri/src/init_script.js`; `lib.rs` and `settings.html` were reorganized
  and duplicated code merged.
- The sidebar is only scanned while the title reports unread messages, and the
  "open on another computer" check is debounced.
- The Settings version badge is read from the app at runtime, and
  `tauri.conf.json` takes its version from `Cargo.toml`.
- Removed the unused `frontend/index.html` and `@tauri-apps/api` dependency;
  generated `src-tauri/gen/schemas` files are no longer tracked.

### Added
- Tests: `npm test` runs regression tests for the injected script, and
  `cargo test` covers settings, URL validation and a check that the command
  list in `lib.rs`, `build.rs` and `capabilities/default.json` stays in sync.
- GitHub Actions workflow that runs both test suites and compiles the app on
  macOS, Windows and Linux.

## [1.3.1] - 2026-11-14

### Fixed
- **Clear desktop notifications & unread badges when read or replied to on phone:**
  - Observes both `<title>` and sidebar `mws-conversation-list-item` DOM state
    (including outgoing `"You: ..."` snippets) so stale `(N)` title counts and
    tray/taskbar/Dock badges clear immediately when messages are read or replied
    to from your phone while the desktop window is unfocused or in the tray.
  - Tracks active notifications by conversation tag/ID (including
    `ServiceWorkerRegistration.showNotification` / `getNotifications`) and
    dismisses OS notifications on Windows (`OrigNotification.close()`), macOS
    (`removeDeliveredNotification` / `removeAllDeliveredNotifications`), and
    Linux D-Bus (`CloseNotification`) when `.close()` is called, a conversation
    is read, or unread count reaches `0`.
  - Adds a low-rate `setTimeout` fallback for `requestAnimationFrame` when
    `document.hidden` is true so Angular flushes phone sync updates while
    hidden in the tray.

## [1.3.0] - 2026-11-12

### Added
- **Background listener & anti-throttling keep-alive:**
  - Disables WebView background throttling (`BackgroundThrottlingPolicy::Disabled`)
    and passes Chromium background timer/renderer flags on Windows WebView2 so
    connections and notifications stay alive when hidden in the tray.
  - Holds a macOS `NSProcessInfo` `UserInitiatedAllowingIdleSystemSleep`
    activity assertion while background keep-alive is active to prevent App Nap
    suspension without blocking idle system sleep.
  - Runs a 30-second Rust heartbeat & connection watchdog
    (`start_background_listener`) that detects OS sleep/wake jumps, monitors
    connection banners, and silently reloads stalled/unresponsive pages (with a
    120-second cooldown, skipping reloads while composing or on a call).
  - Adds a **Keep connected in background** toggle (`background_keep_alive`,
    enabled by default) in Settings.
- **Native notification bridge & unread fallback:**
  - Replaces `tauri-plugin-notification` with a direct OS notification bridge
    (`send_native_notification`): `NSUserNotificationCenter` + persistent
    delegate on macOS, `notify-rust` action listener on Linux, and WebView2
    native `Notification` with `COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS`
    auto-granted on Windows.
  - Exposes a full `EventTarget`-compliant `window.Notification` wrapper with
    reliable click-to-restore-and-focus behavior.
  - Dispatches a fallback native notification if the unread count increases
    while the app is unfocused/hidden and no web `Notification` fires within
    1.5 seconds.
- **"Google Messages is open on another computer" protection:**
  - Detects when another computer takes over the active Google Messages session
    (`"Use here"` prompt), immediately pauses background auto-refresh, and
    releases the macOS App Nap assertion.
  - Pops a native notification alerting the user; clicking it restores the main
    window and opens a confirmation modal (**Move session here** vs. **No, let
    app sleep**). Ignoring the notification or choosing **No** lets the OS sleep
    the app normally.
- **Redesigned Settings UI & larger SVG gear button:**
  - Replaced the flat Settings page with a native-style grouped card layout
    featuring category icon badges, a segmented **System / Light / Dark** theme
    control, auto-save on toggle/theme change, and a sticky footer bar.
  - Replaced the tiny Unicode `⚙` gear button with a crisp 22×22 Material SVG
    cog inside a 34×34 circular button.

### Fixed
- **Windows build & Settings window deadlock:** removed the unsupported
  `browser_accelerator_keys` builder call (suppressing browser-only shortcuts in
  `INIT_SCRIPT` instead), fixed `set_overlay_icon(Some(badge))` ownership,
  matched `WINDOWS_BROWSER_ARGS` across windows sharing the WebView2
  `userDataFolder`, and spawned `show_settings_window` off the UI thread so
  opening Settings never deadlocks WebView2.

## [1.2.0] - 2026-10-26

### Added
- **Unread badges across Taskbar, Dock, and System Tray:**
  - **Windows:** renders a 32×32 red taskbar overlay icon (`set_overlay_icon`)
    with white bitmap digits (`1`–`99`, `99+`) when unread messages exist.
  - **macOS & Linux:** sets the native numeric Dock / launcher badge
    (`set_badge_count`), re-applying the cached unread count when restoring the
    window from tray-only (`Accessory`) mode.
  - **System Tray (all OSes):** dynamically composites a top-right unread dot
    with a cutout halo onto the tray icon (`#EA4335` red on Windows/Linux;
    template silhouette + menu-bar count title on macOS).
- **Click notification to restore & focus:** clicking a desktop notification
  unminimizes, unhides from the tray, and focuses the main window via a new
  `focus_main_window` command.
- **Full window state persistence & cold-start polish:**
  - Persists window `(x, y)` position and `maximized` state alongside
    `(width, height)`, validating on startup that the saved position still
    overlaps an active monitor before restoring.
  - Sets `skip_taskbar(true/false)` on Windows and Linux when hiding/showing to
    the tray.
  - Sets a theme-matched native window background color (`#131314` dark /
    `#ffffff` light) to prevent a white flash while `messages.google.com` loads.
- **Windows WebView2 & cross-platform web-feel cleanup:**
  - Enables Windows 11 `ScrollBarStyle::FluentOverlay` and disables Edge browser
    accelerator keys (`browser_accelerator_keys(false)` to suppress `Ctrl+P`,
    `F3`, `F7`, and `Ctrl+J`).
  - Adds a native `.on_new_window()` handler across all frames and upgrades the
    JS link router to pierce Shadow DOM, handle middle-clicks (`auxclick`), and
    route external non-Google links to the OS browser even without
    `target="_blank"`.
  - Disables viewport rubber-band overscroll (`overscroll-behavior: none`) and
    ghost-dragging of UI links, buttons, and images.
- **Cross-platform keyboard shortcuts (`Ctrl` on Windows/Linux, `Cmd` on macOS):**
  - `Mod+,` opens Settings; `Mod+N` starts a new chat; `Mod+F` / `Mod+K`
    focuses conversation search; `Mod+1`–`9` switches to the 1st–9th
    conversation; `Mod+=` / `Mod+-` / `Mod+0` adjusts native WebView zoom
    (`50%`–`250%`); `Mod+R` / `F5` reloads; `Esc` / `Mod+W` closes Settings and
    `Enter` / `Mod+S` saves Settings.
  - Adds a native macOS Application Menu bar (**Google Messages / Edit / View /
    Window**) while keeping Windows and Linux free of a classic Win32 menu bar.

## [1.1.0] - 2026-10-24

### Added
- **Linux call audio/video support:** added `webkit2gtk` (`cfg(target_os = "linux")`)
  and implemented `grant_media_permissions` to enable WebKitGTK's
  `enable-media-stream` and `enable-webrtc` settings and auto-allow
  `UserMediaPermissionRequest` prompts.
- **macOS Dock icon hiding when in tray:** switches `tauri::ActivationPolicy` to
  `Accessory` whenever the main window is hidden (closing to tray, left-clicking
  the tray icon to hide, or launching with **Start in Tray** enabled) and back
  to `Regular` when the window is shown again.

### Fixed
- **Google Sign-In passkey failure in embedded webview:** disabled WebAuthn /
  `PublicKeyCredential` capability advertisement across all frames so
  `accounts.google.com` skips the unsupported embedded-webview passkey prompt
  and goes directly to "Approve on another device" (Google Prompt) or password
  sign-in.

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
