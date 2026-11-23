use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Mutex, OnceLock,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder,
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
const POPOVER_WINDOW: &str = "popover";
const TRAY_ID: &str = "main";
// Identifier of the "open on another computer" notification.
const TAKEOVER_NOTIFICATION_ID: &str = "gm-takeover";

// Default start URL if the user has not customized it. The canonical web
// entry point: when already paired this lands straight on the (cached)
// conversation list; when not paired it redirects to the QR pairing page on
// its own. Loading the pairing sub-route directly added an extra hop.
const DEFAULT_START_URL: &str = "https://messages.google.com/web/";
const MESSAGES_HOST: &str = "messages.google.com";

// Quick-action emoji offered on message notifications until the user picks
// another (or turns it off) in Settings.
const DEFAULT_QUICK_ACTION: &str = "👍";
// Upper bound on the stored quick action, in bytes: room for any single emoji
// (including multi-part ones), not for arbitrary text.
const MAX_QUICK_ACTION_BYTES: usize = 32;

// Main window geometry (logical pixels): size used until one is remembered,
// and the minimum the window can be resized (or restored) to.
const DEFAULT_MAIN_SIZE: (f64, f64) = (1100.0, 780.0);
const MIN_MAIN_SIZE: (f64, f64) = (400.0, 500.0);

// Tray popover size and its gap from the tray icon and screen edges (logical
// pixels), and how many recent conversations it lists. The height is a
// maximum: the window shrinks to fit its content, down to the minimum.
const POPOVER_SIZE: (f64, f64) = (360.0, 460.0);
const POPOVER_MIN_HEIGHT: f64 = 160.0;
const POPOVER_GAP: f64 = 6.0;
// Corner radius of the macOS popover's material; keep in step with the
// border-radius in popover.html.
#[cfg(target_os = "macos")]
const POPOVER_CORNER_RADIUS: f64 = 12.0;
const MAX_RECENT_CONVERSATIONS: usize = 5;
// Longest contact photo URL passed to the popover (the page scales photos to
// a few KB; keep in step with MAX_PHOTO_CHARS in init_script.js).
const MAX_PHOTO_CHARS: usize = 100_000;
// Connection states the page may report for the popover: connected, open on
// another computer, no internet, can't reach the phone, signed out or not
// paired, and still loading.
const CONNECTION_STATES: [&str; 6] = [
    "connected",
    "elsewhere",
    "offline",
    "phone",
    "signed-out",
    "loading",
];
// The right-button state that opens the popover or tray menu: macOS menus
// open on press, Windows menus on release (the only right-click it reports).
#[cfg(target_os = "macos")]
const TRAY_MENU_BUTTON_STATE: MouseButtonState = MouseButtonState::Down;
#[cfg(windows)]
const TRAY_MENU_BUTTON_STATE: MouseButtonState = MouseButtonState::Up;
// Clicking the tray icon while the popover is open first takes focus from the
// popover, which hides it; a click this soon after that must not reopen it.
const POPOVER_REOPEN_GUARD: Duration = Duration::from_millis(400);

// Minimum gap between watchdog-triggered reloads so auto-reload can never loop.
const AUTO_RELOAD_COOLDOWN_SECS: u64 = 120;

// What a Windows toast reports back when it is clicked: `launch` for the body,
// and each button's `arguments`. The reply text arrives under the input's id.
#[cfg(any(windows, test))]
const TOAST_ARG_OPEN: &str = "open";
#[cfg(any(windows, test))]
const TOAST_ARG_REPLY: &str = "reply";
#[cfg(any(windows, test))]
const TOAST_ARG_QUICK: &str = "quick";
#[cfg(any(windows, test))]
const TOAST_ARG_READ: &str = "read";
#[cfg(any(windows, test))]
const TOAST_ARG_SWITCH: &str = "switch";
#[cfg(any(windows, test))]
const TOAST_ARG_NO: &str = "no";
#[cfg(any(windows, test))]
const TOAST_REPLY_INPUT: &str = "reply";

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
static TRAY_POPOVER: AtomicBool = AtomicBool::new(true);
// True when "Google Messages is open on another computer" is active and the
// user hasn't switched the session back here; while true, background keep-alive
// and auto-refresh are paused so the OS can sleep the app normally.
static SESSION_YIELDED: AtomicBool = AtomicBool::new(false);
// Watchdog counters for the background heartbeat loop.
static PENDING_HEARTBEAT_PINGS: AtomicU32 = AtomicU32::new(0);
static UNHEALTHY_STREAK: AtomicU32 = AtomicU32::new(0);
static LAST_AUTO_RELOAD_SECS: AtomicU64 = AtomicU64::new(0);

// The latest conversations the page reported for the tray popover, and when
// the popover last hid itself because it lost focus.
static RECENT_CONVERSATIONS: Mutex<Vec<RecentConversation>> = Mutex::new(Vec::new());
// The page's connection state for the popover footer (see CONNECTION_STATES);
// empty until the page reports one.
static CONNECTION_STATE: Mutex<String> = Mutex::new(String::new());
static POPOVER_HIDDEN_AT: Mutex<Option<Instant>> = Mutex::new(None);
// The popover's height as its page last asked for (fit to its content), and
// the tray icon it was last opened from, so a new height keeps it anchored.
static POPOVER_HEIGHT: Mutex<f64> = Mutex::new(POPOVER_SIZE.1);
static POPOVER_ANCHOR: Mutex<Option<PopoverAnchor>> = Mutex::new(None);

// The tray menu, attached to the icon only while it is open, so that clicks
// reach the app and right-click can open the popover instead (with a menu
// attached, macOS opens it on a left-click too and the app never sees the
// click). Linux trays report no clicks, so there the menu stays attached.
#[cfg(not(target_os = "linux"))]
static TRAY_MENU: OnceLock<Menu<tauri::Wry>> = OnceLock::new();

// Notifications currently on screen on Linux, by our notification id.
#[cfg(target_os = "linux")]
static LINUX_NOTIFS: OnceLock<std::sync::Mutex<LinuxNotifMap>> = OnceLock::new();

// =============================================================================
// Types
// =============================================================================

#[cfg(target_os = "linux")]
type LinuxNotifMap = std::collections::HashMap<String, notify_rust::NotificationHandle>;

// What an OS notification is for, which decides the buttons it offers.
#[derive(Clone, Debug, PartialEq)]
enum NotificationKind {
    // "Open on another computer": offers to move the session here.
    Takeover,
    // A message from one identifiable conversation: offers a reply field,
    // "mark as read", and the quick-action emoji if one is configured.
    Conversation { quick_action: Option<String> },
    // Anything else: no buttons, clicking opens the window.
    Plain,
}

// A conversation shown in the tray popover, as read from the page's
// conversation list.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
struct RecentConversation {
    title: String,
    // The sidebar's preview of the latest message ("You: …" when it was the
    // user's own).
    snippet: String,
    // Relative time as the page shows it ("2 min", "Mon"); may be empty.
    time: String,
    unread: bool,
    // False when another conversation has the same name, so a reply could
    // not be routed to the right one.
    can_reply: bool,
    // The contact's photo as a data:image URL (or an https URL), or empty.
    photo: String,
}

// Everything the popover page needs to draw itself.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PopoverState {
    conversations: Vec<RecentConversation>,
    // One of CONNECTION_STATES, or empty if the page hasn't said.
    connection: String,
    quick_action: String,
    theme: Option<String>,
}

// A rectangle in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PxRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

// Where the popover was last opened: the tray icon and its monitor's work
// area (physical pixels), and that monitor's scale factor.
#[derive(Clone, Copy, Debug, PartialEq)]
struct PopoverAnchor {
    icon: PxRect,
    work: PxRect,
    scale: f64,
}

// Something the user did from a conversation notification without opening
// the window.
enum NotificationAction {
    // Send this text to the conversation (typed reply or quick-action emoji).
    Reply(String),
    MarkRead,
}

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
    /// When true, left-clicking the tray icon shows the recent-messages
    /// popover; when false it shows or hides the main window.
    tray_popover: bool,
    /// Emoji sent by the quick-action button on message notifications; empty
    /// turns the button off.
    quick_action: String,
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
            tray_popover: true,
            quick_action: DEFAULT_QUICK_ACTION.to_string(),
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
    TRAY_POPOVER.store(settings.tray_popover, Ordering::Relaxed);
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
    // Toasts left behind could not do anything once the app is gone.
    #[cfg(windows)]
    windows_native::remove_all_notifications();
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
fn is_takeover_notification(id: &str) -> bool {
    id.starts_with(TAKEOVER_NOTIFICATION_ID)
}

// Carries out a reply or mark-as-read chosen on a notification by handing it
// to the page script, which drives the Messages UI. The window stays where it
// is; the script shows it only if the action cannot be completed.
fn handle_notification_action(
    app: &tauri::AppHandle,
    id: &str,
    title: &str,
    action: NotificationAction,
) {
    let (name, text) = match action {
        NotificationAction::Reply(text) => ("reply", text),
        NotificationAction::MarkRead => ("read", String::new()),
    };
    if let (Ok(id), Ok(title), Ok(text)) = (
        serde_json::to_string(id),
        serde_json::to_string(title),
        serde_json::to_string(&text),
    ) {
        eval_in_main(
            app,
            &format!(
                "window.__gmNotificationAction && window.__gmNotificationAction({id}, {title}, \"{name}\", {text});"
            ),
        );
    }
}

// Handles a click on one of our OS notifications: the takeover notification
// opens the "move session here?" prompt; a message notification shows the
// window and hands the click to the page, which runs the web app's own click
// handler so the conversation opens.
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
// D-Bus via notify-rust. Windows: native toasts in `windows_native`.

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
    kind: NotificationKind,
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
        // Buttons. (The freedesktop spec has no text field, so there is no
        // typed reply here; desktops usually show about three buttons.)
        n.action("default", "Open");
        match &kind {
            NotificationKind::Takeover => {
                n.action("use_here", "Switch Here").action("no", "No");
            }
            NotificationKind::Conversation { quick_action } => {
                if let Some(emoji) = quick_action {
                    n.action("quick", emoji);
                }
                n.action("read", "Mark as read");
            }
            NotificationKind::Plain => {}
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
                let quick_action = match &kind {
                    NotificationKind::Conversation { quick_action } => quick_action.clone(),
                    _ => None,
                };
                match *action {
                    "default" | "use_here" => {
                        handle_notification_click(&app, &id, kind == NotificationKind::Takeover)
                    }
                    "quick" => {
                        if let Some(emoji) = quick_action {
                            handle_notification_action(
                                &app,
                                &id,
                                &title,
                                NotificationAction::Reply(emoji),
                            );
                        }
                    }
                    "read" => {
                        handle_notification_action(&app, &id, &title, NotificationAction::MarkRead)
                    }
                    _ => {}
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
    kind: NotificationKind,
) {
    windows_native::send_notification(app, id, title, body, kind);
}

#[cfg(windows)]
fn close_os_notification(_app: &tauri::AppHandle, id: &str) {
    windows_native::close_notification(id);
}

#[cfg(windows)]
fn clear_os_notifications(_app: &tauri::AppHandle) {
    windows_native::clear_notifications();
}

// Escapes text for use inside toast XML (element text or an attribute), and
// drops control characters that XML does not allow at all.
#[cfg(any(windows, test))]
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(ch),
            ch if ch.is_control() => {}
            ch => out.push(ch),
        }
    }
    out
}

// Builds the XML for a Windows toast. The `arguments` of each button come
// back when it is pressed; see `windows_native::on_activated`.
#[cfg(any(windows, test))]
fn toast_xml(title: &str, body: &str, kind: &NotificationKind) -> String {
    // A toast with a reply field stays on screen until it is answered or
    // dismissed (the "reminder" scenario). Windows otherwise hides it after a
    // fixed time, even while the reply is being typed.
    let duration = match kind {
        NotificationKind::Conversation { .. } => " scenario=\"reminder\"",
        _ => "",
    };
    let mut xml = format!(
        "<toast launch=\"{TOAST_ARG_OPEN}\"{duration}><visual><binding template=\"ToastGeneric\"><text>{}</text>",
        xml_escape(title)
    );
    if !body.is_empty() {
        xml.push_str(&format!("<text>{}</text>", xml_escape(body)));
    }
    xml.push_str("</binding></visual>");
    let button = |label: &str, argument: &str| {
        format!(
            "<action content=\"{}\" arguments=\"{argument}\" activationType=\"foreground\"/>",
            xml_escape(label)
        )
    };
    match kind {
        NotificationKind::Takeover => {
            xml.push_str("<actions>");
            xml.push_str(&button("Switch Here", TOAST_ARG_SWITCH));
            xml.push_str(&button("No", TOAST_ARG_NO));
            xml.push_str("</actions>");
        }
        NotificationKind::Conversation { quick_action } => {
            // A text field with its own Send button, then the extra buttons.
            xml.push_str(&format!(
                "<actions><input id=\"{TOAST_REPLY_INPUT}\" type=\"text\" placeHolderContent=\"Reply…\"/>\
                 <action content=\"Send\" arguments=\"{TOAST_ARG_REPLY}\" activationType=\"foreground\" hint-inputId=\"{TOAST_REPLY_INPUT}\"/>"
            ));
            if let Some(emoji) = quick_action {
                xml.push_str(&button(emoji, TOAST_ARG_QUICK));
            }
            xml.push_str(&button("Mark as read", TOAST_ARG_READ));
            xml.push_str("</actions>");
        }
        NotificationKind::Plain => {}
    }
    xml.push_str("</toast>");
    xml
}

// A short, stable tag for a toast, derived from our notification id (FNV-1a).
// Windows limits tag length, and ids can be long and contain any character.
#[cfg(any(windows, test))]
fn toast_tag(id: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
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

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(load_base_tray_icon()?)
        .tooltip(APP_NAME)
        .show_menu_on_left_click(false);

    // Template rendering is a macOS-only concept (light/dark menu-bar tinting).
    #[cfg(target_os = "macos")]
    let tray = tray.icon_as_template(true);
    // The menu is kept aside and shown on right-click (see TRAY_MENU).
    #[cfg(not(target_os = "linux"))]
    let _ = TRAY_MENU.set(menu);
    #[cfg(target_os = "linux")]
    let tray = tray.menu(&menu);

    tray.on_menu_event(|app, event| match event.id().as_ref() {
        "show" => show_main_window(app),
        "settings" => show_settings_window(app),
        "quit" => quit_app(app),
        _ => {}
    })
    .on_tray_icon_event(|tray, event| {
        // Right-click opens the popover too, so it is the one tray menu; the
        // old menu is only for when the popover is turned off.
        #[cfg(not(target_os = "linux"))]
        if let TrayIconEvent::Click {
            button: MouseButton::Right,
            button_state,
            rect,
            position,
            ..
        } = event
        {
            if button_state == TRAY_MENU_BUTTON_STATE {
                if TRAY_POPOVER.load(Ordering::Relaxed) {
                    toggle_popover(tray.app_handle(), rect, position);
                } else {
                    show_tray_menu(tray);
                }
            }
            return;
        }
        // Left-click shows the recent-messages popover, or toggles the main
        // window when the popover is turned off. (Linux trays don't report
        // clicks, only the menu.)
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            rect,
            position,
            ..
        } = event
        {
            let app = tray.app_handle();
            if TRAY_POPOVER.load(Ordering::Relaxed) {
                toggle_popover(app, rect, position);
                return;
            }
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

// Opens the tray menu at the icon (see TRAY_MENU). The menu is attached only
// while it is open: showing it blocks until it closes.
#[cfg(not(target_os = "linux"))]
fn show_tray_menu(tray: &tauri::tray::TrayIcon) {
    let Some(menu) = TRAY_MENU.get().cloned() else {
        return;
    };
    let tray = tray.clone();
    // Off the event handler, which runs on the main thread these calls hop to.
    std::thread::spawn(move || {
        if tray.set_menu(Some(menu)).is_ok() {
            let _ = tray.with_inner_tray_icon(|inner| inner.show_menu());
        }
        let _ = tray.set_menu(None::<Menu<tauri::Wry>>);
    });
}

// =============================================================================
// Tray popover (recent messages with quick reply)
// =============================================================================

// Where to put a `w`×`h` popover for a tray icon at `icon`, inside the
// monitor's work area `work` (all physical pixels): centred on the icon and
// below it when the icon is at the top of the screen (macOS menu bar), above
// it when at the bottom, beside it when the taskbar is on the left or right.
fn popover_position(icon: PxRect, work: PxRect, w: f64, h: f64, gap: f64) -> (f64, f64) {
    fn clamp(value: f64, low: f64, high: f64) -> f64 {
        if high < low {
            low
        } else {
            value.clamp(low, high)
        }
    }
    let (min_x, max_x) = (work.x + gap, work.x + work.w - w - gap);
    let (min_y, max_y) = (work.y + gap, work.y + work.h - h - gap);
    let icon_center_x = icon.x + icon.w / 2.0;
    let icon_center_y = icon.y + icon.h / 2.0;
    if icon.x + icon.w <= work.x {
        return (min_x, clamp(icon_center_y - h / 2.0, min_y, max_y));
    }
    if icon.x >= work.x + work.w {
        return (max_x, clamp(icon_center_y - h / 2.0, min_y, max_y));
    }
    let x = clamp(icon_center_x - w / 2.0, min_x, max_x);
    let y = if icon_center_y < work.y + work.h / 2.0 {
        icon.y + icon.h + gap
    } else {
        icon.y - h - gap
    };
    (x, clamp(y, min_y, max_y))
}

// The popover's size and position for its tray icon, `height` logical pixels
// tall.
fn popover_frame(anchor: PopoverAnchor, height: f64) -> (PhysicalSize<u32>, PhysicalPosition<i32>) {
    let scale = anchor.scale;
    let (w, h) = (POPOVER_SIZE.0 * scale, height * scale);
    let (x, y) = popover_position(anchor.icon, anchor.work, w, h, POPOVER_GAP * scale);
    (
        PhysicalSize::new(w.round() as u32, h.round() as u32),
        PhysicalPosition::new(x.round() as i32, y.round() as i32),
    )
}

// The popover height to use (logical pixels): the last one its page asked
// for, or the maximum until it has.
fn popover_height() -> f64 {
    POPOVER_HEIGHT.lock().map(|h| *h).unwrap_or(POPOVER_SIZE.1)
}

// A popover height the page asked for, kept between the minimum and the
// maximum; None if it isn't a number.
fn clamp_popover_height(height: f64) -> Option<f64> {
    height
        .is_finite()
        .then(|| height.clamp(POPOVER_MIN_HEIGHT, POPOVER_SIZE.1))
}

// Hides the popover and remembers when, for POPOVER_REOPEN_GUARD.
fn hide_popover_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window(POPOVER_WINDOW) {
        let _ = window.hide();
    }
    if let Ok(mut hidden_at) = POPOVER_HIDDEN_AT.lock() {
        *hidden_at = Some(Instant::now());
    }
}

// Runs a script in the popover page, if the popover exists.
fn eval_in_popover(app: &tauri::AppHandle, script: &str) {
    if let Some(window) = app.get_webview_window(POPOVER_WINDOW) {
        let _ = window.eval(script);
    }
}

// Opens the popover next to the tray icon, or closes it if it is open.
fn toggle_popover(app: &tauri::AppHandle, rect: tauri::Rect, click: PhysicalPosition<f64>) {
    if let Some(window) = app.get_webview_window(POPOVER_WINDOW) {
        if window.is_visible().unwrap_or(false) {
            hide_popover_window(app);
            return;
        }
    }
    let just_hidden = POPOVER_HIDDEN_AT
        .lock()
        .ok()
        .and_then(|hidden_at| *hidden_at)
        .is_some_and(|at| at.elapsed() < POPOVER_REOPEN_GUARD);
    if just_hidden {
        return;
    }

    let monitor = app
        .monitor_from_point(click.x, click.y)
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten());
    let Some(monitor) = monitor else {
        return;
    };
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let work = PxRect {
        x: area.position.x as f64,
        y: area.position.y as f64,
        w: area.size.width as f64,
        h: area.size.height as f64,
    };
    let icon_pos = rect.position.to_physical::<f64>(scale);
    let icon_size = rect.size.to_physical::<f64>(scale);
    let icon = if icon_size.width > 0.0 {
        PxRect {
            x: icon_pos.x,
            y: icon_pos.y,
            w: icon_size.width,
            h: icon_size.height,
        }
    } else {
        PxRect {
            x: click.x,
            y: click.y,
            w: 0.0,
            h: 0.0,
        }
    };
    let anchor = PopoverAnchor { icon, work, scale };
    if let Ok(mut current) = POPOVER_ANCHOR.lock() {
        *current = Some(anchor);
    }
    let (size, position) = popover_frame(anchor, popover_height());

    let place_and_show = move |window: &tauri::WebviewWindow| {
        let _ = window.set_size(size);
        let _ = window.set_position(position);
        let _ = window.show();
        let _ = window.set_focus();
    };
    if let Some(window) = app.get_webview_window(POPOVER_WINDOW) {
        place_and_show(&window);
        let _ = window.eval("window.__gmPopoverShown && window.__gmPopoverShown();");
        return;
    }
    // Built off the main thread, as for the settings window (WebView2).
    let app = app.clone();
    std::thread::spawn(move || {
        let theme = parse_theme(load_settings(&app).theme.as_deref());
        let builder = themed_window_builder(
            &app,
            POPOVER_WINDOW,
            WebviewUrl::App("popover.html".into()),
            theme,
        )
        .title("Recent messages")
        .inner_size(POPOVER_SIZE.0, popover_height())
        .decorations(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .visible(false)
        // The page picks its macOS or Windows look from this.
        .initialization_script(format!(
            "window.__GM_POPOVER_OS = {};",
            serde_json::to_string(std::env::consts::OS).unwrap_or_default()
        ));
        // On macOS the window is see-through with the system popover material
        // behind the page, like a menu bar extra, rounded to match its CSS.
        #[cfg(target_os = "macos")]
        let builder = builder
            .transparent(true)
            .background_color(tauri::window::Color(0, 0, 0, 0))
            .effects(
                tauri::window::EffectsBuilder::new()
                    .effect(tauri::window::Effect::Popover)
                    .state(tauri::window::EffectState::Active)
                    .radius(POPOVER_CORNER_RADIUS)
                    .build(),
            );
        let built = builder.build();
        match built {
            Ok(window) => place_and_show(&window),
            Err(e) => eprintln!("[toggle_popover] failed to build popover window: {e}"),
        }
    });
}

// Keeps at most `max` characters of `text`.
fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
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

// Auto-allow microphone and camera permission requests in the WebView2 webview
// so calls work without prompting the user every session. (Notifications are
// native toasts sent from Rust, so the page needs no notification permission.) Mirrors the pattern wry uses internally for clipboard.
#[cfg(windows)]
fn grant_media_permissions(window: &tauri::WebviewWindow) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PERMISSION_KIND, COREWEBVIEW2_PERMISSION_KIND_CAMERA,
        COREWEBVIEW2_PERMISSION_KIND_MICROPHONE, COREWEBVIEW2_PERMISSION_STATE_ALLOW,
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

// Called by the page whenever its list of recent incoming conversations
// changes; the popover is updated if it exists.
// Keeps a conversation photo only if it is an image data URL or an https URL
// of a sensible size; a cut-off one would be useless.
fn checked_photo(photo: String) -> String {
    let allowed = photo.starts_with("data:image/") || photo.starts_with("https://");
    if allowed && photo.len() <= MAX_PHOTO_CHARS {
        photo
    } else {
        String::new()
    }
}

#[tauri::command]
fn set_recent_conversations(app: tauri::AppHandle, conversations: Vec<RecentConversation>) {
    let conversations: Vec<RecentConversation> = conversations
        .into_iter()
        .take(MAX_RECENT_CONVERSATIONS)
        .map(|c| RecentConversation {
            title: truncate_chars(&c.title, 200),
            snippet: truncate_chars(&c.snippet, 500),
            time: truncate_chars(&c.time, 40),
            photo: checked_photo(c.photo),
            ..c
        })
        .collect();
    ipc_log!("set_recent_conversations: {}", conversations.len());
    if let Ok(json) = serde_json::to_string(&conversations) {
        eval_in_popover(
            &app,
            &format!("window.__gmPopoverUpdate && window.__gmPopoverUpdate({json});"),
        );
    }
    if let Ok(mut recent) = RECENT_CONVERSATIONS.lock() {
        *recent = conversations;
    }
}

// Called by the page when its connection state changes (one of
// CONNECTION_STATES, or empty on other pages); the popover is updated if it
// exists.
#[tauri::command]
fn set_connection_state(app: tauri::AppHandle, state: String) {
    let state = if CONNECTION_STATES.contains(&state.as_str()) {
        state
    } else {
        String::new()
    };
    ipc_log!("set_connection_state: {state}");
    if let Ok(json) = serde_json::to_string(&state) {
        eval_in_popover(
            &app,
            &format!("window.__gmPopoverConnection && window.__gmPopoverConnection({json});"),
        );
    }
    if let Ok(mut current) = CONNECTION_STATE.lock() {
        *current = state;
    }
}

#[tauri::command]
fn get_popover_state(app: tauri::AppHandle) -> PopoverState {
    let settings = load_settings(&app);
    PopoverState {
        conversations: RECENT_CONVERSATIONS
            .lock()
            .map(|recent| recent.clone())
            .unwrap_or_default(),
        connection: CONNECTION_STATE
            .lock()
            .map(|state| state.clone())
            .unwrap_or_default(),
        quick_action: settings.quick_action,
        theme: settings.theme,
    }
}

// Something chosen in the popover: "reply" (with text), "read" or "open" a
// conversation, or "use_here" to move the session from another computer to
// this app, which the page carries out and reports on through
// popover_action_done; or "show" the main window, open "settings", or
// "quit". Returns false for an unknown action.
#[tauri::command]
fn popover_action(app: tauri::AppHandle, title: String, action: String, text: String) -> bool {
    ipc_log!("popover_action: {action}");
    match action.as_str() {
        "reply" | "read" | "use_here" => {}
        "open" | "show" => {
            hide_popover_window(&app);
            show_main_window(&app);
            if action == "show" {
                return true;
            }
        }
        "settings" => {
            hide_popover_window(&app);
            show_settings_window(&app);
            return true;
        }
        "quit" => {
            quit_app(&app);
            return true;
        }
        _ => return false,
    }
    if let (Ok(title), Ok(action), Ok(text)) = (
        serde_json::to_string(&title),
        serde_json::to_string(&action),
        serde_json::to_string(&text),
    ) {
        eval_in_main(
            &app,
            &format!(
                "window.__gmPopoverAction && window.__gmPopoverAction({title}, {action}, {text});"
            ),
        );
    }
    true
}

// Called by the page when a popover action finishes; passes the outcome on.
#[tauri::command]
fn popover_action_done(app: tauri::AppHandle, title: String, action: String, ok: bool) {
    ipc_log!("popover_action_done: {action} ok={ok}");
    if let (Ok(title), Ok(action)) = (
        serde_json::to_string(&title),
        serde_json::to_string(&action),
    ) {
        eval_in_popover(
            &app,
            &format!("window.__gmPopoverActionDone && window.__gmPopoverActionDone({title}, {action}, {ok});"),
        );
    }
}

// Called by the popover page with the height of its content (logical
// pixels); the window is resized to fit, keeping it anchored to the tray icon
// (on Windows, above the taskbar, its top edge moves).
#[tauri::command]
fn set_popover_height(app: tauri::AppHandle, height: f64) {
    let Some(height) = clamp_popover_height(height) else {
        return;
    };
    if let Ok(mut current) = POPOVER_HEIGHT.lock() {
        if (*current - height).abs() < 0.5 {
            return;
        }
        *current = height;
    }
    let anchor = POPOVER_ANCHOR.lock().ok().and_then(|anchor| *anchor);
    if let (Some(window), Some(anchor)) = (app.get_webview_window(POPOVER_WINDOW), anchor) {
        let (size, position) = popover_frame(anchor, height);
        let _ = window.set_size(size);
        let _ = window.set_position(position);
    }
}

#[tauri::command]
fn hide_popover(app: tauri::AppHandle) {
    hide_popover_window(&app);
}

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

    settings.quick_action = settings.quick_action.trim().to_string();
    if settings.quick_action.len() > MAX_QUICK_ACTION_BYTES {
        settings.quick_action = DEFAULT_QUICK_ACTION.to_string();
    }

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

// Presses the Return key in the main window's page as a real key press, so
// the page sends the message a notification reply has typed into its message
// box. The page ignores key presses and clicks simulated from script. Returns
// false where this is not implemented, and the page falls back to simulating.
#[tauri::command]
fn press_enter_key(app: tauri::AppHandle) -> bool {
    ipc_log!("press_enter_key invoked");
    #[cfg(target_os = "macos")]
    {
        main_window(&app).is_some_and(|window| macos_native::press_return(&window))
    }
    #[cfg(windows)]
    {
        main_window(&app).is_some_and(|window| windows_native::press_return(&window))
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = app;
        false
    }
}

// Lets the page script write a line to the debug log (debug builds only).
#[tauri::command]
fn log_page_diagnostic(message: String) {
    ipc_log!("page: {message}");
    let _ = message;
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
                    NotificationKind::Takeover,
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
    can_reply: Option<bool>,
) {
    ipc_log!(
        "send_native_notification(id={id}, kind={kind}, can_reply={can_reply:?}, title={title})"
    );
    if !SHOW_NOTIFICATIONS.load(Ordering::Relaxed) {
        return;
    }
    let kind = if kind == "takeover" {
        NotificationKind::Takeover
    } else if can_reply == Some(true) {
        let quick_action = Some(load_settings(&app).quick_action).filter(|emoji| !emoji.is_empty());
        NotificationKind::Conversation { quick_action }
    } else {
        NotificationKind::Plain
    };
    send_os_notification(&app, id, title, body, kind);
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
            TRAY_POPOVER.store(settings.tray_popover, Ordering::Relaxed);
            #[cfg(target_os = "macos")]
            macos_native::init_notifications(&handle);
            #[cfg(windows)]
            windows_native::init_notifications(&handle);
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
            // The popover closes when it loses focus, like a menu.
            if window.label() == POPOVER_WINDOW {
                if let tauri::WindowEvent::Focused(false) = event {
                    hide_popover_window(window.app_handle());
                }
                return;
            }
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
            prompt_takeover_session,
            press_enter_key,
            log_page_diagnostic,
            set_recent_conversations,
            set_connection_state,
            get_popover_state,
            popover_action,
            popover_action_done,
            hide_popover,
            set_popover_height
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
        class, define_class, msg_send,
        rc::Retained,
        runtime::{AnyObject, ProtocolObject},
        DefinedClass, MainThreadMarker, MainThreadOnly,
    };
    use objc2_foundation::{
        ns_string, NSActivityOptions, NSArray, NSObject, NSObjectProtocol, NSPoint, NSProcessInfo,
        NSString, NSUserNotification, NSUserNotificationAction, NSUserNotificationActivationType,
        NSUserNotificationCenter, NSUserNotificationCenterDelegate,
        NSUserNotificationDefaultSoundName,
    };
    use std::cell::RefCell;

    use crate::{NotificationAction, NotificationKind};

    // Identifiers of the extra buttons on a conversation notification. The
    // quick action carries its emoji after the prefix, so handling a click
    // needs no lookup of what was configured when the notification was sent.
    const QUICK_ACTION_PREFIX: &str = "gm-quick:";
    const MARK_READ_ACTION_ID: &str = "gm-read";

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
                ipc_log!("notification activated: id={id} type={}", activation.0);
                center.removeDeliveredNotification(notification);
                DELIVERED_NOTIFS.with(|slot| slot.borrow_mut().remove(&id));
                let app = &self.ivars().app;

                if activation == NSUserNotificationActivationType::Replied
                    || activation == NSUserNotificationActivationType::AdditionalActionClicked
                {
                    let title = notification
                        .title()
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    if let Some(action) = Self::chosen_action(notification, activation) {
                        crate::handle_notification_action(app, &id, &title, action);
                    }
                    return;
                }

                let clicked = activation == NSUserNotificationActivationType::ContentsClicked
                    || activation == NSUserNotificationActivationType::ActionButtonClicked;
                if !clicked {
                    return;
                }
                crate::handle_notification_click(app, &id, crate::is_takeover_notification(&id));
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
        // What the user chose on a conversation notification: text typed into
        // the reply field, or one of the extra buttons.
        #[allow(deprecated)]
        fn chosen_action(
            notification: &NSUserNotification,
            activation: NSUserNotificationActivationType,
        ) -> Option<NotificationAction> {
            if activation == NSUserNotificationActivationType::Replied {
                let text = notification.response()?.string().to_string();
                return Some(NotificationAction::Reply(text));
            }
            let action_id = notification
                .additionalActivationAction()?
                .identifier()?
                .to_string();
            if let Some(emoji) = action_id.strip_prefix(QUICK_ACTION_PREFIX) {
                Some(NotificationAction::Reply(emoji.to_string()))
            } else if action_id == MARK_READ_ACTION_ID {
                Some(NotificationAction::MarkRead)
            } else {
                None
            }
        }

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
        kind: NotificationKind,
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
                match &kind {
                    NotificationKind::Takeover => {
                        notif.setHasActionButton(true);
                        notif.setActionButtonTitle(ns_string!("Switch Here"));
                        notif.setOtherButtonTitle(ns_string!("No"));
                    }
                    NotificationKind::Conversation { quick_action } => {
                        // A text field to reply in place, plus extra buttons.
                        notif.setHasReplyButton(true);
                        notif.setResponsePlaceholder(Some(ns_string!("Reply…")));
                        let mut actions = Vec::new();
                        if let Some(emoji) = quick_action {
                            actions.push(notification_action(
                                &format!("{QUICK_ACTION_PREFIX}{emoji}"),
                                &format!("Send {emoji}"),
                            ));
                        }
                        actions.push(notification_action(MARK_READ_ACTION_ID, "Mark as Read"));
                        notif.setAdditionalActions(Some(&NSArray::from_retained_slice(&actions)));
                    }
                    NotificationKind::Plain => {}
                }
                center.deliverNotification(&notif);
                DELIVERED_NOTIFS.with(|slot| {
                    slot.borrow_mut().insert(id, notif);
                });
            }
        });
    }

    #[allow(deprecated)]
    fn notification_action(id: &str, title: &str) -> Retained<NSUserNotificationAction> {
        NSUserNotificationAction::actionWithIdentifier_title(
            Some(&NSString::from_str(id)),
            Some(&NSString::from_str(title)),
        )
    }

    const KEY_DOWN: usize = 10; // NSEventTypeKeyDown
    const KEY_UP: usize = 11; // NSEventTypeKeyUp
    const RETURN_KEY_CODE: u16 = 36;

    // Builds a Return key event (KEY_DOWN or KEY_UP) for the given window.
    pub(crate) fn return_key_event(
        kind: usize,
        window_number: isize,
    ) -> Option<Retained<AnyObject>> {
        let characters = ns_string!("\r");
        unsafe {
            msg_send![
                class!(NSEvent),
                keyEventWithType: kind,
                location: NSPoint::new(0.0, 0.0),
                modifierFlags: 0usize,
                timestamp: NSProcessInfo::processInfo().systemUptime(),
                windowNumber: window_number,
                context: std::ptr::null::<AnyObject>(),
                characters: characters,
                charactersIgnoringModifiers: characters,
                isARepeat: false,
                keyCode: RETURN_KEY_CODE,
            ]
        }
    }

    // Sends a Return key press (down, then up) to the window's web view, as
    // if typed on the keyboard. Returns whether the press was queued.
    pub fn press_return(window: &tauri::WebviewWindow) -> bool {
        window
            .with_webview(|webview| unsafe {
                let view = webview.inner() as *mut AnyObject;
                let ns_window = webview.ns_window() as *mut AnyObject;
                if view.is_null() || ns_window.is_null() {
                    return;
                }
                let _: bool = msg_send![ns_window, makeFirstResponder: view];
                let window_number: isize = msg_send![ns_window, windowNumber];
                if let Some(event) = return_key_event(KEY_DOWN, window_number) {
                    let _: () = msg_send![view, keyDown: &*event];
                }
                if let Some(event) = return_key_event(KEY_UP, window_number) {
                    let _: () = msg_send![view, keyUp: &*event];
                }
            })
            .is_ok()
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
// Windows native toasts
// =============================================================================

#[cfg(windows)]
mod windows_native {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    use tauri::Manager;
    use windows::core::{IInspectable, Interface, HSTRING};
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::Foundation::{IPropertyValue, TypedEventHandler};
    use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
    use windows::UI::Notifications::{
        ToastActivatedEventArgs, ToastNotification, ToastNotificationManager,
    };

    use crate::{NotificationAction, NotificationKind};

    // Toast groups. Clearing message notifications removes one group and
    // leaves the takeover toast alone.
    const GROUP_MESSAGES: &str = "messages";
    const GROUP_TAKEOVER: &str = "takeover";

    // Shown next to the app name on toasts.
    const TOAST_ICON: &[u8] = include_bytes!("../icons/128x128.png");

    // The AppUserModelID toasts are sent under; set once at startup.
    static APP_ID: OnceLock<HSTRING> = OnceLock::new();
    // Toasts we have shown, by our notification id. A toast only reports
    // clicks while its object is alive, so they are kept until closed.
    static TOASTS: OnceLock<Mutex<HashMap<String, ToastNotification>>> = OnceLock::new();

    fn toasts() -> &'static Mutex<HashMap<String, ToastNotification>> {
        TOASTS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn group_for(id: &str) -> &'static str {
        if crate::is_takeover_notification(id) {
            GROUP_TAKEOVER
        } else {
            GROUP_MESSAGES
        }
    }

    fn set_registry_string(key: &str, name: &str, value: &str) {
        let data: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                &HSTRING::from(key),
                &HSTRING::from(name),
                REG_SZ.0,
                Some(data.as_ptr().cast()),
                (data.len() * std::mem::size_of::<u16>()) as u32,
            );
        }
    }

    // Registers the app's identity for toasts. An app that is not installed
    // from a package needs its AppUserModelID described in the registry for
    // toasts to carry its name and icon; doing it at startup covers dev runs
    // as well as installed builds (whose Start Menu shortcut uses the same id).
    pub fn init_notifications(app: &tauri::AppHandle) {
        let app_id = app.config().identifier.clone();
        let key = format!("Software\\Classes\\AppUserModelId\\{app_id}");
        set_registry_string(&key, "DisplayName", crate::APP_NAME);
        if let Ok(dir) = app.path().app_local_data_dir() {
            let icon = dir.join("toast-icon.png");
            let written =
                std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&icon, TOAST_ICON));
            if let (Ok(()), Some(path)) = (written, icon.to_str()) {
                set_registry_string(&key, "IconUri", path);
            }
        }
        let _ = APP_ID.set(HSTRING::from(app_id));
    }

    pub fn send_notification(
        app: &tauri::AppHandle,
        id: String,
        title: String,
        body: String,
        kind: NotificationKind,
    ) {
        if let Err(_err) = show_toast(app, id, title, &body, kind) {
            ipc_log!("toast failed: {_err}");
        }
    }

    fn show_toast(
        app: &tauri::AppHandle,
        id: String,
        title: String,
        body: &str,
        kind: NotificationKind,
    ) -> windows::core::Result<()> {
        let Some(app_id) = APP_ID.get() else {
            return Ok(());
        };
        let document = XmlDocument::new()?;
        document.LoadXml(&HSTRING::from(crate::toast_xml(&title, body, &kind)))?;
        let toast = ToastNotification::CreateToastNotification(&document)?;
        // Showing a toast with the tag and group of one still on screen
        // replaces it, so a conversation never has two.
        toast.SetTag(&HSTRING::from(crate::toast_tag(&id)))?;
        toast.SetGroup(&HSTRING::from(group_for(&id)))?;

        let handler_app = app.clone();
        let handler_id = id.clone();
        toast.Activated(&TypedEventHandler::<ToastNotification, IInspectable>::new(
            move |_, args| {
                let activated = args
                    .as_ref()
                    .and_then(|args| args.cast::<ToastActivatedEventArgs>().ok());
                let argument = activated
                    .as_ref()
                    .and_then(|args| args.Arguments().ok())
                    .map(|argument| argument.to_string())
                    .unwrap_or_default();
                // Text typed into the reply field (Windows 10 1903 and later).
                let reply = activated
                    .as_ref()
                    .and_then(|args| args.UserInput().ok())
                    .and_then(|input| input.Lookup(&HSTRING::from(crate::TOAST_REPLY_INPUT)).ok())
                    .and_then(|value| value.cast::<IPropertyValue>().ok())
                    .and_then(|value| value.GetString().ok())
                    .map(|text| text.to_string())
                    .unwrap_or_default();
                on_activated(&handler_app, &handler_id, &title, &kind, &argument, reply);
                Ok(())
            },
        ))?;

        ToastNotificationManager::CreateToastNotifierWithId(app_id)?.Show(&toast)?;
        if let Ok(mut guard) = toasts().lock() {
            guard.insert(id, toast);
        }
        Ok(())
    }

    // Runs when a toast's body or one of its buttons is clicked. This is
    // called on a Windows thread pool thread, not the UI thread.
    fn on_activated(
        app: &tauri::AppHandle,
        id: &str,
        title: &str,
        kind: &NotificationKind,
        argument: &str,
        reply: String,
    ) {
        close_notification(id);
        let quick_action = match kind {
            NotificationKind::Conversation { quick_action } => quick_action.clone(),
            _ => None,
        };
        let is_conversation = matches!(kind, NotificationKind::Conversation { .. });
        let is_takeover = *kind == NotificationKind::Takeover;
        let action = match argument {
            crate::TOAST_ARG_NO if is_takeover => return,
            crate::TOAST_ARG_REPLY if is_conversation && !reply.trim().is_empty() => {
                Some(NotificationAction::Reply(reply))
            }
            crate::TOAST_ARG_QUICK if is_conversation => {
                quick_action.map(NotificationAction::Reply)
            }
            crate::TOAST_ARG_READ if is_conversation => Some(NotificationAction::MarkRead),
            _ => None,
        };
        match action {
            Some(action) => crate::handle_notification_action(app, id, title, action),
            None => crate::handle_notification_click(app, id, is_takeover),
        }
    }

    pub fn close_notification(id: &str) {
        if let Ok(mut guard) = toasts().lock() {
            guard.remove(id);
        }
        let Some(app_id) = APP_ID.get() else {
            return;
        };
        if let Ok(history) = ToastNotificationManager::History() {
            let _ = history.RemoveGroupedTagWithId(
                &HSTRING::from(crate::toast_tag(id)),
                &HSTRING::from(group_for(id)),
                app_id,
            );
        }
    }

    // Removes every message toast. The takeover toast is left in place: it
    // is cleared only when the session comes back.
    pub fn clear_notifications() {
        if let Ok(mut guard) = toasts().lock() {
            guard.retain(|id, _| crate::is_takeover_notification(id));
        }
        let Some(app_id) = APP_ID.get() else {
            return;
        };
        if let Ok(history) = ToastNotificationManager::History() {
            let _ = history.RemoveGroupWithId(&HSTRING::from(GROUP_MESSAGES), app_id);
        }
    }

    // Removes all of the app's toasts, including the takeover one (on quit).
    pub fn remove_all_notifications() {
        if let Ok(mut guard) = toasts().lock() {
            guard.clear();
        }
        let Some(app_id) = APP_ID.get() else {
            return;
        };
        if let Ok(history) = ToastNotificationManager::History() {
            let _ = history.ClearWithId(app_id);
        }
    }

    // Sends a Return key press (down, then up) to the page through WebView2's
    // DevTools protocol, which the page treats as a real key press. Returns
    // whether the press was queued.
    pub fn press_return(window: &tauri::WebviewWindow) -> bool {
        use webview2_com::CallDevToolsProtocolMethodCompletedHandler;

        window
            .with_webview(|webview| unsafe {
                let Ok(core) = webview.controller().CoreWebView2() else {
                    return;
                };
                let method = HSTRING::from("Input.dispatchKeyEvent");
                for params in [
                    r#"{"type":"keyDown","key":"Enter","code":"Enter","windowsVirtualKeyCode":13,"nativeVirtualKeyCode":13,"text":"\r","unmodifiedText":"\r"}"#,
                    r#"{"type":"keyUp","key":"Enter","code":"Enter","windowsVirtualKeyCode":13,"nativeVirtualKeyCode":13}"#,
                ] {
                    let _ = core.CallDevToolsProtocolMethod(
                        &method,
                        &HSTRING::from(params),
                        &CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(()))),
                    );
                }
            })
            .is_ok()
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

    // Building the event checks the Objective-C call's argument types, which
    // are only verified at run time.
    #[cfg(target_os = "macos")]
    #[test]
    fn return_key_event_is_a_return_key_press() {
        use objc2::msg_send;
        let event = macos_native::return_key_event(10, 0).expect("event is created");
        let key_code: u16 = unsafe { msg_send![&*event, keyCode] };
        assert_eq!(key_code, 36);
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
        assert_eq!(settings.quick_action, DEFAULT_QUICK_ACTION);
        assert!(settings.tray_popover);
    }

    const WORK: PxRect = PxRect {
        x: 0.0,
        y: 50.0,
        w: 2000.0,
        h: 1000.0,
    };

    #[test]
    fn popover_opens_below_a_menu_bar_icon() {
        let icon = PxRect {
            x: 1500.0,
            y: 0.0,
            w: 40.0,
            h: 44.0,
        };
        assert_eq!(
            popover_position(icon, WORK, 360.0, 460.0, 6.0),
            (1340.0, 56.0)
        );
    }

    #[test]
    fn popover_opens_above_a_bottom_taskbar_icon_and_stays_on_screen() {
        let work = PxRect {
            x: 0.0,
            y: 0.0,
            w: 2000.0,
            h: 1000.0,
        };
        let icon = PxRect {
            x: 1980.0,
            y: 1010.0,
            w: 40.0,
            h: 40.0,
        };
        assert_eq!(
            popover_position(icon, work, 360.0, 460.0, 6.0),
            (1634.0, 534.0)
        );
    }

    #[test]
    fn popover_opens_beside_a_side_taskbar_icon() {
        let work = PxRect {
            x: 100.0,
            y: 0.0,
            w: 1900.0,
            h: 1000.0,
        };
        let icon = PxRect {
            x: 30.0,
            y: 900.0,
            w: 40.0,
            h: 40.0,
        };
        assert_eq!(
            popover_position(icon, work, 360.0, 460.0, 6.0),
            (106.0, 534.0)
        );
        let work = PxRect {
            x: 0.0,
            y: 0.0,
            w: 1900.0,
            h: 1000.0,
        };
        let icon = PxRect {
            x: 1930.0,
            y: 20.0,
            w: 40.0,
            h: 40.0,
        };
        assert_eq!(
            popover_position(icon, work, 360.0, 460.0, 6.0),
            (1534.0, 6.0)
        );
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

    #[test]
    fn toast_text_is_escaped_for_xml() {
        assert_eq!(
            xml_escape("<b> & \"it's\" \u{1}ok"),
            "&lt;b&gt; &amp; &quot;it&apos;s&quot; ok"
        );
        let xml = toast_xml("A & B", "1 < 2", &NotificationKind::Plain);
        assert!(xml.contains("<text>A &amp; B</text><text>1 &lt; 2</text>"));
        assert!(!xml.contains("<actions>"));
        assert!(!xml.contains("scenario="));
    }

    #[test]
    fn conversation_toast_has_reply_field_and_buttons() {
        let kind = NotificationKind::Conversation {
            quick_action: Some("👍".to_string()),
        };
        let xml = toast_xml("Bob", "hi", &kind);
        assert!(xml.starts_with("<toast launch=\"open\" scenario=\"reminder\">"));
        assert!(xml.contains("<input id=\"reply\" type=\"text\""));
        assert!(xml
            .contains("arguments=\"reply\" activationType=\"foreground\" hint-inputId=\"reply\""));
        assert!(xml.contains("content=\"👍\" arguments=\"quick\""));
        assert!(xml.contains("content=\"Mark as read\" arguments=\"read\""));

        let without_emoji = toast_xml(
            "Bob",
            "hi",
            &NotificationKind::Conversation { quick_action: None },
        );
        assert!(!without_emoji.contains("arguments=\"quick\""));
    }

    #[test]
    fn takeover_toast_offers_switch_and_no() {
        let xml = toast_xml("Open elsewhere", "", &NotificationKind::Takeover);
        assert!(xml.contains("content=\"Switch Here\" arguments=\"switch\""));
        assert!(xml.contains("content=\"No\" arguments=\"no\""));
        assert!(!xml.contains("<input"));
    }

    #[test]
    fn toast_tags_are_short_stable_and_distinct() {
        let tag = toast_tag("gm-conv-Some very long conversation title with emoji 🎉");
        assert_eq!(tag.len(), 16);
        assert_eq!(
            tag,
            toast_tag("gm-conv-Some very long conversation title with emoji 🎉")
        );
        assert_ne!(tag, toast_tag("gm-conv-Bob"));
    }

    #[test]
    fn popover_heights_are_kept_between_the_minimum_and_maximum() {
        assert_eq!(clamp_popover_height(300.0), Some(300.0));
        assert_eq!(clamp_popover_height(20.0), Some(POPOVER_MIN_HEIGHT));
        assert_eq!(clamp_popover_height(5000.0), Some(POPOVER_SIZE.1));
        assert_eq!(clamp_popover_height(f64::NAN), None);
    }

    #[test]
    fn a_shorter_popover_above_the_taskbar_keeps_its_bottom_edge() {
        let anchor = PopoverAnchor {
            icon: PxRect {
                x: 1800.0,
                y: 1040.0,
                w: 24.0,
                h: 24.0,
            },
            work: PxRect {
                x: 0.0,
                y: 0.0,
                w: 1920.0,
                h: 1032.0,
            },
            scale: 1.0,
        };
        let (tall_size, tall_pos) = popover_frame(anchor, 460.0);
        let (short_size, short_pos) = popover_frame(anchor, 300.0);
        assert_eq!(short_size.height, 300);
        assert_eq!(
            tall_pos.y + tall_size.height as i32,
            short_pos.y + short_size.height as i32
        );
    }

    #[test]
    fn conversation_photos_are_limited_to_images_of_a_sensible_size() {
        let data = "data:image/png;base64,iVBORw0KGgo=".to_string();
        assert_eq!(checked_photo(data.clone()), data);
        let https = "https://lh3.googleusercontent.com/a/photo=s64".to_string();
        assert_eq!(checked_photo(https.clone()), https);
        assert_eq!(checked_photo("javascript:alert(1)".to_string()), "");
        assert_eq!(checked_photo("http://example.com/a.png".to_string()), "");
        assert_eq!(
            checked_photo("blob:https://messages.google.com/x".to_string()),
            ""
        );
        let huge = format!("data:image/png;base64,{}", "A".repeat(MAX_PHOTO_CHARS));
        assert_eq!(checked_photo(huge), "");
    }

    #[test]
    fn takeover_notification_ids_are_recognized() {
        assert!(is_takeover_notification(TAKEOVER_NOTIFICATION_ID));
        assert!(!is_takeover_notification("gm-conv-Alice"));
    }
}
