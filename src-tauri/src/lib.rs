use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    OnceLock,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_autostart::ManagerExt;

#[cfg(target_os = "macos")]
use macos_native::{
    clear_notifications as clear_os_notifications, close_notification as close_os_notification,
    send_notification as send_os_notification,
};

// File layout: constants, statics and types first, then every free function
// grouped by feature (shared helpers → settings → windows → keep-alive →
// notifications → tray/badges → menus → permissions → Tauri commands → `run`),
// then the macOS-native module. The injected page script lives in
// `init_script.js`.

// Logs an IPC command invocation to stderr in debug builds only.
macro_rules! ipc_log {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        eprintln!("[ipc] {}", format_args!($($arg)*));
    };
}

// =============================================================================
// Constants
// =============================================================================

const APP_NAME: &str = "Google Messages";
const MAIN_WINDOW: &str = "main";
const SETTINGS_WINDOW: &str = "settings";
const TRAY_ID: &str = "main";
// Identifier of the "open on another computer" notification.
const TAKEOVER_NOTIFICATION_ID: &str = "gm-takeover";

// Default start URL if the user has not customized it. The canonical web
// entry point: when already paired this lands straight on the (cached)
// conversation list; when not paired it redirects to the QR pairing page on
// its own. Loading the pairing sub-route directly added an extra hop.
const DEFAULT_START_URL: &str = "https://messages.google.com/web/";
const MESSAGES_HOST: &str = "messages.google.com";

// Main window geometry (logical pixels): size used until one is remembered,
// and the minimum the window can be resized (or restored) to.
const DEFAULT_MAIN_SIZE: (f64, f64) = (1100.0, 780.0);
const MIN_MAIN_SIZE: (f64, f64) = (400.0, 500.0);

// Minimum gap between watchdog-triggered reloads so auto-reload can never loop.
const AUTO_RELOAD_COOLDOWN_SECS: u64 = 120;

#[cfg(windows)]
const WINDOWS_BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
     --disable-background-timer-throttling \
     --disable-renderer-backgrounding \
     --disable-backgrounding-occluded-windows";

// On macOS & Linux the embedded WebKit view's default user-agent omits the
// "Version/… Safari/…" token, so some Google properties (e.g. the Fi calls app)
// reject it as an unsupported browser. Override it to present as Safari, which
// is the engine we actually are.
//
// On Windows we deliberately leave the UA alone: the Chromium-based WebView2
// runtime already sends a complete, always-current Edge UA that these apps
// accept, and hard-coding a version string here would only go stale.
#[cfg(any(target_os = "macos", target_os = "linux"))]
const USER_AGENT: Option<&str> = Some("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4.1 Safari/605.1.15");
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const USER_AGENT: Option<&str> = None;

// Script injected into the main webview on load (including subframes); see the
// header of init_script.js for what it does. The two `__GM_*_DEFAULT__` tokens
// in it are replaced with the persisted settings before injection.
const INIT_SCRIPT: &str = include_str!("init_script.js");

// =============================================================================
// Statics
// =============================================================================

// Tracks the latest unread count so the Dock/taskbar badge can be re-applied
// whenever the main window transitions back from tray-only (Accessory) mode.
static LAST_UNREAD_COUNT: AtomicU32 = AtomicU32::new(0);
// Tracks the last unread count applied to the tray so repeated title mutations
// with the same count don't re-apply icons/tooltips.
static LAST_TRAY_COUNT: AtomicU32 = AtomicU32::new(u32::MAX);

// Cached RGBA buffers for the base and badged tray icons to avoid repeated PNG
// decoding and compositing on every unread count change.
static BASE_TRAY_ICON: OnceLock<IconData> = OnceLock::new();
static BADGED_TRAY_ICON: OnceLock<IconData> = OnceLock::new();

// Tracks the main window's zoom percentage (100 = 1.0x, clamped to 50..=250).
static ZOOM_PERCENT: AtomicU32 = AtomicU32::new(100);

// Live runtime flags mirrored from Settings and page state:
static SHOW_NOTIFICATIONS: AtomicBool = AtomicBool::new(true);
static BACKGROUND_KEEP_ALIVE: AtomicBool = AtomicBool::new(true);
// True when "Google Messages is open on another computer" is active and the
// user hasn't switched the session back here; while true, background keep-alive
// and auto-refresh are paused so the OS can sleep the app normally.
static SESSION_YIELDED: AtomicBool = AtomicBool::new(false);
// Watchdog counters for the background heartbeat loop.
static PENDING_HEARTBEAT_PINGS: AtomicU32 = AtomicU32::new(0);
static UNHEALTHY_STREAK: AtomicU32 = AtomicU32::new(0);
static LAST_AUTO_RELOAD_SECS: AtomicU64 = AtomicU64::new(0);

// Notifications currently on screen on Linux, by our notification id.
#[cfg(target_os = "linux")]
static LINUX_NOTIFS: OnceLock<std::sync::Mutex<LinuxNotifMap>> = OnceLock::new();

// =============================================================================
// Types
// =============================================================================

#[cfg(target_os = "linux")]
type LinuxNotifMap = std::collections::HashMap<String, notify_rust::NotificationHandle>;

// Decoded icon pixels: (RGBA bytes, width, height).
type IconData = (Vec<u8>, u32, u32);

// User-facing settings, persisted to settings.json in the app config dir.
// (Launch-at-login is tracked by the autostart plugin's own OS state, so it is
// not duplicated here.)
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
struct Settings {
    /// When true, the app launches with the window hidden (tray only).
    start_in_tray: bool,
    /// URL the main window loads on launch. Defaults to Google Messages.
    start_url: Option<String>,
    /// When true, the app shows native web notifications (the web app's own).
    show_notifications: bool,
    /// When true, keeps the webview active in the background and recovers
    /// stalled connections (automatically pauses if open on another computer).
    background_keep_alive: bool,
    /// UI theme override: "system", "light", or "dark".
    theme: Option<String>,
    /// Remembered window geometry for the main webview.
    main_window_size: Option<WindowSize>,
    /// Mirrored copy of the autostart plugin state so the settings page can show it.
    launch_at_login: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            start_in_tray: false,
            start_url: None,
            // Notifications and background keep-alive on by default — also the
            // fallback for settings.json files written before these fields
            // existed (via #[serde(default)]).
            show_notifications: true,
            background_keep_alive: true,
            theme: None,
            main_window_size: None,
            launch_at_login: false,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
struct WindowSize {
    width: f64,
    height: f64,
    x: Option<f64>,
    y: Option<f64>,
    maximized: bool,
}

// =============================================================================
// Shared helpers
// =============================================================================

fn main_window(app: &tauri::AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window(MAIN_WINDOW)
}

// Runs a script in the main window's page, if the window exists.
fn eval_in_main(app: &tauri::AppHandle, script: &str) {
    if let Some(window) = main_window(app) {
        let _ = window.eval(script);
    }
}

#[cfg(target_os = "macos")] // only the macOS app menu needs this
fn is_main_focused(app: &tauri::AppHandle) -> bool {
    main_window(app).is_some_and(|w| w.is_focused().unwrap_or(false))
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Unread count as shown in badges and tooltips ("1".."99", then "99+").
fn unread_label(count: u32) -> String {
    if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    }
}

// True while the app should actively hold its connection in the background:
// the setting is on and the session has not been yielded to another computer.
fn keep_alive_active() -> bool {
    BACKGROUND_KEEP_ALIVE.load(Ordering::Relaxed) && !SESSION_YIELDED.load(Ordering::Relaxed)
}

fn reset_watchdog_counters() {
    PENDING_HEARTBEAT_PINGS.store(0, Ordering::Relaxed);
    UNHEALTHY_STREAK.store(0, Ordering::Relaxed);
}

// =============================================================================
// Settings persistence
// =============================================================================

fn settings_path(app: &tauri::AppHandle) -> Option<std::path::PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join("settings.json"))
}

fn load_settings(app: &tauri::AppHandle) -> Settings {
    settings_path(app)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_settings(app: &tauri::AppHandle, settings: &Settings) {
    if let Some(path) = settings_path(app) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_string_pretty(settings) {
            let _ = std::fs::write(path, json);
        }
    }
}

// Parses a user-supplied start URL, accepting only http(s).
fn parse_start_url(value: &str) -> Option<tauri::Url> {
    value
        .trim()
        .parse::<tauri::Url>()
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
}

// Mirrors the settings that have live runtime flags into their atomics and
// re-syncs the OS keep-alive assertion.
fn apply_runtime_flags(app: &tauri::AppHandle, settings: &Settings) {
    SHOW_NOTIFICATIONS.store(settings.show_notifications, Ordering::Relaxed);
    BACKGROUND_KEEP_ALIVE.store(settings.background_keep_alive, Ordering::Relaxed);
    sync_os_keep_alive(app);
}

// =============================================================================
// Theme & window construction
// =============================================================================

// Maps the persisted theme string to a Tauri theme override; anything other
// than an explicit "light"/"dark" means "follow the system".
fn parse_theme(theme: Option<&str>) -> Option<tauri::Theme> {
    match theme {
        Some("light") => Some(tauri::Theme::Light),
        Some("dark") => Some(tauri::Theme::Dark),
        _ => None,
    }
}

// Native window background color matching Google Messages' light (#ffffff) and
// dark (#131314) surfaces so cold starts don't flash white in dark mode.
fn theme_bg_color(theme: tauri::Theme) -> tauri::window::Color {
    match theme {
        tauri::Theme::Dark => tauri::window::Color(19, 19, 20, 255),
        _ => tauri::window::Color(255, 255, 255, 255),
    }
}

// Starts a window builder with everything the main and settings windows share:
// the theme override, a theme-matched background, and (on Windows) the
// WebView2 options. Both windows use one WebView2 userDataFolder environment,
// so `additional_browser_args` and `scroll_bar_style` must be identical.
fn themed_window_builder<'a>(
    app: &'a tauri::AppHandle,
    label: &str,
    url: WebviewUrl,
    theme: Option<tauri::Theme>,
) -> WebviewWindowBuilder<'a, tauri::Wry, tauri::AppHandle> {
    let builder = WebviewWindowBuilder::new(app, label, url)
        .theme(theme)
        .background_color(theme_bg_color(theme.unwrap_or(tauri::Theme::Dark)));
    #[cfg(windows)]
    let builder = builder
        .scroll_bar_style(tauri::webview::ScrollBarStyle::FluentOverlay)
        .additional_browser_args(WINDOWS_BROWSER_ARGS);
    builder
}

// Checks whether the saved logical window position (x, y) is still visible on
// at least one connected monitor (guarding against unplugged external displays).
fn is_position_on_screen(app: &tauri::AppHandle, x: f64, y: f64, width: f64) -> bool {
    let Ok(monitors) = app.available_monitors() else {
        return true;
    };
    if monitors.is_empty() {
        return true;
    }
    for m in monitors {
        let scale = m.scale_factor();
        let pos = m.position().to_logical::<f64>(scale);
        let size = m.size().to_logical::<f64>(scale);
        let mx2 = pos.x + size.width;
        let my2 = pos.y + size.height;
        // Require at least 100px of the window's top edge to lie within the monitor.
        let overlap_x = (x + width).min(mx2) - x.max(pos.x);
        let in_y = y >= pos.y - 20.0 && y + 60.0 <= my2;
        if overlap_x >= 100.0 && in_y {
            return true;
        }
    }
    false
}

// Opens the local settings window, or focuses it if already open.
// Building a new WebView2 window on Windows must happen off the main UI thread
// (otherwise `webview2_com::wait_with_pump` deadlocks inside WebView2 IPC or
// menu callbacks), hence the spawned thread.
fn show_settings_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(SETTINGS_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let theme = parse_theme(load_settings(&app).theme.as_deref());
        let built = themed_window_builder(
            &app,
            SETTINGS_WINDOW,
            WebviewUrl::App("settings.html".into()),
            theme,
        )
        .title("Settings")
        .inner_size(500.0, 580.0)
        .min_inner_size(400.0, 460.0)
        .center()
        .build();
        if let Err(e) = built {
            eprintln!("[show_settings_window] failed to build settings window: {e}");
        }
    });
}

// =============================================================================
// Main window visibility & geometry
// =============================================================================

// Shows, unminimizes, and focuses the main window, restoring the Dock/taskbar icon.
fn show_main_window(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(window) = main_window(app) {
        #[cfg(not(target_os = "macos"))]
        let _ = window.set_skip_taskbar(false);
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        apply_window_badge(&window, LAST_UNREAD_COUNT.load(Ordering::Relaxed));
    }
}

// Saves geometry and hides the main window to the tray, removing the Dock/taskbar icon.
fn hide_main_window(app: &tauri::AppHandle) {
    save_main_window_size(app);
    if let Some(window) = main_window(app) {
        let _ = window.hide();
        #[cfg(not(target_os = "macos"))]
        let _ = window.set_skip_taskbar(true);
    }
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Accessory);
}

// Persists the main window's current logical size, position, and maximized
// state so it can be restored on the next launch. Called when the window hides
// to the tray and on quit.
fn save_main_window_size(app: &tauri::AppHandle) {
    let Some(window) = main_window(app) else {
        return;
    };
    // Minimized or hidden windows can report bogus positions/sizes (such as
    // (-32000, -32000) on Windows); preserve the last valid geometry instead.
    if window.is_minimized().unwrap_or(false) || !window.is_visible().unwrap_or(true) {
        return;
    }

    let mut settings = load_settings(app);
    let maximized = window.is_maximized().unwrap_or(false);

    if maximized {
        // Keep the previous unmaximized size/position if we have one so
        // unmaximizing on a future session restores reasonable bounds.
        let mut geom = settings.main_window_size.unwrap_or(WindowSize {
            width: DEFAULT_MAIN_SIZE.0,
            height: DEFAULT_MAIN_SIZE.1,
            x: None,
            y: None,
            maximized: true,
        });
        geom.maximized = true;
        settings.main_window_size = Some(geom);
        save_settings(app, &settings);
    } else if let (Ok(size), Ok(scale)) = (window.inner_size(), window.scale_factor()) {
        let logical_size = size.to_logical::<f64>(scale);
        let (x, y) = window
            .outer_position()
            .ok()
            .map(|p| {
                let lp = p.to_logical::<f64>(scale);
                (Some(lp.x), Some(lp.y))
            })
            .unwrap_or((None, None));
        settings.main_window_size = Some(WindowSize {
            width: logical_size.width,
            height: logical_size.height,
            x,
            y,
            maximized: false,
        });
        save_settings(app, &settings);
    }
}

// Captures the final window geometry, then exits. The visible window never
// fires CloseRequested on the quit path, so it has to be saved here.
fn quit_app(app: &tauri::AppHandle) {
    save_main_window_size(app);
    app.exit(0);
}

// Opens and focuses the main window and asks the user whether to move the
// active Google Messages session back to this computer.
fn open_and_prompt_takeover(app: &tauri::AppHandle) {
    show_main_window(app);
    eval_in_main(
        app,
        "window.__GM_SHOW_TAKEOVER_PROMPT && window.__GM_SHOW_TAKEOVER_PROMPT();",
    );
}

// True for the "open on another computer" notification's identifier.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn is_takeover_notification(id: &str) -> bool {
    id.starts_with(TAKEOVER_NOTIFICATION_ID)
}

// Handles a click on one of our OS notifications: the takeover notification
// opens the "move session here?" prompt; a message notification shows the
// window and hands the click to the page, which runs the web app's own click
// handler so the conversation opens.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn handle_notification_click(app: &tauri::AppHandle, id: &str, is_takeover: bool) {
    if is_takeover {
        open_and_prompt_takeover(app);
        return;
    }
    show_main_window(app);
    if let Ok(id) = serde_json::to_string(id) {
        eval_in_main(
            app,
            &format!(
                "window.__gmNativeNotificationClicked && window.__gmNativeNotificationClicked({id});"
            ),
        );
    }
}

// =============================================================================
// Background keep-alive & connection watchdog
// =============================================================================

// Syncs the OS-level background activity assertion (macOS App Nap prevention)
// with the current `BACKGROUND_KEEP_ALIVE` setting and `SESSION_YIELDED` state.
// When the session is yielded to another computer (or keep-alive is disabled),
// the assertion is released so the OS can sleep the app normally.
fn sync_os_keep_alive(app: &tauri::AppHandle) {
    let active = keep_alive_active();
    #[cfg(target_os = "macos")]
    macos_native::set_keep_alive(app, active);
    #[cfg(not(target_os = "macos"))]
    let _ = (app, active);
}

fn try_reload_main_window(app: &tauri::AppHandle, reason: &str) {
    let now = now_epoch_secs();
    let last = LAST_AUTO_RELOAD_SECS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < AUTO_RELOAD_COOLDOWN_SECS {
        return;
    }
    LAST_AUTO_RELOAD_SECS.store(now, Ordering::Relaxed);
    #[cfg(debug_assertions)]
    eprintln!("[watchdog] reloading main window ({reason})");
    let _ = reason;
    eval_in_main(
        app,
        &format!(
            "if (location.host === '{MESSAGES_HOST}' && navigator.onLine !== false) {{ location.reload(); }}"
        ),
    );
}

// Background listener thread: wakes every 30s to nudge the hidden webview,
// detect sleep/wake jumps, and verify connection health when keep-alive is
// active and the session has not been yielded to another computer.
fn start_background_listener(app: tauri::AppHandle) {
    std::thread::spawn(move || {
        let mut last_tick = now_epoch_secs();
        loop {
            std::thread::sleep(Duration::from_secs(30));
            let now = now_epoch_secs();
            let elapsed = now.saturating_sub(last_tick);
            last_tick = now;

            if !keep_alive_active() {
                reset_watchdog_counters();
                continue;
            }

            // If wall-clock time jumped by >= 90s for a 30s sleep, the machine
            // just woke from OS sleep — give networking 6s to settle first.
            let woke_from_sleep = elapsed >= 90;
            if woke_from_sleep {
                PENDING_HEARTBEAT_PINGS.store(0, Ordering::Relaxed);
                std::thread::sleep(Duration::from_secs(6));
            }

            let pending = PENDING_HEARTBEAT_PINGS.fetch_add(1, Ordering::Relaxed) + 1;
            if pending >= 3 {
                PENDING_HEARTBEAT_PINGS.store(0, Ordering::Relaxed);
                try_reload_main_window(&app, "unresponsive webview");
                continue;
            }

            eval_in_main(
                &app,
                &format!("window.__gmHeartbeat && window.__gmHeartbeat({woke_from_sleep});"),
            );
        }
    });
}

// =============================================================================
// OS notifications
// =============================================================================
// Each platform provides `send_os_notification`, `close_os_notification` and
// `clear_os_notifications`. macOS: `macos_native` (imported above). Linux:
// D-Bus via notify-rust. Windows: WebView2's own Notification, driven through
// the page script.

#[cfg(target_os = "linux")]
fn linux_notif_map() -> &'static std::sync::Mutex<LinuxNotifMap> {
    LINUX_NOTIFS.get_or_init(|| std::sync::Mutex::new(LinuxNotifMap::new()))
}

// Closes notifications with a real D-Bus `CloseNotification` call. Off the
// calling thread because notify-rust blocks on the D-Bus round trip.
#[cfg(target_os = "linux")]
fn close_linux_notifications(handles: Vec<notify_rust::NotificationHandle>) {
    if handles.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for handle in handles {
            handle.close();
        }
    });
}

#[cfg(target_os = "linux")]
fn send_os_notification(
    app: &tauri::AppHandle,
    id: String,
    title: String,
    body: String,
    is_takeover: bool,
) {
    let app = app.clone();
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        n.summary(&title).body(&body).auto_icon();
        // Re-sending an id that is still on screen replaces that notification.
        let replaced_id = linux_notif_map()
            .lock()
            .ok()
            .and_then(|guard| guard.get(&id).map(|handle| handle.id()));
        if let Some(existing_id) = replaced_id {
            n.id(existing_id);
        }
        n.action("default", "Open");
        if is_takeover {
            n.action("use_here", "Switch Here").action("no", "No");
        }
        let Ok(handle) = n.show() else {
            return;
        };
        // The handle is kept in the map so the notification can be closed
        // later (and because it keeps the D-Bus connection that some desktops
        // need for actions alive).
        let dbus_id = handle.id();
        if let Ok(mut guard) = linux_notif_map().lock() {
            guard.insert(id.clone(), handle);
        }
        // A replaced notification keeps its D-Bus id, and the thread that
        // first showed it is still listening for that id — a second listener
        // would handle each click twice.
        if replaced_id == Some(dbus_id) {
            return;
        }

        // Blocks until the notification is clicked or closed.
        let _ = notify_rust::handle_action(dbus_id, |response: &notify_rust::ActionResponse| {
            if let notify_rust::ActionResponse::Custom(action) = response {
                if matches!(*action, "default" | "use_here") {
                    handle_notification_click(&app, &id, is_takeover);
                }
            }
        });
        if let Ok(mut guard) = linux_notif_map().lock() {
            if guard.get(&id).is_some_and(|handle| handle.id() == dbus_id) {
                guard.remove(&id);
            }
        }
    });
}

#[cfg(target_os = "linux")]
fn close_os_notification(_app: &tauri::AppHandle, id: &str) {
    let removed = linux_notif_map().lock().ok().and_then(|mut g| g.remove(id));
    close_linux_notifications(removed.into_iter().collect());
}

// Closes every message notification. The takeover notification is left in
// place: it is cleared only when the session comes back.
#[cfg(target_os = "linux")]
fn clear_os_notifications(_app: &tauri::AppHandle) {
    let handles: Vec<notify_rust::NotificationHandle> = linux_notif_map()
        .lock()
        .map(|mut guard| {
            let ids: Vec<String> = guard
                .keys()
                .filter(|id| !is_takeover_notification(id))
                .cloned()
                .collect();
            ids.iter().filter_map(|id| guard.remove(id)).collect()
        })
        .unwrap_or_default();
    close_linux_notifications(handles);
}

#[cfg(windows)]
fn send_os_notification(
    app: &tauri::AppHandle,
    id: String,
    title: String,
    body: String,
    is_takeover: bool,
) {
    if let (Ok(i), Ok(t), Ok(b)) = (
        serde_json::to_string(&id),
        serde_json::to_string(&title),
        serde_json::to_string(&body),
    ) {
        let kind = if is_takeover {
            "\"takeover\""
        } else {
            "\"message\""
        };
        eval_in_main(
            app,
            &format!(
                "window.__gmShowWebviewNotification && window.__gmShowWebviewNotification({t}, {b}, {kind}, {{ tag: {i} }});"
            ),
        );
    }
}

#[cfg(windows)]
fn close_os_notification(app: &tauri::AppHandle, id: &str) {
    if let Ok(i) = serde_json::to_string(id) {
        eval_in_main(
            app,
            &format!(
                "window.__gmCloseWebviewNotification && window.__gmCloseWebviewNotification({i});"
            ),
        );
    }
}

#[cfg(windows)]
fn clear_os_notifications(app: &tauri::AppHandle) {
    eval_in_main(
        app,
        "window.__gmClearWebviewNotifications && window.__gmClearWebviewNotifications();",
    );
}

// =============================================================================
// Zoom
// =============================================================================

// Adjusts the main webview's native zoom factor ("in", "out", or "reset").
fn apply_zoom_action(app: &tauri::AppHandle, action: &str) {
    let cur = ZOOM_PERCENT.load(Ordering::Relaxed);
    let next = match action {
        "in" => (cur + 10).min(250),
        "out" => cur.saturating_sub(10).max(50),
        "reset" => 100,
        _ => cur,
    };
    ZOOM_PERCENT.store(next, Ordering::Relaxed);
    if let Some(window) = main_window(app) {
        let _ = window.set_zoom(next as f64 / 100.0);
    }
}

// =============================================================================
// Tray icon & unread badges
// =============================================================================

fn icon_from_data(data: &IconData) -> tauri::image::Image<'static> {
    tauri::image::Image::new_owned(data.0.clone(), data.1, data.2)
}

// Returns the base (no-unread) tray icon for the current platform:
//   - macOS: a monochrome (black-on-transparent) template; the OS recolors
//     it for light/dark menu bars. The full app icon would render as a solid
//     square here, since template mode only uses the alpha channel.
//   - Windows/Linux: a colored bubble. Those platforms don't support
//     template images, so a black icon would be invisible on a dark taskbar.
fn load_base_tray_icon() -> tauri::Result<tauri::image::Image<'static>> {
    if let Some(data) = BASE_TRAY_ICON.get() {
        return Ok(icon_from_data(data));
    }
    #[cfg(target_os = "macos")]
    let img = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png"))?;
    #[cfg(not(target_os = "macos"))]
    let img = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon-color.png"))?;
    let _ = BASE_TRAY_ICON.set((img.rgba().to_vec(), img.width(), img.height()));
    Ok(img)
}

// Composites an unread notification dot onto the top-right corner of the tray
// icon, surrounded by a transparent cutout ring so it separates cleanly from
// the chat bubble glyph.
//  - On Windows/Linux, the dot is Google Red (#EA4335).
//  - On macOS, the dot is opaque black so `icon_as_template(true)` tints it to
//    match the menu bar alongside the bubble silhouette.
fn load_badged_tray_icon() -> tauri::Result<tauri::image::Image<'static>> {
    if let Some(data) = BADGED_TRAY_ICON.get() {
        return Ok(icon_from_data(data));
    }
    let base = load_base_tray_icon()?;
    let width = base.width();
    let height = base.height();
    let mut rgba = base.rgba().to_vec();

    let cx = 102.0_f32;
    let cy = 26.0_f32;
    let r_inner = 20.0_f32;
    let r_outer = 26.0_f32;

    #[cfg(target_os = "macos")]
    let (dot_r, dot_g, dot_b): (u8, u8, u8) = (0, 0, 0);
    #[cfg(not(target_os = "macos"))]
    let (dot_r, dot_g, dot_b): (u8, u8, u8) = (234, 67, 53);

    for y in 0..height {
        for x in 0..width {
            let dx = x as f32 + 0.5 - cx;
            let dy = y as f32 + 0.5 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist >= r_outer + 1.0 {
                continue;
            }
            let idx = ((y * width + x) * 4) as usize;
            // Clear existing glyph pixels inside the outer cutout halo
            if dist <= r_outer {
                rgba[idx + 3] = 0;
            } else {
                let keep = (dist - r_outer).clamp(0.0, 1.0);
                rgba[idx + 3] = (rgba[idx + 3] as f32 * keep).round() as u8;
            }
            // Draw the solid unread dot inside r_inner
            if dist < r_inner + 1.0 {
                let alpha = (r_inner + 0.5 - dist).clamp(0.0, 1.0);
                let a = (alpha * 255.0).round() as u8;
                if a > 0 {
                    rgba[idx] = dot_r;
                    rgba[idx + 1] = dot_g;
                    rgba[idx + 2] = dot_b;
                    rgba[idx + 3] = a;
                }
            }
        }
    }

    let data = BADGED_TRAY_ICON.get_or_init(|| (rgba, width, height));
    Ok(icon_from_data(data))
}

// Renders a 32x32 red badge overlay with white bitmap digits ("1".."99", "99+")
// for the Windows taskbar button (`set_overlay_icon`).
#[cfg(windows)]
fn render_windows_overlay_badge(count: u32) -> tauri::image::Image<'static> {
    const SIZE: u32 = 32;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let cx = 15.5_f32;
    let cy = 15.5_f32;
    let radius = 14.5_f32;

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            if dist < radius + 0.5 {
                let alpha = (radius + 0.5 - dist).clamp(0.0, 1.0);
                let idx = ((y * SIZE + x) * 4) as usize;
                rgba[idx] = 217; // #D93025
                rgba[idx + 1] = 48;
                rgba[idx + 2] = 37;
                rgba[idx + 3] = (alpha * 255.0).round() as u8;
            }
        }
    }

    // 3x5 pixel font for digits '0'..'9' and '+' (each row is 3 bits, MSB left)
    fn glyph(ch: char) -> [u8; 5] {
        match ch {
            '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
            '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
            '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
            '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
            '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
            '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
            '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
            '7' => [0b111, 0b001, 0b001, 0b010, 0b010],
            '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
            '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
            '+' => [0b000, 0b010, 0b111, 0b010, 0b000],
            _ => [0; 5],
        }
    }

    let chars: Vec<char> = unread_label(count).chars().collect();
    let scale: u32 = 2;
    let char_w = 3 * scale;
    let char_h = 5 * scale;
    let gap = if chars.len() >= 3 { 1 } else { 2 };
    let total_w = chars.len() as u32 * char_w + (chars.len().saturating_sub(1) as u32) * gap;
    let start_x = (SIZE.saturating_sub(total_w)) / 2;
    let start_y = (SIZE.saturating_sub(char_h)) / 2;

    for (i, ch) in chars.into_iter().enumerate() {
        let rows = glyph(ch);
        let ox = start_x + i as u32 * (char_w + gap);
        for (ry, bits) in rows.iter().enumerate() {
            for rx in 0..3u32 {
                if (bits >> (2 - rx)) & 1 == 1 {
                    for sy in 0..scale {
                        for sx in 0..scale {
                            let px = ox + rx * scale + sx;
                            let py = start_y + ry as u32 * scale + sy;
                            if px < SIZE && py < SIZE {
                                let idx = ((py * SIZE + px) * 4) as usize;
                                rgba[idx] = 255;
                                rgba[idx + 1] = 255;
                                rgba[idx + 2] = 255;
                                rgba[idx + 3] = 255;
                            }
                        }
                    }
                }
            }
        }
    }

    tauri::image::Image::new_owned(rgba, SIZE, SIZE)
}

// Updates the native taskbar / Dock / launcher unread indicator on the main window.
fn apply_window_badge(window: &tauri::WebviewWindow, count: u32) {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let badge = if count > 0 { Some(count as i64) } else { None };
        let _ = window.set_badge_count(badge);
    }
    #[cfg(windows)]
    {
        let badge = (count > 0).then(|| render_windows_overlay_badge(count));
        let _ = window.set_overlay_icon(badge);
    }
}

// Updates the tray icon, tooltip and (macOS) menu-bar title for an unread count.
fn apply_tray_badge(app: &tauri::AppHandle, count: u32) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let (tooltip, title, icon) = if count > 0 {
        let label = unread_label(count);
        // Badge tooltip so the count is visible on hover
        (
            format!("{APP_NAME} ({label} unread)"),
            label,
            load_badged_tray_icon(),
        )
    } else {
        (APP_NAME.to_string(), String::new(), load_base_tray_icon())
    };
    let _ = tray.set_tooltip(Some(&tooltip));
    if let Ok(icon) = icon {
        let _ = tray.set_icon(Some(icon));
        #[cfg(target_os = "macos")]
        let _ = tray.set_icon_as_template(true);
    }
    #[cfg(target_os = "macos")]
    let _ = tray.set_title(Some(&title));
    #[cfg(not(target_os = "macos"))]
    let _ = title;
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    // The individual toggles (Launch at Login, Start in Tray) live in the
    // settings window now — keeping duplicates here as check items would let
    // their check state drift from settings.json.
    let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &show,
            &settings_item,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    #[allow(unused_mut)] // `tray` is only reassigned on macOS (template flag)
    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(load_base_tray_icon()?)
        .tooltip(APP_NAME)
        .menu(&menu)
        .show_menu_on_left_click(false);

    // Template rendering is a macOS-only concept (light/dark menu-bar tinting).
    #[cfg(target_os = "macos")]
    {
        tray = tray.icon_as_template(true);
    }

    tray.on_menu_event(|app, event| match event.id().as_ref() {
        "show" => show_main_window(app),
        "settings" => show_settings_window(app),
        "quit" => quit_app(app),
        _ => {}
    })
    .on_tray_icon_event(|tray, event| {
        // Left-click toggles window visibility
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            let app = tray.app_handle();
            if let Some(window) = main_window(app) {
                if window.is_visible().unwrap_or(false) {
                    hide_main_window(app);
                } else {
                    show_main_window(app);
                }
            }
        }
    })
    .build(app)?;

    Ok(())
}

// =============================================================================
// macOS application menu
// =============================================================================

// Builds the native macOS application menu bar (Google Messages / Edit / View /
// Window) so standard shortcuts like Cmd+, (Settings), Cmd+R (Reload), and
// Cmd+=/Cmd+-/Cmd+0 (Zoom) work natively. Windows and Linux omit a top-of-window
// menu bar strip so the window stays modern and frameless, handling those
// shortcuts directly via keydown listeners.
#[cfg(target_os = "macos")]
fn build_macos_app_menu(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::Submenu;

    let app_settings =
        MenuItem::with_id(app, "app_settings", "Settings…", true, Some("CmdOrCtrl+,"))?;
    let app_quit = MenuItem::with_id(
        app,
        "app_quit",
        format!("Quit {APP_NAME}"),
        true,
        Some("CmdOrCtrl+Q"),
    )?;
    let app_submenu = Submenu::with_items(
        app,
        APP_NAME,
        true,
        &[
            &PredefinedMenuItem::about(app, None, None)?,
            &PredefinedMenuItem::separator(app)?,
            &app_settings,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &app_quit,
        ],
    )?;

    let edit_submenu = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;

    let view_reload = MenuItem::with_id(app, "view_reload", "Reload", true, Some("CmdOrCtrl+R"))?;
    let view_zoom_reset = MenuItem::with_id(
        app,
        "view_zoom_reset",
        "Actual Size",
        true,
        Some("CmdOrCtrl+0"),
    )?;
    let view_zoom_in =
        MenuItem::with_id(app, "view_zoom_in", "Zoom In", true, Some("CmdOrCtrl+="))?;
    let view_zoom_out =
        MenuItem::with_id(app, "view_zoom_out", "Zoom Out", true, Some("CmdOrCtrl+-"))?;
    let view_submenu = Submenu::with_items(
        app,
        "View",
        true,
        &[
            &view_reload,
            &PredefinedMenuItem::separator(app)?,
            &view_zoom_reset,
            &view_zoom_in,
            &view_zoom_out,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::fullscreen(app, None)?,
        ],
    )?;

    let window_submenu = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;

    let menu = Menu::with_items(
        app,
        &[&app_submenu, &edit_submenu, &view_submenu, &window_submenu],
    )?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| match event.id().as_ref() {
        "app_settings" => show_settings_window(app),
        "app_quit" => quit_app(app),
        "view_reload" if is_main_focused(app) => {
            eval_in_main(app, "window.location.reload();");
        }
        "view_zoom_reset" if is_main_focused(app) => apply_zoom_action(app, "reset"),
        "view_zoom_in" if is_main_focused(app) => apply_zoom_action(app, "in"),
        "view_zoom_out" if is_main_focused(app) => apply_zoom_action(app, "out"),
        _ => {}
    });

    Ok(())
}

// =============================================================================
// Media permissions (Windows / Linux)
// =============================================================================

// Auto-allow microphone, camera, and notification permission requests in the
// WebView2 webview so calls and background notifications work without prompting
// the user every session. Mirrors the pattern wry uses internally for clipboard.
#[cfg(windows)]
fn grant_media_permissions(window: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_CAMERA,
        COREWEBVIEW2_PERMISSION_KIND_MICROPHONE, COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS,
        COREWEBVIEW2_PERMISSION_STATE_ALLOW,
    };
    use webview2_com::PermissionRequestedEventHandler;

    let _ = window.with_webview(|webview| unsafe {
        let controller = webview.controller();
        if let Ok(core) = controller.CoreWebView2() {
            let mut token = 0i64;
            let _ = core.add_PermissionRequested(
                &PermissionRequestedEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                        args.PermissionKind(&mut kind)?;
                        if kind == COREWEBVIEW2_PERMISSION_KIND_MICROPHONE
                            || kind == COREWEBVIEW2_PERMISSION_KIND_CAMERA
                            || kind == COREWEBVIEW2_PERMISSION_KIND_NOTIFICATIONS
                        {
                            args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW)?;
                        }
                    }
                    Ok(())
                })),
                &mut token,
            );
        }
    });
}

// Enable WebKitGTK media-stream/WebRTC support (off by default) and auto-allow
// user-media permission requests so voice/video calls work on Linux.
#[cfg(target_os = "linux")]
fn grant_media_permissions(window: &tauri::WebviewWindow) {
    use webkit2gtk::glib::ObjectExt;
    use webkit2gtk::{PermissionRequestExt, SettingsExt, UserMediaPermissionRequest, WebViewExt};

    let _ = window.with_webview(|webview| {
        let wv = webview.inner();
        if let Some(settings) = WebViewExt::settings(&wv) {
            settings.set_enable_media_stream(true);
            settings.set_enable_webrtc(true);
        }
        wv.connect_permission_request(|_, request| {
            if request.is::<UserMediaPermissionRequest>() {
                request.allow();
                return true;
            }
            false
        });
    });
}

// =============================================================================
// Tauri commands (invoked from the injected page script and the settings page)
// =============================================================================
// Adding a command means registering it in three places: `generate_handler!`
// in `run` below, the command list in build.rs, and capabilities/default.json.

#[tauri::command]
fn get_settings(app: tauri::AppHandle) -> Settings {
    load_settings(&app)
}

#[tauri::command]
fn set_settings(app: tauri::AppHandle, mut settings: Settings) {
    // The settings page doesn't manage window geometry, so its payload always
    // carries the field's default — keep the stored value instead of wiping it.
    settings.main_window_size = load_settings(&app).main_window_size;
    // Never persist a start URL the main window could not load.
    settings.start_url = settings
        .start_url
        .filter(|url| parse_start_url(url).is_some());

    // Keep the OS autostart entry in sync with the persisted launch-at-login flag.
    let mgr = app.autolaunch();
    let enabled = mgr.is_enabled().unwrap_or(false);
    if settings.launch_at_login && !enabled {
        let _ = mgr.enable();
    } else if !settings.launch_at_login && enabled {
        let _ = mgr.disable();
    }
    save_settings(&app, &settings);

    // Apply what can take effect immediately. (start_url and start_in_tray
    // naturally apply on next launch.)
    apply_runtime_flags(&app, &settings);

    let theme = parse_theme(settings.theme.as_deref());
    for window in app.webview_windows().values() {
        let _ = window.set_theme(theme);
        let effective = theme
            .or_else(|| window.theme().ok())
            .unwrap_or(tauri::Theme::Light);
        let _ = window.set_background_color(Some(theme_bg_color(effective)));
    }
    eval_in_main(
        &app,
        &format!(
            "window.__GM_SHOW_NOTIFICATIONS = {}; window.__GM_BACKGROUND_KEEP_ALIVE = {};",
            settings.show_notifications, settings.background_keep_alive
        ),
    );
}

// Opens the settings window; invoked by the gear button injected into the
// Messages web UI (the tray menu calls show_settings_window directly).
#[tauri::command]
async fn open_settings(app: tauri::AppHandle) {
    ipc_log!("open_settings invoked");
    show_settings_window(&app);
}

// Invoked from JS when a desktop notification is clicked so the main window is
// unminimized, unhidden from the tray, and brought to the foreground.
#[tauri::command]
fn focus_main_window(app: tauri::AppHandle) {
    ipc_log!("focus_main_window invoked");
    show_main_window(&app);
}

#[tauri::command]
fn prompt_takeover_session(app: tauri::AppHandle) {
    ipc_log!("prompt_takeover_session invoked");
    open_and_prompt_takeover(&app);
}

// Called from JS when "Google Messages is open on another computer" appears or
// clears. When `yielded` becomes true, we pause background keep-alive/reloads
// (allowing OS sleep) and pop a single notification offering to switch back.
#[tauri::command]
fn set_session_yielded(app: tauri::AppHandle, yielded: bool) {
    ipc_log!("set_session_yielded({yielded})");

    let was_yielded = SESSION_YIELDED.swap(yielded, Ordering::Relaxed);
    sync_os_keep_alive(&app);

    if yielded {
        reset_watchdog_counters();
        // Only on the transition into the yielded state, so it pops once.
        if !was_yielded {
            let main_focused = main_window(&app).is_some_and(|w| {
                w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false)
            });
            if !main_focused && SHOW_NOTIFICATIONS.load(Ordering::Relaxed) {
                send_os_notification(
                    &app,
                    TAKEOVER_NOTIFICATION_ID.to_string(),
                    format!("{APP_NAME} is open on another computer"),
                    "Click to open and move the session here, or ignore to let the app sleep."
                        .to_string(),
                    true,
                );
            }
        }
    } else {
        close_os_notification(&app, TAKEOVER_NOTIFICATION_ID);
    }
}

#[tauri::command]
fn send_native_notification(
    app: tauri::AppHandle,
    id: String,
    title: String,
    body: String,
    kind: String,
) {
    ipc_log!("send_native_notification(id={id}, kind={kind}, title={title})");
    if !SHOW_NOTIFICATIONS.load(Ordering::Relaxed) {
        return;
    }
    send_os_notification(&app, id, title, body, kind == "takeover");
}

#[tauri::command]
fn close_native_notification(app: tauri::AppHandle, id: String) {
    ipc_log!("close_native_notification({id})");
    close_os_notification(&app, &id);
}

#[tauri::command]
fn clear_native_notifications(app: tauri::AppHandle) {
    ipc_log!("clear_native_notifications");
    clear_os_notifications(&app);
}

#[tauri::command]
fn report_heartbeat(app: tauri::AppHandle, healthy: bool, reason: Option<String>) {
    ipc_log!("report_heartbeat(healthy={healthy}, reason={reason:?})");

    PENDING_HEARTBEAT_PINGS.store(0, Ordering::Relaxed);
    if healthy || reason.as_deref() == Some("offline") || !keep_alive_active() {
        UNHEALTHY_STREAK.store(0, Ordering::Relaxed);
        return;
    }

    let streak = UNHEALTHY_STREAK.fetch_add(1, Ordering::Relaxed) + 1;
    if streak >= 2 {
        UNHEALTHY_STREAK.store(0, Ordering::Relaxed);
        try_reload_main_window(&app, "stalled connection");
    }
}

#[tauri::command]
fn adjust_zoom(app: tauri::AppHandle, action: String) {
    ipc_log!("adjust_zoom({action})");
    apply_zoom_action(&app, &action);
}

// Opens an external (non-Messages) URL in the OS default browser. Used for links
// like the Google Fi account page that point outside messages.google.com — those
// open with target="_blank" and would otherwise be dropped by the webview.
#[tauri::command]
fn open_external(app: tauri::AppHandle, url: String) {
    use tauri_plugin_opener::OpenerExt;
    if url.starts_with("https://") || url.starts_with("http://") {
        if let Err(e) = app.opener().open_url(&url, None::<&str>) {
            eprintln!("[open_external] failed to open {url}: {e}");
        }
    }
}

// Called from JS when the unread count changes
#[tauri::command]
fn set_tray_badge(app: tauri::AppHandle, count: u32) {
    // Fires on page load via the injected title observer, so its presence in
    // dev stderr doubles as proof that IPC from the remote origin works.
    ipc_log!("set_tray_badge({count})");

    LAST_UNREAD_COUNT.store(count, Ordering::Relaxed);
    if LAST_TRAY_COUNT.swap(count, Ordering::Relaxed) == count {
        return;
    }

    if let Some(window) = main_window(&app) {
        apply_window_badge(&window, count);
    }
    apply_tray_badge(&app, count);
}

// =============================================================================
// App entry point
// =============================================================================

// Builds the main window pointing at Google Messages (hidden if the user chose
// "Start in Tray"), restoring the remembered geometry.
fn build_main_window(
    app: &tauri::AppHandle,
    settings: &Settings,
) -> Result<tauri::WebviewWindow, Box<dyn std::error::Error>> {
    // A missing or unusable start URL (e.g. a hand-edited settings.json) falls
    // back to the default instead of failing startup.
    let start_url = match settings.start_url.as_deref().and_then(parse_start_url) {
        Some(url) => url,
        None => DEFAULT_START_URL.parse()?,
    };
    // Restore the remembered window size, clamped to the window minimum in
    // case the persisted file was edited or corrupted.
    let saved_geom = settings.main_window_size.as_ref();
    let (width, height) = saved_geom
        .map(|s| (s.width.max(MIN_MAIN_SIZE.0), s.height.max(MIN_MAIN_SIZE.1)))
        .unwrap_or(DEFAULT_MAIN_SIZE);
    let init_script = INIT_SCRIPT
        .replace(
            "__GM_NOTIFICATIONS_DEFAULT__",
            &settings.show_notifications.to_string(),
        )
        .replace(
            "__GM_KEEP_ALIVE_DEFAULT__",
            &settings.background_keep_alive.to_string(),
        );
    let explicit_theme = parse_theme(settings.theme.as_deref());

    let mut builder = themed_window_builder(
        app,
        MAIN_WINDOW,
        WebviewUrl::External(start_url),
        explicit_theme,
    )
    .title(APP_NAME)
    .inner_size(width, height)
    .min_inner_size(MIN_MAIN_SIZE.0, MIN_MAIN_SIZE.1)
    .visible(!settings.start_in_tray)
    .background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Disabled)
    .initialization_script_for_all_frames(&init_script);
    #[cfg(not(target_os = "macos"))]
    {
        builder = builder.skip_taskbar(settings.start_in_tray);
    }

    // Native new-window requests (all frames): Messages links navigate the
    // main window, other http(s) links open in the OS default browser.
    let new_win_app = app.clone();
    builder = builder.on_new_window(move |url, _features| {
        let scheme = url.scheme();
        if scheme == "https" || scheme == "http" {
            if url.host_str() == Some(MESSAGES_HOST) {
                if let Some(main) = main_window(&new_win_app) {
                    let _ = main.navigate(url);
                }
            } else {
                open_external(new_win_app.clone(), url.to_string());
            }
        }
        tauri::webview::NewWindowResponse::Deny
    });

    // Restore the saved position only if it is still on a connected monitor.
    let saved_position = saved_geom
        .and_then(|g| g.x.zip(g.y))
        .filter(|&(x, y)| is_position_on_screen(app, x, y, width));
    builder = match saved_position {
        Some((x, y)) => builder.position(x, y),
        None => builder.center(),
    };
    if saved_geom.is_some_and(|g| g.maximized) {
        builder = builder.maximized(true);
    }
    if let Some(ua) = USER_AGENT {
        builder = builder.user_agent(ua);
    }

    let window = builder.build()?;
    if explicit_theme.is_none() {
        if let Ok(os_theme) = window.theme() {
            let _ = window.set_background_color(Some(theme_bg_color(os_theme)));
        }
    }
    Ok(window)
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // If a second instance is launched, focus the existing window
            show_main_window(app);
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let handle = app.handle().clone();
            let mut settings = load_settings(&handle);
            // Keep the persisted launch-at-login flag in sync with the OS/plugin.
            settings.launch_at_login = app.autolaunch().is_enabled().unwrap_or(false);
            save_settings(&handle, &settings);

            SHOW_NOTIFICATIONS.store(settings.show_notifications, Ordering::Relaxed);
            BACKGROUND_KEEP_ALIVE.store(settings.background_keep_alive, Ordering::Relaxed);
            #[cfg(target_os = "macos")]
            macos_native::init_notifications(&handle);
            sync_os_keep_alive(&handle);

            #[allow(unused_variables)]
            let window = build_main_window(&handle, &settings)?;

            // On Windows & Linux, auto-grant mic/camera so calls don't prompt
            // every session (or fail outright on Linux where media-stream is
            // disabled by default). macOS is handled by wry (auto-grant) +
            // Info.plist.
            #[cfg(any(windows, target_os = "linux"))]
            grant_media_permissions(&window);

            build_tray(app)?;
            #[cfg(target_os = "macos")]
            build_macos_app_menu(app)?;
            #[cfg(target_os = "macos")]
            if settings.start_in_tray {
                let _ = handle.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
            start_background_listener(handle);
            Ok(())
        })
        // Close the main window to tray instead of quitting; other windows
        // (Settings, Calls, Voicemail) close normally.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == MAIN_WINDOW {
                    hide_main_window(window.app_handle());
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            set_tray_badge,
            open_external,
            get_settings,
            set_settings,
            open_settings,
            focus_main_window,
            adjust_zoom,
            send_native_notification,
            close_native_notification,
            clear_native_notifications,
            report_heartbeat,
            set_session_yielded,
            prompt_takeover_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// =============================================================================
// macOS-native notifications & App Nap control
// =============================================================================

#[cfg(target_os = "macos")]
// NSUserNotificationCenter works in ad-hoc signed and dev builds without an
// Apple Developer provisioning profile (unlike UNUserNotificationCenter).
#[allow(deprecated)]
mod macos_native {
    use objc2::{
        define_class, msg_send, rc::Retained, runtime::ProtocolObject, DefinedClass,
        MainThreadMarker, MainThreadOnly,
    };
    use objc2_foundation::{
        ns_string, NSActivityOptions, NSObject, NSObjectProtocol, NSProcessInfo, NSString,
        NSUserNotification, NSUserNotificationActivationType, NSUserNotificationCenter,
        NSUserNotificationCenterDelegate, NSUserNotificationDefaultSoundName,
    };
    use std::cell::RefCell;

    struct GmNotificationDelegateIvars {
        app: tauri::AppHandle,
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[ivars = GmNotificationDelegateIvars]
        struct GmNotificationDelegate;

        unsafe impl NSObjectProtocol for GmNotificationDelegate {}

        #[allow(deprecated)]
        unsafe impl NSUserNotificationCenterDelegate for GmNotificationDelegate {
            #[unsafe(method(userNotificationCenter:didActivateNotification:))]
            fn did_activate(
                &self,
                center: &NSUserNotificationCenter,
                notification: &NSUserNotification,
            ) {
                let id = notification
                    .identifier()
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                let activation = notification.activationType();
                center.removeDeliveredNotification(notification);
                DELIVERED_NOTIFS.with(|slot| slot.borrow_mut().remove(&id));
                let clicked = activation == NSUserNotificationActivationType::ContentsClicked
                    || activation == NSUserNotificationActivationType::ActionButtonClicked;
                if !clicked {
                    return;
                }
                crate::handle_notification_click(
                    &self.ivars().app,
                    &id,
                    crate::is_takeover_notification(&id),
                );
            }

            #[unsafe(method(userNotificationCenter:shouldPresentNotification:))]
            fn should_present(
                &self,
                _center: &NSUserNotificationCenter,
                _notification: &NSUserNotification,
            ) -> bool {
                true
            }
        }
    );

    impl GmNotificationDelegate {
        fn new(mtm: MainThreadMarker, app: tauri::AppHandle) -> Retained<Self> {
            let delegate = mtm
                .alloc::<GmNotificationDelegate>()
                .set_ivars(GmNotificationDelegateIvars { app });
            unsafe { msg_send![super(delegate), init] }
        }
    }

    thread_local! {
        static ACTIVITY_TOKEN: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> =
            const { RefCell::new(None) };
        static NOTIF_DELEGATE: RefCell<Option<Retained<GmNotificationDelegate>>> =
            const { RefCell::new(None) };
        static DELIVERED_NOTIFS: RefCell<std::collections::HashMap<String, Retained<NSUserNotification>>> =
            RefCell::new(std::collections::HashMap::new());
    }

    pub fn init_notifications(app: &tauri::AppHandle) {
        if tauri::is_dev() {
            let _ = mac_notification_sys::set_application("com.apple.Terminal");
        }
        let app_clone = app.clone();
        let _ = app.run_on_main_thread(move || {
            let Some(mtm) = MainThreadMarker::new() else {
                return;
            };
            let delegate = GmNotificationDelegate::new(mtm, app_clone);
            let proto = ProtocolObject::from_ref(&*delegate);
            #[allow(deprecated)]
            unsafe {
                NSUserNotificationCenter::defaultUserNotificationCenter().setDelegate(Some(proto));
            }
            NOTIF_DELEGATE.with(|slot| {
                *slot.borrow_mut() = Some(delegate);
            });
        });
    }

    pub fn set_keep_alive(app: &tauri::AppHandle, enabled: bool) {
        let _ = app.run_on_main_thread(move || {
            ACTIVITY_TOKEN.with(|slot| {
                let mut guard = slot.borrow_mut();
                if enabled && guard.is_none() {
                    let info = NSProcessInfo::processInfo();
                    let token = info.beginActivityWithOptions_reason(
                        NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
                        ns_string!("Keep Google Messages connected in background"),
                    );
                    *guard = Some(token);
                } else if !enabled {
                    if let Some(token) = guard.take() {
                        let info = NSProcessInfo::processInfo();
                        unsafe { info.endActivity(&token) };
                    }
                }
            });
        });
    }

    pub fn send_notification(
        app: &tauri::AppHandle,
        id: String,
        title: String,
        body: String,
        is_takeover: bool,
    ) {
        let _ = app.run_on_main_thread(move || {
            #[allow(deprecated)]
            {
                let center = NSUserNotificationCenter::defaultUserNotificationCenter();

                DELIVERED_NOTIFS.with(|slot| {
                    if let Some(prev) = slot.borrow_mut().remove(&id) {
                        center.removeDeliveredNotification(&prev);
                    }
                });

                let notif = NSUserNotification::new();
                notif.setTitle(Some(&NSString::from_str(&title)));
                if !body.is_empty() {
                    notif.setInformativeText(Some(&NSString::from_str(&body)));
                }
                notif.setIdentifier(Some(&NSString::from_str(&id)));
                unsafe {
                    notif.setSoundName(Some(NSUserNotificationDefaultSoundName));
                }
                if is_takeover {
                    notif.setHasActionButton(true);
                    notif.setActionButtonTitle(ns_string!("Switch Here"));
                    notif.setOtherButtonTitle(ns_string!("No"));
                }
                center.deliverNotification(&notif);
                DELIVERED_NOTIFS.with(|slot| {
                    slot.borrow_mut().insert(id, notif);
                });
            }
        });
    }

    pub fn close_notification(app: &tauri::AppHandle, id: &str) {
        let id = id.to_string();
        let _ = app.run_on_main_thread(move || {
            #[allow(deprecated)]
            {
                let center = NSUserNotificationCenter::defaultUserNotificationCenter();
                let removed = DELIVERED_NOTIFS.with(|slot| slot.borrow_mut().remove(&id));
                if let Some(notif) = removed {
                    center.removeDeliveredNotification(&notif);
                } else {
                    let temp = NSUserNotification::new();
                    temp.setIdentifier(Some(&NSString::from_str(&id)));
                    center.removeDeliveredNotification(&temp);
                }
            }
        });
    }

    // Removes every delivered message notification. The takeover notification
    // is left in place: it is cleared only when the session comes back.
    pub fn clear_notifications(app: &tauri::AppHandle) {
        let _ = app.run_on_main_thread(move || {
            #[allow(deprecated)]
            {
                DELIVERED_NOTIFS.with(|slot| {
                    slot.borrow_mut()
                        .retain(|id, _| crate::is_takeover_notification(id))
                });
                let center = NSUserNotificationCenter::defaultUserNotificationCenter();
                for notif in center.deliveredNotifications().iter() {
                    let is_takeover = notif
                        .identifier()
                        .is_some_and(|id| crate::is_takeover_notification(&id.to_string()));
                    if !is_takeover {
                        center.removeDeliveredNotification(&notif);
                    }
                }
            }
        });
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    // Returns the text between the first `open` and the next `close` after it.
    fn between<'a>(text: &'a str, open: &str, close: &str) -> &'a str {
        let start = text.find(open).expect("opening marker not found") + open.len();
        let len = text[start..].find(close).expect("closing marker not found");
        &text[start..start + len]
    }

    // Commands registered with the invoke handler in `run`.
    fn handler_commands() -> BTreeSet<String> {
        // Built with concat! so this test's own source does not match first.
        let marker = concat!("generate_handler", "![");
        between(include_str!("lib.rs"), marker, "]")
            .split(',')
            .map(|name| name.trim().to_string())
            .filter(|name| !name.is_empty())
            .collect()
    }

    // Commands declared in the ACL manifest by build.rs.
    fn manifest_commands() -> BTreeSet<String> {
        between(include_str!("../build.rs"), ".commands(&[", "])")
            .split(',')
            .map(|name| name.trim().trim_matches('"').to_string())
            .filter(|name| !name.is_empty())
            .collect()
    }

    // Commands allowed by capabilities/default.json ("allow-foo-bar" -> "foo_bar").
    fn capability_commands() -> BTreeSet<String> {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json"))
                .expect("capabilities/default.json is valid JSON");
        capability["permissions"]
            .as_array()
            .expect("permissions is an array")
            .iter()
            .filter_map(|permission| permission.as_str()?.strip_prefix("allow-"))
            .map(|name| name.replace('-', "_"))
            .collect()
    }

    // A command has to be listed in three places to be callable from the page.
    // This fails as soon as one of them is missed.
    #[test]
    fn command_lists_agree() {
        let handler = handler_commands();
        assert!(
            !handler.is_empty(),
            "no commands found in generate_handler!"
        );
        assert_eq!(
            handler,
            manifest_commands(),
            "generate_handler! in lib.rs and .commands(..) in build.rs differ"
        );
        assert_eq!(
            handler,
            capability_commands(),
            "generate_handler! in lib.rs and capabilities/default.json differ"
        );
    }

    // The settings placeholders are substituted by plain string replacement,
    // so each must appear exactly once in the injected script.
    #[test]
    fn init_script_has_each_placeholder_once() {
        for token in ["__GM_NOTIFICATIONS_DEFAULT__", "__GM_KEEP_ALIVE_DEFAULT__"] {
            assert_eq!(INIT_SCRIPT.matches(token).count(), 1, "{token}");
        }
    }

    #[test]
    fn unread_label_caps_at_99() {
        assert_eq!(unread_label(1), "1");
        assert_eq!(unread_label(99), "99");
        assert_eq!(unread_label(100), "99+");
    }

    #[test]
    fn parse_theme_only_accepts_explicit_light_or_dark() {
        assert_eq!(parse_theme(Some("light")), Some(tauri::Theme::Light));
        assert_eq!(parse_theme(Some("dark")), Some(tauri::Theme::Dark));
        assert_eq!(parse_theme(Some("system")), None);
        assert_eq!(parse_theme(None), None);
    }

    #[test]
    fn parse_start_url_accepts_only_http_and_https() {
        assert!(parse_start_url(DEFAULT_START_URL).is_some());
        assert!(parse_start_url("  http://example.com/x  ").is_some());
        assert!(parse_start_url("ftp://example.com").is_none());
        assert!(parse_start_url("javascript:alert(1)").is_none());
        assert!(parse_start_url("not a url").is_none());
        assert!(parse_start_url("").is_none());
    }

    // settings.json files written by older versions lack the newer fields.
    #[test]
    fn settings_from_older_versions_get_defaults() {
        let settings: Settings = serde_json::from_str(r#"{"start_in_tray": true}"#).unwrap();
        assert!(settings.start_in_tray);
        assert!(settings.show_notifications);
        assert!(settings.background_keep_alive);
        assert!(settings.start_url.is_none());
        assert!(settings.main_window_size.is_none());
    }

    #[test]
    fn settings_round_trip_keeps_window_geometry() {
        let settings = Settings {
            main_window_size: Some(WindowSize {
                width: 900.0,
                height: 700.0,
                x: Some(10.0),
                y: None,
                maximized: true,
            }),
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        let geometry = serde_json::from_str::<Settings>(&json)
            .unwrap()
            .main_window_size
            .unwrap();
        assert_eq!((geometry.width, geometry.height), (900.0, 700.0));
        assert_eq!((geometry.x, geometry.y), (Some(10.0), None));
        assert!(geometry.maximized);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn takeover_notification_ids_are_recognized() {
        assert!(is_takeover_notification(TAKEOVER_NOTIFICATION_ID));
        assert!(!is_takeover_notification("gm-conv-Alice"));
    }
}
