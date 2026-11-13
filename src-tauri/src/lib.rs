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

// Tracks the latest unread count so the Dock/taskbar badge can be re-applied
// whenever the main window transitions back from tray-only (Accessory) mode.
static LAST_UNREAD_COUNT: AtomicU32 = AtomicU32::new(0);
// Tracks the last unread count applied to the tray so repeated title mutations
// with the same count don't re-apply icons/tooltips.
static LAST_TRAY_COUNT: AtomicU32 = AtomicU32::new(u32::MAX);

// Cached RGBA buffers for the base and badged tray icons to avoid repeated PNG
// decoding and compositing on every unread count change.
static BASE_TRAY_ICON: OnceLock<(Vec<u8>, u32, u32)> = OnceLock::new();
static BADGED_TRAY_ICON: OnceLock<(Vec<u8>, u32, u32)> = OnceLock::new();

// Tracks the main window's zoom percentage (100 = 1.0x, clamped to 50..=250).
static ZOOM_PERCENT: AtomicU32 = AtomicU32::new(100);

// Live runtime flags mirrored from Settings and page state:
static SHOW_NOTIFICATIONS: AtomicBool = AtomicBool::new(true);
static BACKGROUND_KEEP_ALIVE: AtomicBool = AtomicBool::new(true);
// True when "Google Messages is open on another computer" is active and the
// user hasn't switched the session back here; while true, background keep-alive
// and auto-refresh are paused so the OS can sleep the app normally.
static SESSION_YIELDED: AtomicBool = AtomicBool::new(false);
// Ensures we only pop one notification per transition into the yielded state.
static TAKEOVER_NOTIFIED: AtomicBool = AtomicBool::new(false);
// Watchdog counters for the background heartbeat loop.
static PENDING_HEARTBEAT_PINGS: AtomicU32 = AtomicU32::new(0);
static UNHEALTHY_STREAK: AtomicU32 = AtomicU32::new(0);
static LAST_AUTO_RELOAD_SECS: AtomicU64 = AtomicU64::new(0);

// Default start URL if the user has not customized it. The canonical web
// entry point: when already paired this lands straight on the (cached)
// conversation list; when not paired it redirects to the QR pairing page on
// its own. Loading the pairing sub-route directly added an extra hop.
const DEFAULT_START_URL: &str = "https://messages.google.com/web/";

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

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
struct WindowSize {
    width: f64,
    height: f64,
    x: Option<f64>,
    y: Option<f64>,
    maximized: bool,
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

// Tauri commands exposed to the settings page.
#[tauri::command]
fn get_settings(app: tauri::AppHandle) -> Settings {
    load_settings(&app)
}

#[tauri::command]
fn set_settings(app: tauri::AppHandle, mut settings: Settings) {
    // The settings page doesn't manage window geometry, so its payload always
    // carries the field's default — keep the stored value instead of wiping it.
    settings.main_window_size = load_settings(&app).main_window_size;

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
    SHOW_NOTIFICATIONS.store(settings.show_notifications, Ordering::Relaxed);
    BACKGROUND_KEEP_ALIVE.store(settings.background_keep_alive, Ordering::Relaxed);
    sync_os_keep_alive(&app);

    let theme = parse_theme(settings.theme.as_deref());
    for window in app.webview_windows().values() {
        let _ = window.set_theme(theme);
        let effective = theme
            .or_else(|| window.theme().ok())
            .unwrap_or(tauri::Theme::Light);
        let _ = window.set_background_color(Some(theme_bg_color(effective)));
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.eval(&format!(
            "window.__GM_SHOW_NOTIFICATIONS = {}; window.__GM_BACKGROUND_KEEP_ALIVE = {};",
            settings.show_notifications, settings.background_keep_alive
        ));
    }
}

// Opens the settings window; invoked by the gear button injected into the
// Messages web UI (the tray menu calls show_settings_window directly).
#[tauri::command]
fn open_settings(app: tauri::AppHandle) {
    #[cfg(debug_assertions)]
    eprintln!("[ipc] open_settings invoked");
    if let Err(e) = show_settings_window(&app) {
        eprintln!("[open_settings] failed to open settings window: {e}");
    }
}

// Opens the local settings window, or focuses it if already open.
fn show_settings_window(app: &tauri::AppHandle) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("settings") {
        let _ = window.show();
        let _ = window.set_focus();
    } else {
        let settings = load_settings(app);
        let explicit_theme = parse_theme(settings.theme.as_deref());
        #[allow(unused_mut)]
        let mut builder =
            WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
                .title("Settings")
                .inner_size(500.0, 580.0)
                .min_inner_size(400.0, 460.0)
                .theme(explicit_theme)
                .background_color(theme_bg_color(explicit_theme.unwrap_or(tauri::Theme::Dark)))
                .center();
        #[cfg(windows)]
        {
            builder = builder.scroll_bar_style(tauri::webview::ScrollBarStyle::FluentOverlay);
        }
        let _window = builder.build()?;
    }
    Ok(())
}

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
            let mut settings = load_settings(app.handle());
            // Keep the persisted launch-at-login flag in sync with the OS/plugin.
            settings.launch_at_login = app.autolaunch().is_enabled().unwrap_or(false);
            save_settings(app.handle(), &settings);

            SHOW_NOTIFICATIONS.store(settings.show_notifications, Ordering::Relaxed);
            BACKGROUND_KEEP_ALIVE.store(settings.background_keep_alive, Ordering::Relaxed);
            #[cfg(target_os = "macos")]
            macos_native::init_notifications(app.handle());
            sync_os_keep_alive(app.handle());

            // Build the main window pointing at Google Messages. If the user
            // chose "Start in Tray", build it hidden.
            let start_url = settings.start_url.as_deref().unwrap_or(DEFAULT_START_URL);
            // Restore the remembered window geometry, clamped to the window
            // minimum in case the persisted file was edited or corrupted.
            let saved_geom = settings.main_window_size.clone();
            let (width, height) = saved_geom
                .as_ref()
                .map(|s| (s.width.max(400.0), s.height.max(500.0)))
                .unwrap_or((1100.0, 780.0));
            let init_script = INIT_SCRIPT
                .replace(
                    "__GM_NOTIFICATIONS_DEFAULT__",
                    if settings.show_notifications {
                        "true"
                    } else {
                        "false"
                    },
                )
                .replace(
                    "__GM_KEEP_ALIVE_DEFAULT__",
                    if settings.background_keep_alive {
                        "true"
                    } else {
                        "false"
                    },
                );
            let explicit_theme = parse_theme(settings.theme.as_deref());
            let mut builder =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::External(start_url.parse()?))
                    .title("Google Messages")
                    .inner_size(width, height)
                    .min_inner_size(400.0, 500.0)
                    .theme(explicit_theme)
                    .background_color(theme_bg_color(explicit_theme.unwrap_or(tauri::Theme::Dark)))
                    .visible(!settings.start_in_tray)
                    .background_throttling(
                        tauri::utils::config::BackgroundThrottlingPolicy::Disabled,
                    )
                    .initialization_script_for_all_frames(&init_script);
            #[cfg(not(target_os = "macos"))]
            {
                builder = builder.skip_taskbar(settings.start_in_tray);
            }
            #[cfg(windows)]
            {
                builder = builder
                    .scroll_bar_style(tauri::webview::ScrollBarStyle::FluentOverlay)
                    .additional_browser_args(
                        "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
                         --disable-background-timer-throttling \
                         --disable-renderer-backgrounding \
                         --disable-backgrounding-occluded-windows",
                    );
            }
            let new_win_app = app.handle().clone();
            builder = builder.on_new_window(move |url, _features| {
                let scheme = url.scheme();
                if scheme == "https" || scheme == "http" {
                    if url.host_str() == Some("messages.google.com") {
                        if let Some(main) = new_win_app.get_webview_window("main") {
                            let _ = main.navigate(url);
                        }
                    } else {
                        open_external(new_win_app.clone(), url.to_string());
                    }
                }
                tauri::webview::NewWindowResponse::Deny
            });
            if let Some(ref geom) = saved_geom {
                if let (Some(x), Some(y)) = (geom.x, geom.y) {
                    if is_position_on_screen(app.handle(), x, y, width) {
                        builder = builder.position(x, y);
                    } else {
                        builder = builder.center();
                    }
                } else {
                    builder = builder.center();
                }
                if geom.maximized {
                    builder = builder.maximized(true);
                }
            } else {
                builder = builder.center();
            }
            if let Some(ua) = USER_AGENT {
                builder = builder.user_agent(ua);
            }
            #[allow(unused_variables)]
            let window = builder.build()?;
            if explicit_theme.is_none() {
                if let Ok(os_theme) = window.theme() {
                    let _ = window.set_background_color(Some(theme_bg_color(os_theme)));
                }
            }

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
                let _ = app
                    .handle()
                    .set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
            start_background_listener(app.handle().clone());
            Ok(())
        })
        // Close the main window to tray instead of quitting; other windows
        // (Settings, Calls, Voicemail) close normally.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
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
            report_heartbeat,
            set_session_yielded,
            prompt_takeover_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

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
    use webkit2gtk::glib::Cast;
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

// Shows, unminimizes, and focuses the main window, restoring the Dock/taskbar icon.
fn show_main_window(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(window) = app.get_webview_window("main") {
        #[cfg(not(target_os = "macos"))]
        let _ = window.set_skip_taskbar(false);
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        apply_window_badge(&window, LAST_UNREAD_COUNT.load(Ordering::Relaxed));
    }
}

// Invoked from JS when a desktop notification is clicked so the main window is
// unminimized, unhidden from the tray, and brought to the foreground.
#[tauri::command]
fn focus_main_window(app: tauri::AppHandle) {
    #[cfg(debug_assertions)]
    eprintln!("[ipc] focus_main_window invoked");
    show_main_window(&app);
}

// Opens and focuses the main window and asks the user whether to move the
// active Google Messages session back to this computer.
fn open_and_prompt_takeover(app: &tauri::AppHandle) {
    show_main_window(app);
    if let Some(window) = app.get_webview_window("main") {
        let _ =
            window.eval("window.__GM_SHOW_TAKEOVER_PROMPT && window.__GM_SHOW_TAKEOVER_PROMPT();");
    }
}

#[tauri::command]
fn prompt_takeover_session(app: tauri::AppHandle) {
    #[cfg(debug_assertions)]
    eprintln!("[ipc] prompt_takeover_session invoked");
    open_and_prompt_takeover(&app);
}

// Syncs the OS-level background activity assertion (macOS App Nap prevention)
// with the current `BACKGROUND_KEEP_ALIVE` setting and `SESSION_YIELDED` state.
// When the session is yielded to another computer (or keep-alive is disabled),
// the assertion is released so the OS can sleep the app normally.
fn sync_os_keep_alive(app: &tauri::AppHandle) {
    let active =
        BACKGROUND_KEEP_ALIVE.load(Ordering::Relaxed) && !SESSION_YIELDED.load(Ordering::Relaxed);
    #[cfg(target_os = "macos")]
    macos_native::set_keep_alive(app, active);
    #[cfg(not(target_os = "macos"))]
    let _ = (app, active);
}

// Called from JS when "Google Messages is open on another computer" appears or
// clears. When `yielded` becomes true, we pause background keep-alive/reloads
// (allowing OS sleep) and pop a single notification offering to switch back.
#[tauri::command]
fn set_session_yielded(app: tauri::AppHandle, yielded: bool) {
    #[cfg(debug_assertions)]
    eprintln!("[ipc] set_session_yielded({yielded})");

    let was_yielded = SESSION_YIELDED.swap(yielded, Ordering::Relaxed);
    sync_os_keep_alive(&app);

    if yielded {
        PENDING_HEARTBEAT_PINGS.store(0, Ordering::Relaxed);
        UNHEALTHY_STREAK.store(0, Ordering::Relaxed);
        if !was_yielded && !TAKEOVER_NOTIFIED.swap(true, Ordering::Relaxed) {
            let main_focused = app.get_webview_window("main").is_some_and(|w| {
                w.is_visible().unwrap_or(false) && w.is_focused().unwrap_or(false)
            });
            if !main_focused && SHOW_NOTIFICATIONS.load(Ordering::Relaxed) {
                send_os_notification(
                    &app,
                    "Google Messages is open on another computer".to_string(),
                    "Click to open and move the session here, or ignore to let the app sleep."
                        .to_string(),
                    true,
                );
            }
        }
    } else {
        TAKEOVER_NOTIFIED.store(false, Ordering::Relaxed);
    }
}

#[tauri::command]
fn send_native_notification(app: tauri::AppHandle, title: String, body: String, kind: String) {
    #[cfg(debug_assertions)]
    eprintln!("[ipc] send_native_notification(kind={kind}, title={title})");
    if !SHOW_NOTIFICATIONS.load(Ordering::Relaxed) {
        return;
    }
    send_os_notification(&app, title, body, kind == "takeover");
}

#[tauri::command]
fn report_heartbeat(app: tauri::AppHandle, healthy: bool, reason: Option<String>) {
    #[cfg(debug_assertions)]
    eprintln!("[ipc] report_heartbeat(healthy={healthy}, reason={reason:?})");

    PENDING_HEARTBEAT_PINGS.store(0, Ordering::Relaxed);
    if healthy || reason.as_deref() == Some("offline") {
        UNHEALTHY_STREAK.store(0, Ordering::Relaxed);
        return;
    }

    if !BACKGROUND_KEEP_ALIVE.load(Ordering::Relaxed) || SESSION_YIELDED.load(Ordering::Relaxed) {
        UNHEALTHY_STREAK.store(0, Ordering::Relaxed);
        return;
    }

    let streak = UNHEALTHY_STREAK.fetch_add(1, Ordering::Relaxed) + 1;
    if streak >= 2 {
        UNHEALTHY_STREAK.store(0, Ordering::Relaxed);
        try_reload_main_window(&app, "stalled connection");
    }
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn try_reload_main_window(app: &tauri::AppHandle, reason: &str) {
    let now = now_epoch_secs();
    let last = LAST_AUTO_RELOAD_SECS.load(Ordering::Relaxed);
    // Enforce a 120-second cooldown so auto-reload can never loop.
    if now.saturating_sub(last) < 120 {
        return;
    }
    LAST_AUTO_RELOAD_SECS.store(now, Ordering::Relaxed);
    #[cfg(debug_assertions)]
    eprintln!("[watchdog] reloading main window ({reason})");
    let _ = reason;
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.eval(
            "if (location.host === 'messages.google.com' && navigator.onLine !== false) { location.reload(); }",
        );
    }
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

            if !BACKGROUND_KEEP_ALIVE.load(Ordering::Relaxed)
                || SESSION_YIELDED.load(Ordering::Relaxed)
            {
                PENDING_HEARTBEAT_PINGS.store(0, Ordering::Relaxed);
                UNHEALTHY_STREAK.store(0, Ordering::Relaxed);
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

            if let Some(window) = app.get_webview_window("main") {
                let arg = if woke_from_sleep { "true" } else { "false" };
                let _ = window.eval(&format!(
                    "window.__gmHeartbeat && window.__gmHeartbeat({arg});"
                ));
            }
        }
    });
}

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
    use std::sync::atomic::{AtomicU32, Ordering};

    static NOTIF_SEQ: AtomicU32 = AtomicU32::new(1);

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
                let clicked = activation == NSUserNotificationActivationType::ContentsClicked
                    || activation == NSUserNotificationActivationType::ActionButtonClicked;
                if !clicked {
                    return;
                }
                let app = &self.ivars().app;
                if id.starts_with("gm-takeover") {
                    crate::open_and_prompt_takeover(app);
                } else {
                    crate::show_main_window(app);
                }
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
        title: String,
        body: String,
        is_takeover: bool,
    ) {
        let _ = app.run_on_main_thread(move || {
            #[allow(deprecated)]
            {
                let notif = NSUserNotification::new();
                notif.setTitle(Some(&NSString::from_str(&title)));
                if !body.is_empty() {
                    notif.setInformativeText(Some(&NSString::from_str(&body)));
                }
                let seq = NOTIF_SEQ.fetch_add(1, Ordering::Relaxed);
                let id = if is_takeover {
                    format!("gm-takeover-{seq}")
                } else {
                    format!("gm-msg-{seq}")
                };
                notif.setIdentifier(Some(&NSString::from_str(&id)));
                unsafe {
                    notif.setSoundName(Some(NSUserNotificationDefaultSoundName));
                }
                if is_takeover {
                    notif.setHasActionButton(true);
                    notif.setActionButtonTitle(ns_string!("Switch Here"));
                    notif.setOtherButtonTitle(ns_string!("No"));
                }
                let center = NSUserNotificationCenter::defaultUserNotificationCenter();
                center.deliverNotification(&notif);
            }
        });
    }
}

#[cfg(target_os = "macos")]
fn send_os_notification(app: &tauri::AppHandle, title: String, body: String, is_takeover: bool) {
    macos_native::send_notification(app, title, body, is_takeover);
}

#[cfg(target_os = "linux")]
fn send_os_notification(app: &tauri::AppHandle, title: String, body: String, is_takeover: bool) {
    let app = app.clone();
    std::thread::spawn(move || {
        let mut n = notify_rust::Notification::new();
        n.summary(&title).body(&body).auto_icon();
        if is_takeover {
            n.action("default", "Open")
                .action("use_here", "Switch Here")
                .action("no", "No");
        } else {
            n.action("default", "Open");
        }
        if let Ok(handle) = n.show() {
            handle.wait_for_action(|action| match action {
                "default" | "use_here" => {
                    if is_takeover {
                        open_and_prompt_takeover(&app);
                    } else {
                        show_main_window(&app);
                    }
                }
                _ => {}
            });
        }
    });
}

#[cfg(windows)]
fn send_os_notification(app: &tauri::AppHandle, title: String, body: String, is_takeover: bool) {
    if let Some(window) = app.get_webview_window("main") {
        if let (Ok(t), Ok(b)) = (serde_json::to_string(&title), serde_json::to_string(&body)) {
            let kind = if is_takeover {
                "\"takeover\""
            } else {
                "\"message\""
            };
            let _ = window.eval(&format!(
                "window.__gmShowWebviewNotification && window.__gmShowWebviewNotification({t}, {b}, {kind});"
            ));
        }
    }
}

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
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_zoom(next as f64 / 100.0);
    }
}

#[tauri::command]
fn adjust_zoom(app: tauri::AppHandle, action: String) {
    #[cfg(debug_assertions)]
    eprintln!("[ipc] adjust_zoom({action})");
    apply_zoom_action(&app, &action);
}

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
        "Quit Google Messages",
        true,
        Some("CmdOrCtrl+Q"),
    )?;
    let app_submenu = Submenu::with_items(
        app,
        "Google Messages",
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
        "app_settings" => {
            let _ = show_settings_window(app);
        }
        "app_quit" => {
            if let Some(window) = app.get_webview_window("main") {
                save_main_window_size(&window.as_ref().window());
            }
            app.exit(0);
        }
        "view_reload" => {
            if let Some(window) = app.get_webview_window("main") {
                if window.is_focused().unwrap_or(false) {
                    let _ = window.eval("window.location.reload();");
                }
            }
        }
        action @ ("view_zoom_reset" | "view_zoom_in" | "view_zoom_out") => {
            if app
                .get_webview_window("main")
                .is_some_and(|w| w.is_focused().unwrap_or(false))
            {
                let zoom = match action {
                    "view_zoom_reset" => "reset",
                    "view_zoom_in" => "in",
                    _ => "out",
                };
                apply_zoom_action(app, zoom);
            }
        }
        _ => {}
    });

    Ok(())
}

// Saves geometry and hides the main window to the tray, removing the Dock/taskbar icon.
fn hide_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        save_main_window_size(&window.as_ref().window());
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
fn save_main_window_size(window: &tauri::Window) {
    // Minimized or hidden windows can report bogus positions/sizes (such as
    // (-32000, -32000) on Windows); preserve the last valid geometry instead.
    if window.is_minimized().unwrap_or(false) || !window.is_visible().unwrap_or(true) {
        return;
    }

    let app = window.app_handle();
    let mut settings = load_settings(app);
    let maximized = window.is_maximized().unwrap_or(false);

    if maximized {
        // Keep the previous unmaximized size/position if we have one so
        // unmaximizing on a future session restores reasonable bounds.
        let mut geom = settings.main_window_size.unwrap_or(WindowSize {
            width: 1100.0,
            height: 780.0,
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

    // Tray glyph, per platform:
    //   - macOS: a monochrome (black-on-transparent) template; the OS recolors
    //     it for light/dark menu bars. The full app icon would render as a solid
    //     square here, since template mode only uses the alpha channel.
    //   - Windows/Linux: a colored bubble. Those platforms don't support
    //     template images, so a black icon would be invisible on a dark taskbar.
    let tray_icon = load_base_tray_icon()?;

    #[allow(unused_mut)] // `tray` is only reassigned on macOS (template flag)
    let mut tray = TrayIconBuilder::with_id("main")
        .icon(tray_icon)
        .tooltip("Google Messages")
        .menu(&menu)
        .show_menu_on_left_click(false);

    // Template rendering is a macOS-only concept (light/dark menu-bar tinting).
    #[cfg(target_os = "macos")]
    {
        tray = tray.icon_as_template(true);
    }

    tray.on_menu_event(|app, event| match event.id().as_ref() {
        "show" => {
            show_main_window(app);
        }
        "settings" => {
            let _ = show_settings_window(app);
        }
        "quit" => {
            // Capture the final window size before exiting; the visible
            // window never fires CloseRequested on this path.
            if let Some(window) = app.get_webview_window("main") {
                save_main_window_size(&window.as_ref().window());
            }
            app.exit(0);
        }
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
            if let Some(window) = app.get_webview_window("main") {
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

// Returns the base (no-unread) tray icon for the current platform.
fn load_base_tray_icon() -> tauri::Result<tauri::image::Image<'static>> {
    if let Some((rgba, width, height)) = BASE_TRAY_ICON.get() {
        return Ok(tauri::image::Image::new_owned(
            rgba.clone(),
            *width,
            *height,
        ));
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
    if let Some((rgba, width, height)) = BADGED_TRAY_ICON.get() {
        return Ok(tauri::image::Image::new_owned(
            rgba.clone(),
            *width,
            *height,
        ));
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

    let _ = BADGED_TRAY_ICON.set((rgba.clone(), width, height));
    Ok(tauri::image::Image::new_owned(rgba, width, height))
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

    let text = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    let chars: Vec<char> = text.chars().collect();
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
        if count > 0 {
            let badge = render_windows_overlay_badge(count);
            let _ = window.set_overlay_icon(Some(badge));
        } else {
            let _ = window.set_overlay_icon(None);
        }
    }
}

// Called from JS when the unread count changes
#[tauri::command]
fn set_tray_badge(app: tauri::AppHandle, count: u32) {
    // Fires on page load via the injected title observer, so its presence in
    // dev stderr doubles as proof that IPC from the remote origin works.
    #[cfg(debug_assertions)]
    eprintln!("[ipc] set_tray_badge({count})");

    LAST_UNREAD_COUNT.store(count, Ordering::Relaxed);
    if LAST_TRAY_COUNT.swap(count, Ordering::Relaxed) == count {
        return;
    }

    if let Some(window) = app.get_webview_window("main") {
        apply_window_badge(&window, count);
    }

    if let Some(tray) = app.tray_by_id("main") {
        if count > 0 {
            let label = if count > 99 {
                "99+".to_string()
            } else {
                count.to_string()
            };
            // Badge tooltip so the count is visible on hover
            let _ = tray.set_tooltip(Some(&format!("Google Messages ({label} unread)")));
            if let Ok(icon) = load_badged_tray_icon() {
                let _ = tray.set_icon(Some(icon));
                #[cfg(target_os = "macos")]
                let _ = tray.set_icon_as_template(true);
            }
            #[cfg(target_os = "macos")]
            let _ = tray.set_title(Some(&label));
        } else {
            let _ = tray.set_tooltip(Some("Google Messages"));
            if let Ok(icon) = load_base_tray_icon() {
                let _ = tray.set_icon(Some(icon));
                #[cfg(target_os = "macos")]
                let _ = tray.set_icon_as_template(true);
            }
            #[cfg(target_os = "macos")]
            let _ = tray.set_title(Some(""));
        }
    }
}

// Injected into the webview on load (including subframes):
//  - Disables WebAuthn/passkeys so Google Sign-In skips broken embedded-webview
//    passkey prompts and goes straight to "Approve on another device" / password
//  - Observes the page title for unread count changes and calls set_tray_badge
//  - Adds a floating gear button that opens the app settings window
//  - Routes target="_blank" links (in-window for Messages, OS browser otherwise)
//  - Wraps the Notification API to honor the "Show notifications" setting
// The __GM_NOTIFICATIONS_DEFAULT__ token is replaced with the persisted
// setting before injection.
const INIT_SCRIPT: &str = r#"
(function () {
    // --- Disable WebAuthn / Passkeys ---
    // Embedded webviews (WKWebView on macOS, WebKitGTK on Linux, WebView2 on
    // Windows) cannot satisfy third-party Relying Party / Associated Domain
    // attestation for google.com, so passkey challenges fail after the user
    // enters their email. By reporting that platform authenticators and
    // PublicKeyCredential are unavailable in every frame, accounts.google.com
    // skips the broken passkey step and goes directly to "Approve on another
    // device" (Google Prompt) or password sign-in.
    try {
        if (window.PublicKeyCredential) {
            if (typeof window.PublicKeyCredential.isUserVerifyingPlatformAuthenticatorAvailable === 'function') {
                window.PublicKeyCredential.isUserVerifyingPlatformAuthenticatorAvailable = function () {
                    return Promise.resolve(false);
                };
            }
            if (typeof window.PublicKeyCredential.isConditionalMediationAvailable === 'function') {
                window.PublicKeyCredential.isConditionalMediationAvailable = function () {
                    return Promise.resolve(false);
                };
            }
            if (typeof window.PublicKeyCredential.getClientCapabilities === 'function') {
                window.PublicKeyCredential.getClientCapabilities = function () {
                    return Promise.resolve({
                        conditionalCreate: false,
                        conditionalGet: false,
                        hybridTransport: false,
                        passkeyPlatformAuthenticator: false,
                        relatedOrigins: false,
                        signalAllAcceptedCredentials: false,
                        signalCurrentUserDetails: false,
                        signalUnknownCredential: false,
                        userVerifyingPlatformAuthenticator: false,
                    });
                };
            }
        }
        try {
            delete window.PublicKeyCredential;
        } catch (_) {
            Object.defineProperty(window, 'PublicKeyCredential', {
                value: undefined,
                configurable: true,
                writable: true,
            });
        }
        if (navigator.credentials) {
            const origGet = navigator.credentials.get;
            if (origGet) {
                navigator.credentials.get = function (options) {
                    if (options && options.publicKey) {
                        return Promise.reject(
                            new DOMException('Passkeys are not supported in this embedded webview.', 'NotSupportedError')
                        );
                    }
                    return origGet.apply(navigator.credentials, arguments);
                };
            }
            const origCreate = navigator.credentials.create;
            if (origCreate) {
                navigator.credentials.create = function (options) {
                    if (options && options.publicKey) {
                        return Promise.reject(
                            new DOMException('Passkeys are not supported in this embedded webview.', 'NotSupportedError')
                        );
                    }
                    return origCreate.apply(navigator.credentials, arguments);
                };
            }
        }
    } catch (_) {}

    // The remaining hooks (unread badge, settings gear, link routing,
    // Notification wrapper) only belong in the top-level frame.
    if (window !== window.top) {
        return;
    }

    function invoke(cmd, args) {
        if (window.__TAURI_INTERNALS__) {
            return window.__TAURI_INTERNALS__.invoke(cmd, args);
        }
        return Promise.reject(new Error('Tauri IPC unavailable'));
    }

    // --- Unread badge via title observation (+ fallback notification) ---
    function parseUnread(title) {
        const m = title.match(/\((\d+)\)/);
        return m ? parseInt(m[1], 10) : 0;
    }

    let lastUnreadCount = null;
    let lastWebNotifAt = 0;

    function findLatestUnreadPreview() {
        const items = document.querySelectorAll('mws-conversation-list-item');
        for (let i = 0; i < items.length; i++) {
            const el = items[i];
            const aria = (el.getAttribute('aria-label') || '') + ' ' +
                ((el.querySelector('[aria-label]') && el.querySelector('[aria-label]').getAttribute('aria-label')) || '');
            const isUnread = /unread/i.test(aria) || el.querySelector('.unread, [data-e2e-is-unread="true"]');
            if (isUnread) {
                const nameEl = el.querySelector('[data-e2e-conversation-name], .name, h2, h3');
                const snipEl = el.querySelector('mws-conversation-snippet, [data-e2e-conversation-snippet], .snippet');
                const title = nameEl && nameEl.textContent ? nameEl.textContent.trim() : 'Google Messages';
                const body = snipEl && snipEl.textContent ? snipEl.textContent.trim() : 'New unread message';
                if (title || body) {
                    return { title: title || 'Google Messages', body: body || 'New unread message' };
                }
            }
        }
        return null;
    }

    function syncBadge() {
        const count = parseUnread(document.title);
        if (count === lastUnreadCount) return;
        const prev = lastUnreadCount;
        lastUnreadCount = count;
        invoke('set_tray_badge', { count }).catch(function () {});

        // Safety-net notification if the unread count increased while the
        // window is unfocused/hidden and the web app didn't fire Notification.
        if (prev !== null && count > prev && window.__GM_SHOW_NOTIFICATIONS !== false && !document.hasFocus()) {
            setTimeout(function () {
                if (Date.now() - lastWebNotifAt < 3000) return;
                if (detectOtherSession()) return;
                const preview = findLatestUnreadPreview();
                const title = preview ? preview.title : 'Google Messages';
                const body = preview
                    ? preview.body
                    : (count === 1 ? 'You have 1 unread message' : 'You have ' + count + ' unread messages');
                dispatchNotification(title, body, 'message');
            }, 1500);
        }
    }

    const titleObserver = new MutationObserver(syncBadge);
    // Observe <title> changes
    const observeTitle = () => {
        const titleEl = document.querySelector('title');
        if (titleEl) {
            titleObserver.observe(titleEl, { childList: true, characterData: true, subtree: true });
            syncBadge();
        } else {
            setTimeout(observeTitle, 500);
        }
    };

    // --- Settings gear, docked left of the Google account avatar ---
    // The avatar is looked up by stable-ish attributes and re-checked as the
    // SPA re-renders; if it can't be found, fall back to a fixed position
    // near where it normally sits (top-right, left of the avatar).
    const GEAR_BASE_CSS =
        'width:34px !important;height:34px !important;min-width:34px !important;min-height:34px !important;' +
        'padding:0 !important;border:none !important;border-radius:50% !important;' +
        'background:rgba(128,128,128,.2) !important;color:inherit !important;' +
        'display:inline-flex !important;align-items:center !important;justify-content:center !important;' +
        'cursor:pointer !important;opacity:.82;box-sizing:border-box !important;flex-shrink:0 !important;' +
        'transition:opacity .15s ease,background-color .15s ease;';
    function makeGear() {
        const btn = document.createElement('button');
        btn.id = 'gm-app-settings';
        btn.type = 'button';
        btn.title = 'App settings';
        btn.setAttribute('aria-label', 'App settings');

        const svgNs = 'http://www.w3.org/2000/svg';
        const svg = document.createElementNS(svgNs, 'svg');
        svg.setAttribute('viewBox', '0 0 24 24');
        svg.setAttribute('width', '22');
        svg.setAttribute('height', '22');
        svg.setAttribute('fill', 'currentColor');
        svg.setAttribute('aria-hidden', 'true');
        svg.style.cssText =
            'width:22px !important;height:22px !important;min-width:22px !important;min-height:22px !important;' +
            'display:block !important;flex-shrink:0 !important;pointer-events:none !important;';

        const path = document.createElementNS(svgNs, 'path');
        path.setAttribute(
            'd',
            'M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58a.49.49 0 0 0 .12-.61l-1.92-3.32a.49.49 0 0 0-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54a.484.484 0 0 0-.48-.41h-3.84c-.24 0-.43.17-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96a.49.49 0 0 0-.59.22L2.74 8.87c-.12.21-.08.47.12.61l2.03 1.58c-.05.3-.09.63-.09.94s.02.64.07.94l-2.03 1.58a.49.49 0 0 0-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32c.12-.22.07-.47-.12-.61l-2.01-1.58zM12 15.6c-1.98 0-3.6-1.62-3.6-3.6s1.62-3.6 3.6-3.6 3.6 1.62 3.6 3.6-1.62 3.6-3.6 3.6z'
        );
        svg.appendChild(path);
        btn.appendChild(svg);

        btn.addEventListener('mouseenter', function () {
            btn.style.opacity = '1';
            btn.style.setProperty('background', 'rgba(128,128,128,.32)', 'important');
        });
        btn.addEventListener('mouseleave', function () {
            btn.style.opacity = '.82';
            btn.style.setProperty('background', 'rgba(128,128,128,.2)', 'important');
        });
        btn.addEventListener('click', function (e) {
            e.preventDefault();
            e.stopPropagation();
            invoke('open_settings').catch(function (err) {
                console.error('[gm] open_settings failed:', err);
            });
        });
        return btn;
    }
    function findAvatar() {
        const els = document.querySelectorAll(
            'a[aria-label*="Google Account"], a[href*="accounts.google.com"], a[href*="SignOutOptions"]');
        for (let i = 0; i < els.length; i++) {
            const r = els[i].getBoundingClientRect();
            if (r.width > 0 && r.top >= 0 && r.top < 150) return els[i];
        }
        return null;
    }
    function placeGear() {
        let btn = document.getElementById('gm-app-settings');
        if (btn && btn.isConnected && btn.dataset.anchored === '1') return;
        if (!btn) btn = makeGear();
        const avatar = findAvatar();
        if (avatar && avatar.parentElement) {
            btn.style.cssText = GEAR_BASE_CSS +
                'vertical-align:middle;align-self:center;margin:0 8px;';
            avatar.parentElement.insertBefore(btn, avatar);
            btn.dataset.anchored = '1';
        } else if (!btn.isConnected) {
            btn.style.cssText = GEAR_BASE_CSS +
                'position:fixed;top:14px;right:64px;z-index:2147483647;';
            btn.dataset.anchored = '0';
            document.body.appendChild(btn);
        }
    }
    // Cheap when already anchored (three property checks), so observing the
    // whole body is fine — it keeps the gear alive across SPA re-renders.
    const gearObserver = new MutationObserver(placeGear);

    // --- Desktop UI polish: suppress browser overscroll & ghost-dragging ---
    function injectDesktopStyles() {
        if (document.getElementById('gm-desktop-polish')) return;
        const style = document.createElement('style');
        style.id = 'gm-desktop-polish';
        style.textContent =
            'html, body { overscroll-behavior: none !important; }' +
            'img, button, [role="tab"], [role="menuitem"] {' +
            '  -webkit-user-drag: none; user-select: none;' +
            '}' +
            'a, [role="button"] {' +
            '  -webkit-user-drag: none;' +
            '}';
        (document.head || document.documentElement).appendChild(style);
    }
    document.addEventListener('dragstart', function (e) {
        const t = e.target;
        if (t && (t.tagName === 'IMG' || t.tagName === 'A' || (t.closest && t.closest('a, button, [role="button"]')))) {
            e.preventDefault();
        }
    }, true);

    function onReady() {
        injectDesktopStyles();
        observeTitle();
        placeGear();
        gearObserver.observe(document.body, { childList: true, subtree: true });
        observeSessionState();
    }
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', onReady);
    } else {
        onReady();
    }

    // --- Route external & target="_blank" / window.open links ---
    // Handle link navigations so the main window stays on Google Messages:
    //   - messages.google.com (Messages / Calls / Voicemail) -> navigate in the
    //     same window, so they behave like tabs.
    //   - any other non-auth site (e.g. fi.google.com/account or links inside
    //     messages) -> open in the OS default browser via open_external, even if
    //     the anchor omits target="_blank" or sits inside a Shadow DOM.
    function parseUrl(url) {
        try {
            return new URL(url, location.href);
        } catch (e) {
            return null;
        }
    }
    function openExternal(href) {
        invoke('open_external', { url: href })
            .catch(function (e) { console.error('[gm] open_external failed:', e); });
    }
    function isInAppHost(host) {
        return host === 'messages.google.com' || host === 'accounts.google.com';
    }
    function onTrustedOrigin() {
        return isInAppHost(location.host);
    }
    // Returns true if the link was handled here.
    function route(u) {
        if (!u || (u.protocol !== 'https:' && u.protocol !== 'http:')) {
            return false;
        }
        if (u.host === 'messages.google.com') {
            window.location.assign(u.href);
        } else {
            openExternal(u.href);
        }
        return true;
    }
    function findAnchor(e) {
        if (e.target && e.target.closest) {
            const direct = e.target.closest('a[href]');
            if (direct) return direct;
        }
        const path = e.composedPath ? e.composedPath() : [];
        for (let i = 0; i < path.length; i++) {
            const el = path[i];
            if (el && el.tagName === 'A' && el.getAttribute && el.getAttribute('href')) {
                return el;
            }
        }
        return null;
    }
    function handleLinkClick(e) {
        if (!onTrustedOrigin()) return;
        if (e.type === 'auxclick' && e.button !== 1) return;
        const a = findAnchor(e);
        if (!a) return;
        const rawHref = a.getAttribute('href');
        if (!rawHref || rawHref.startsWith('#')) {
            if (e.type === 'auxclick') {
                e.preventDefault();
            }
            return;
        }
        const u = parseUrl(a.href);
        if (!u || (u.protocol !== 'https:' && u.protocol !== 'http:')) return;
        if (e.type === 'auxclick' && isInAppHost(u.host) && a.target !== '_blank') {
            e.preventDefault();
            return;
        }
        if (a.target === '_blank' || e.type === 'auxclick' || !isInAppHost(u.host)) {
            if (route(u)) {
                e.preventDefault();
                e.stopPropagation();
            }
        }
    }
    // Capture phase so we beat the app's own click handlers.
    document.addEventListener('click', handleLinkClick, true);
    document.addEventListener('auxclick', handleLinkClick, true);
    const origOpen = window.open;
    window.open = function (url) {
        if (onTrustedOrigin()) {
            const u = url ? parseUrl(url) : null;
            if (u && route(u)) {
                return null;
            }
        }
        return origOpen.apply(window, arguments);
    };

    // --- Cross-platform desktop keyboard shortcuts ---
    // Uses Cmd on macOS and Ctrl on Windows/Linux:
    //   Mod + ,        -> Open Settings window
    //   Mod + N        -> Start a new chat
    //   Mod + F / K    -> Focus conversation search
    //   Mod + 1..9     -> Switch to 1st..9th conversation in sidebar
    //   Mod + = / - / 0 -> Zoom in / out / reset
    //   Mod + R / F5   -> Reload
    const isMac = /Mac|iPod|iPhone|iPad/.test(navigator.platform || '');
    function startNewChat() {
        const selectors = [
            'a[href*="/web/conversations/new"]',
            'mws-fab a',
            'mws-fab button',
            '[data-e2e-start-button]',
            'button[aria-label*="Start chat" i]',
            'a[aria-label*="Start chat" i]',
            '.start-chat button',
            '.fab-label'
        ];
        for (let i = 0; i < selectors.length; i++) {
            const el = document.querySelector(selectors[i]);
            if (el) {
                el.click();
                return true;
            }
        }
        return false;
    }
    function focusSearch() {
        const inputSelectors = [
            'input[placeholder*="Search" i]',
            'input[aria-label*="Search" i]',
            'mws-search-box input',
            'input[type="search"]'
        ];
        for (let i = 0; i < inputSelectors.length; i++) {
            const inp = document.querySelector(inputSelectors[i]);
            if (inp && inp.offsetParent !== null) {
                inp.focus();
                if (inp.select) inp.select();
                return true;
            }
        }
        const btnSelectors = [
            'button[aria-label*="Search" i]',
            '[data-e2e-search-button]',
            'mws-search-box button'
        ];
        for (let i = 0; i < btnSelectors.length; i++) {
            const btn = document.querySelector(btnSelectors[i]);
            if (btn) {
                btn.click();
                setTimeout(function () {
                    for (let j = 0; j < inputSelectors.length; j++) {
                        const inp = document.querySelector(inputSelectors[j]);
                        if (inp) {
                            inp.focus();
                            if (inp.select) inp.select();
                            break;
                        }
                    }
                }, 50);
                return true;
            }
        }
        return false;
    }
    function selectConversationByIndex(idx) {
        let items = document.querySelectorAll(
            'mws-conversation-list-item a, mws-conversation-list-item [role="listitem"], mws-conversation-list-item'
        );
        if (!items || items.length === 0) {
            items = document.querySelectorAll('a[href*="/web/conversations/"]:not([href$="/new"])');
        }
        if (items && items[idx]) {
            const el = items[idx];
            const target = el.querySelector ? (el.querySelector('a') || el) : el;
            target.click();
            return true;
        }
        return false;
    }
    document.addEventListener('keydown', function (e) {
        if (e.key === 'F5' && !e.ctrlKey && !e.metaKey && !e.altKey) {
            e.preventDefault();
            window.location.reload();
            return;
        }
        // Suppress browser-only function keys on Windows/Linux (F3 find bar, F7 caret browsing)
        if (!isMac && (e.key === 'F3' || e.key === 'F7')) {
            e.preventDefault();
            return;
        }
        const mod = isMac ? (e.metaKey && !e.ctrlKey) : (e.ctrlKey && !e.metaKey);
        if (!mod || e.altKey) return;

        const key = e.key;
        if ((key === 'n' || key === 'N') && !e.shiftKey) {
            if (startNewChat()) {
                e.preventDefault();
            }
        } else if ((key === 'f' || key === 'F' || key === 'k' || key === 'K') && !e.shiftKey) {
            if (focusSearch()) {
                e.preventDefault();
            }
        } else if (!e.shiftKey && key >= '1' && key <= '9') {
            const idx = key.charCodeAt(0) - 49;
            if (selectConversationByIndex(idx)) {
                e.preventDefault();
            }
        } else if (!isMac) {
            // macOS handles Settings, Zoom, and Reload via the native app menu bar;
            // handling them here as well would fire them twice.
            if (key === ',' && !e.shiftKey) {
                e.preventDefault();
                invoke('open_settings').catch(function () {});
            } else if (key === '=' || key === '+') {
                e.preventDefault();
                invoke('adjust_zoom', { action: 'in' }).catch(function () {});
            } else if (key === '-' || key === '_') {
                e.preventDefault();
                invoke('adjust_zoom', { action: 'out' }).catch(function () {});
            } else if (key === '0' && !e.shiftKey) {
                e.preventDefault();
                invoke('adjust_zoom', { action: 'reset' }).catch(function () {});
            } else if ((key === 'r' || key === 'R') && !e.shiftKey) {
                e.preventDefault();
                window.location.reload();
            } else if (/^[pjuosgPJUOSG]$/.test(key)) {
                // Suppress Edge WebView2 print (Ctrl+P), downloads (Ctrl+J),
                // view-source (Ctrl+U), open/save file (Ctrl+O/S), find-next (Ctrl+G)
                e.preventDefault();
            }
        }
    }, true);

    // --- Notifications, Session-Takeover Protection & Background Keep-Alive ---
    function focusAppWindow() {
        invoke('focus_main_window').catch(function () {});
    }

    window.__GM_SHOW_NOTIFICATIONS = __GM_NOTIFICATIONS_DEFAULT__;
    window.__GM_BACKGROUND_KEEP_ALIVE = __GM_KEEP_ALIVE_DEFAULT__;

    const OrigNotification = window.Notification;
    const useWebviewNotification = !isMac && !/Linux/.test(navigator.platform || '') && typeof OrigNotification === 'function';

    if (useWebviewNotification && OrigNotification.permission === 'default' && window.__GM_SHOW_NOTIFICATIONS !== false) {
        try {
            const p = OrigNotification.requestPermission();
            if (p && p.catch) p.catch(function () {});
        } catch (_) {}
    }

    function createNotificationStub() {
        const listeners = {};
        return {
            onclick: null,
            onshow: null,
            onclose: null,
            onerror: null,
            close: function () {
                if (typeof this.onclose === 'function') {
                    try { this.onclose(); } catch (_) {}
                }
            },
            addEventListener: function (type, fn) {
                if (!listeners[type]) listeners[type] = [];
                listeners[type].push(fn);
            },
            removeEventListener: function (type, fn) {
                if (!listeners[type]) return;
                listeners[type] = listeners[type].filter(function (f) { return f !== fn; });
            },
            dispatchEvent: function (evt) {
                const list = (evt && listeners[evt.type]) || [];
                for (let i = 0; i < list.length; i++) {
                    try { list[i](evt); } catch (_) {}
                }
                return true;
            }
        };
    }

    function dispatchNotification(title, body, kind) {
        if (window.__GM_SHOW_NOTIFICATIONS === false) {
            return createNotificationStub();
        }
        if (kind === 'message') {
            lastWebNotifAt = Date.now();
        }
        if (useWebviewNotification) {
            try {
                const n = new OrigNotification(title, {
                    body: body || '',
                    tag: kind === 'takeover' ? 'gm-session-takeover' : undefined
                });
                n.addEventListener('click', function () {
                    if (kind === 'takeover') {
                        invoke('prompt_takeover_session').catch(function () {});
                    } else {
                        focusAppWindow();
                    }
                });
                return n;
            } catch (_) {}
        }
        invoke('send_native_notification', {
            title: String(title || 'Google Messages'),
            body: String(body || ''),
            kind: kind || 'message'
        }).catch(function () {});
        return createNotificationStub();
    }

    window.__gmShowWebviewNotification = function (title, body, kind) {
        dispatchNotification(title, body, kind || 'message');
    };

    function GMNotification(title, options) {
        const opts = options || {};
        return dispatchNotification(title, opts.body || '', 'message');
    }
    GMNotification.requestPermission = function () {
        if (useWebviewNotification && OrigNotification.requestPermission) {
            return OrigNotification.requestPermission();
        }
        return Promise.resolve('granted');
    };
    Object.defineProperty(GMNotification, 'permission', {
        get: function () {
            if (useWebviewNotification && OrigNotification.permission && OrigNotification.permission !== 'default') {
                return OrigNotification.permission;
            }
            return 'granted';
        }
    });
    window.Notification = GMNotification;

    // --- "Google Messages is open on another computer" protection ---
    function isElementVisible(el) {
        if (!el) return false;
        const r = el.getBoundingClientRect ? el.getBoundingClientRect() : null;
        return !!(r && r.width > 0 && r.height > 0);
    }

    function findUseHereButton() {
        if (location.host !== 'messages.google.com') return null;
        const buttons = document.querySelectorAll('button, [role="button"]');
        for (let i = 0; i < buttons.length; i++) {
            const btn = buttons[i];
            if (!isElementVisible(btn)) continue;
            const label = ((btn.textContent || '') + ' ' + (btn.getAttribute('aria-label') || '')).trim();
            if (/^use here$/i.test(btn.textContent ? btn.textContent.trim() : '') || /\buse here\b/i.test(label)) {
                // Confirm an ancestor container mentions another computer/window/tab
                let cur = btn.parentElement;
                for (let depth = 0; cur && depth < 6; depth++, cur = cur.parentElement) {
                    const txt = cur.textContent || '';
                    if (txt.length < 600 && /another (computer|window|tab|browser)|open (on|in) another/i.test(txt)) {
                        return btn;
                    }
                }
            }
        }
        return null;
    }

    function detectOtherSession() {
        return !!findUseHereButton();
    }

    let localSessionYielded = false;
    function syncSessionYielded(yielded) {
        if (yielded === localSessionYielded) return;
        localSessionYielded = yielded;
        invoke('set_session_yielded', { yielded: yielded }).catch(function () {});
        if (!yielded) {
            const modal = document.getElementById('gm-takeover-modal');
            if (modal) modal.remove();
        }
    }

    window.__GM_SHOW_TAKEOVER_PROMPT = function () {
        if (document.getElementById('gm-takeover-modal')) return;
        const useHereBtn = findUseHereButton();
        if (!useHereBtn) return;

        const overlay = document.createElement('div');
        overlay.id = 'gm-takeover-modal';
        overlay.style.cssText =
            'position:fixed;inset:0;z-index:2147483647;background:rgba(0,0,0,.55);' +
            'display:flex;align-items:center;justify-content:center;font-family:inherit;';

        const card = document.createElement('div');
        card.style.cssText =
            'background:#1e1f20;color:#e3e3e3;border:1px solid #3c4043;border-radius:12px;' +
            'max-width:380px;width:90%;padding:20px 22px;box-shadow:0 12px 32px rgba(0,0,0,.45);';

        const title = document.createElement('div');
        title.style.cssText = 'font-size:16px;font-weight:600;margin-bottom:8px;';
        title.textContent = 'Move Google Messages here?';

        const desc = document.createElement('div');
        desc.style.cssText = 'font-size:13px;line-height:1.45;color:#9aa0a6;margin-bottom:18px;';
        desc.textContent =
            'Google Messages is open on another computer. Move the active session to this app, or choose No to let this app sleep in the background.';

        const actions = document.createElement('div');
        actions.style.cssText = 'display:flex;justify-content:flex-end;gap:10px;';

        const noBtn = document.createElement('button');
        noBtn.type = 'button';
        noBtn.textContent = 'No, let app sleep';
        noBtn.style.cssText =
            'padding:8px 14px;border-radius:18px;border:1px solid #5f6368;background:transparent;' +
            'color:#e3e3e3;font:inherit;font-size:13px;cursor:pointer;';

        const yesBtn = document.createElement('button');
        yesBtn.type = 'button';
        yesBtn.textContent = 'Move session here';
        yesBtn.style.cssText =
            'padding:8px 16px;border-radius:18px;border:none;background:#8ab4f8;' +
            'color:#131314;font:inherit;font-size:13px;font-weight:600;cursor:pointer;';

        noBtn.addEventListener('click', function () {
            overlay.remove();
            syncSessionYielded(true);
        });
        yesBtn.addEventListener('click', function () {
            overlay.remove();
            const btn = findUseHereButton();
            if (btn) btn.click();
            syncSessionYielded(false);
        });
        overlay.addEventListener('click', function (e) {
            if (e.target === overlay) {
                overlay.remove();
                syncSessionYielded(true);
            }
        });

        actions.appendChild(noBtn);
        actions.appendChild(yesBtn);
        card.appendChild(title);
        card.appendChild(desc);
        card.appendChild(actions);
        overlay.appendChild(card);
        document.body.appendChild(overlay);
    };

    function checkSessionState() {
        if (location.host !== 'messages.google.com') return;
        const other = detectOtherSession();
        if (other) {
            syncSessionYielded(true);
        } else if (localSessionYielded) {
            const hasMainUi = !!document.querySelector('mws-conversations-list, mw-main-nav, [data-e2e-conversation-list]');
            if (hasMainUi) {
                syncSessionYielded(false);
            }
        }
    }

    function observeSessionState() {
        checkSessionState();
        const obs = new MutationObserver(checkSessionState);
        if (document.body) {
            obs.observe(document.body, { childList: true, subtree: true });
        }
    }

    // --- Background Heartbeat & Connection Watchdog ---
    function isUserBusy() {
        if (location.pathname.indexOf('/calls') !== -1) return true;
        if (document.querySelector('button[aria-label*="End call" i], button[aria-label*="Leave call" i], [data-e2e-end-call-button]')) {
            return true;
        }
        const active = document.activeElement;
        if (active && (active.tagName === 'TEXTAREA' || active.tagName === 'INPUT' || active.isContentEditable)) {
            const val = (active.value || active.textContent || '').trim();
            if (val.length > 0) return true;
        }
        const compose = document.querySelector('mw-message-compose textarea, [data-e2e-message-input-box], textarea[placeholder*="message" i]');
        if (compose && ((compose.value || compose.textContent || '').trim().length > 0)) {
            return true;
        }
        return false;
    }

    function detectConnectionProblem() {
        if (navigator.onLine === false) return 'offline';
        const banners = document.querySelectorAll(
            'mws-connection-banner, mw-connection-banner, [data-e2e-connection-error], [role="alert"], [role="status"]'
        );
        for (let i = 0; i < banners.length; i++) {
            const el = banners[i];
            if (!isElementVisible(el)) continue;
            if (el.closest && el.closest('mws-message-wrapper, mws-conversation-list-item')) continue;
            const txt = (el.textContent || '').trim();
            if (txt.length > 0 && txt.length < 240 &&
                /trying to reach your phone|unable to reach your phone|couldn['’]t connect|no internet|waiting for network|reconnecting/i.test(txt)) {
                return 'stalled-connection';
            }
        }
        return null;
    }

    window.__gmHeartbeat = function (wokeFromSleep) {
        if (location.host !== 'messages.google.com') {
            invoke('report_heartbeat', { healthy: true, reason: 'non-messages-host' }).catch(function () {});
            return;
        }
        checkSessionState();
        if (localSessionYielded) {
            invoke('report_heartbeat', { healthy: true, reason: 'other-session' }).catch(function () {});
            return;
        }
        syncBadge();
        if (isUserBusy()) {
            invoke('report_heartbeat', { healthy: true, reason: 'user-busy' }).catch(function () {});
            return;
        }
        const problem = detectConnectionProblem();
        if (problem) {
            invoke('report_heartbeat', { healthy: false, reason: problem }).catch(function () {});
        } else if (wokeFromSleep) {
            // Prod a lightweight fetch to verify the session socket/network after sleep
            fetch(location.origin + '/web/', { method: 'HEAD', cache: 'no-store' })
                .then(function () {
                    invoke('report_heartbeat', { healthy: true, reason: null }).catch(function () {});
                })
                .catch(function () {
                    invoke('report_heartbeat', { healthy: false, reason: 'wake-fetch-failed' }).catch(function () {});
                });
        } else {
            invoke('report_heartbeat', { healthy: true, reason: null }).catch(function () {});
        }
    };

    window.addEventListener('online', function () {
        if (location.host !== 'messages.google.com') return;
        setTimeout(function () {
            if (window.__GM_BACKGROUND_KEEP_ALIVE === false || localSessionYielded || isUserBusy()) return;
            if (detectConnectionProblem() === 'stalled-connection') {
                window.location.reload();
            }
        }, 6000);
    });
})();
"#;
