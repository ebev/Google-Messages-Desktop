# Google Messages for Desktop

A lightweight, cross-platform desktop wrapper for [Google Messages](https://messages.google.com)
built with [Tauri](https://tauri.app) — it uses the OS-native webview instead of
bundling Chromium, so the app stays small.

> Unofficial — not affiliated with or endorsed by Google. "Google Messages" is a
> trademark of Google LLC.

Every desktop Google Messages wrapper I could find was either a bundled-Chromium
Electron app (heavy, most unmaintained) or a bare webview window with no
notifications or tray behavior. This one aims at the gap in between: small,
native-feeling, and complete as a desktop citizen.

## Features

- Native window wrapping `messages.google.com`
- System tray popover — clicking or right-clicking the tray icon opens a
  recent-messages panel (unread conversations, an inline reply box per
  conversation that sends on Enter, the quick-action emoji, Mark as read,
  and Settings/Quit in the footer; native popover look on macOS, tray
  flyout on Windows). The **Recent messages from the tray** setting
  switches back to the old show/hide click and right-click menu; Linux
  trays don't report clicks and keep the attached menu
- Settings window (tray menu → **Settings…**, `Ctrl+,` / `Cmd+,`, or the
  floating gear in the app): **Launch at Login**, **Start in system tray**,
  **Show desktop notifications**, **Keep connected in background**, theme
  override (System / Light / Dark), and a custom start URL
- Closes to tray instead of quitting (hiding the macOS Dock icon and
  Windows/Linux taskbar presence while in the tray); single-instance enforced;
  window size, position, and maximized state remembered across launches
- Background listener & connection watchdog that prevents OS/WebView
  throttling while in the tray and silently recovers stalled connections after
  sleep or network drops
- Protection for **"Google Messages is open on another computer"**: pauses
  background refresh, pops a native notification, and lets you choose whether
  to move the session here or let the OS sleep the app
- Native OS notifications across macOS, Windows, and Linux (including an
  unread-increase fallback when hidden) — clicking a notification unminimizes,
  unhides, and focuses the main window
- **Reply from notifications:** macOS notifications carry a reply field (sent
  through the page only after the open conversation is confirmed to match,
  otherwise a "Reply not sent" notification keeps the text), a configurable
  **Quick action** emoji (Off / 👍 / ❤️ / 🙂, or pick any from Settings),
  and **Mark as read**. Windows notifications are native toasts with the
  same fields (the WebView2 web-notification path was removed) and a toast
  with a reply box stays on screen until answered or dismissed
- **Drag and drop files:** drop files (one or several) from Finder or
  Explorer onto an open chat to attach them, as in the browser
- **Copy verification codes from the notification:** when a message
  notification carries a code (`482913`, or `G-123456`, which copies
  `123456`), it gets a **Copy code** button that puts the code on the
  clipboard without opening the window (macOS, Windows, Linux)
- Follows the system light/dark theme (or force light/dark in Settings) with a
  theme-matched cold-start background to prevent white flashes
- Unread count surfaced via a native Windows taskbar overlay badge, macOS
  Dock / Linux launcher badge, system tray unread indicator dot, and tray
  tooltip
- Desktop keyboard shortcuts (`Ctrl` on Windows/Linux, `Cmd` on macOS):
  `Mod+,` (Settings), `Mod+N` (new chat), `Mod+F` / `Mod+K` (search),
  `Mod+1`–`9` (jump to conversation), `Mod+=` / `Mod+-` / `Mod+0` (zoom), and
  `Mod+R` / `F5` (reload)
- Google Fi **Calls** / **Voicemail** tabs work in-window (handled around their
  `target="_blank"` links), with a Safari user-agent so they aren't rejected as
  an unsupported browser
- Microphone/camera access for calls (macOS via embedded `Info.plist`; Windows
  via WebView2 permission auto-grant; Linux via WebKitGTK media-stream/WebRTC
  and permission auto-grant)

## Prerequisites

- [Rust](https://rustup.rs)
- [Node.js](https://nodejs.org)
- Platform build tools (Xcode CLT on macOS, C++ Build Tools + WebView2 on
  Windows, WebKitGTK + build deps on Linux)

### Automated setup

One-shot scripts install everything above and run `npm install` (idempotent —
safe to re-run):

```bash
# macOS / Linux
./scripts/setup-dev.sh
```

```powershell
# Windows (run from an elevated PowerShell)
powershell -ExecutionPolicy Bypass -File .\scripts\setup-dev.ps1
```

## Development

```bash
npm install   # if you didn't use the setup script
npm run dev
```

## Tests

```bash
npm test                      # injected page script (jsdom)
cd src-tauri && cargo test    # Rust unit tests, incl. command-list sync check
```

## Build

For a native (host-architecture) build:

```bash
npm run build
```

The bundled app is written to `src-tauri/target/release/bundle/`.

### Cross-architecture builds

`scripts/build.sh` (macOS/Linux) and `scripts/build.ps1` (Windows) build for a
specific architecture, cross-compiling where needed. Run `scripts/setup-dev.*`
first — it installs the extra Rust targets and, on Linux, the cross toolchain.
Each script only builds for the OS it runs on (there's no cross-OS build; e.g.
Windows `.msi`/`.exe` bundles are built on Windows):

```bash
# macOS (Apple Silicon only — no x86_64-apple-darwin build)
./scripts/build.sh

# Linux — defaults to the host architecture; pass a target to cross-compile
./scripts/build.sh                              # native
./scripts/build.sh x86_64-unknown-linux-gnu
./scripts/build.sh aarch64-unknown-linux-gnu
```

```powershell
# Windows — defaults to the host architecture
.\scripts\build.ps1
.\scripts\build.ps1 -Target x64
.\scripts\build.ps1 -Target arm64
```

Bundles land in `src-tauri/target/<target-triple>/release/bundle/`.

## Platform notes

- **macOS / Linux** present a Safari user-agent; **Windows** keeps WebView2's
  native Edge user-agent.
- **Linux** calls require a working PipeWire/PulseAudio stack at runtime.

## License

[MIT](LICENSE).
