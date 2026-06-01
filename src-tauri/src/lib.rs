use serde::{Deserialize, Serialize};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_autostart::ManagerExt;

// Canonical web entry point. When already paired this lands straight on the
// (cached) conversation list; when not paired it redirects to the QR pairing
// page on its own. Loading the pairing sub-route directly added an extra hop.
const MESSAGES_URL: &str = "https://messages.google.com/web/";

// User-facing settings, persisted to settings.json in the app config dir.
// (Launch-at-login is tracked by the autostart plugin's own OS state, so it is
// not duplicated here.)
#[derive(Serialize, Deserialize, Clone, Default)]
struct Settings {
    /// When true, the app launches with the window hidden (tray only).
    start_in_tray: bool,
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
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let settings = load_settings(app.handle());

            // Build the main window pointing at Google Messages. If the user
            // chose "Start in Tray", build it hidden.
            let mut builder =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::External(MESSAGES_URL.parse()?))
                    .title("Google Messages")
                    .inner_size(1100.0, 780.0)
                    .min_inner_size(400.0, 500.0)
                    .center()
                    .visible(!settings.start_in_tray)
                    .initialization_script(INIT_SCRIPT);
            if let Some(ua) = USER_AGENT {
                builder = builder.user_agent(ua);
            }
            #[allow(unused_variables)]
            let window = builder.build()?;

            // On Windows, auto-grant mic/camera so calls don't prompt every
            // session. macOS is handled by wry (auto-grant) + Info.plist; Linux
            // is tracked separately in TODO.md.
            #[cfg(windows)]
            grant_media_permissions(&window);

            build_tray(app, &settings)?;
            Ok(())
        })
        // Close the main window to tray instead of quitting; other windows
        // (Calls / Voicemail) close normally.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![set_tray_badge, open_external])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// Auto-allow microphone & camera permission requests in the WebView2 webview so
// the calls app works without prompting the user every session. Mirrors the
// pattern wry uses internally for clipboard. WebView2's default would otherwise
// show a prompt each time. (The OS-level "let desktop apps use the microphone"
// privacy toggle must still be enabled by the user.)
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

fn build_tray(app: &tauri::App, settings: &Settings) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    // Settings as checkable items. Initial check state reflects current state:
    // autostart from the OS (via the plugin), start-in-tray from settings.json.
    let autostart_on = app.autolaunch().is_enabled().unwrap_or(false);
    let launch_at_login = CheckMenuItem::with_id(
        app,
        "autostart",
        "Launch at Login",
        true,
        autostart_on,
        None::<&str>,
    )?;
    let start_in_tray = CheckMenuItem::with_id(
        app,
        "start_in_tray",
        "Start in Tray",
        true,
        settings.start_in_tray,
        None::<&str>,
    )?;

    let menu = Menu::with_items(
        app,
        &[
            &show,
            &PredefinedMenuItem::separator(app)?,
            &launch_at_login,
            &start_in_tray,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    // Dedicated transparent bubble glyph for the menu bar. Using the full app
    // icon here would render as a solid square under `icon_as_template`, since
    // template mode only uses the alpha channel.
    let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png"))?;

    TrayIconBuilder::with_id("main")
        .icon(tray_icon)
        .icon_as_template(true) // macOS: respects light/dark mode automatically
        .tooltip("Google Messages")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "quit" => app.exit(0),
            "autostart" => {
                // The check item has already toggled its own visual state;
                // mirror it to the OS autostart entry.
                let mgr = app.autolaunch();
                if mgr.is_enabled().unwrap_or(false) {
                    let _ = mgr.disable();
                } else {
                    let _ = mgr.enable();
                }
            }
            "start_in_tray" => {
                let mut s = load_settings(app);
                s.start_in_tray = !s.start_in_tray;
                save_settings(app, &s);
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
                        let _ = window.hide();
                    } else {
                        let _ = window.show();
                        let _ = window.set_focus();
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
        let _ = app.opener().open_url(url, None::<&str>);
    }
}

// Called from JS when the unread count changes
#[tauri::command]
fn set_tray_badge(app: tauri::AppHandle, count: u32) {
    if let Some(tray) = app.tray_by_id("main") {
        if count > 0 {
            let label = if count > 99 {
                "99+".to_string()
            } else {
                count.to_string()
            };
            // Badge tooltip so the count is visible on hover
            let _ = tray.set_tooltip(Some(&format!("Google Messages ({label} unread)")));
        } else {
            let _ = tray.set_tooltip(Some("Google Messages"));
        }
    }
}

// Injected into the webview on load:
//  - Observes the page title for unread count changes and calls set_tray_badge
//  - Intercepts the Notification API so system notifications still fire when
//    the window is hidden
const INIT_SCRIPT: &str = r#"
(function () {
    // --- Unread badge via title observation ---
    function parseUnread(title) {
        const m = title.match(/\((\d+)\)/);
        return m ? parseInt(m[1], 10) : 0;
    }

    function syncBadge() {
        const count = parseUnread(document.title);
        if (window.__tauriInvokeQueue === undefined) {
            window.__tauriInvokeQueue = [];
        }
        if (window.__TAURI_INTERNALS__) {
            window.__TAURI_INTERNALS__.invoke('set_tray_badge', { count });
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
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', observeTitle);
    } else {
        observeTitle();
    }

    // --- Route target="_blank" / window.open links ---
    // The web app opens links with target="_blank", which the embedded webview
    // otherwise drops (nothing happens). Handle them ourselves:
    //   - messages.google.com (Messages / Calls / Voicemail) -> navigate in the
    //     same window, so they behave like tabs.
    //   - any other site (e.g. fi.google.com/account) -> open in the OS default
    //     browser via the open_external command.
    function parseUrl(url) {
        try {
            return new URL(url, location.href);
        } catch (e) {
            return null;
        }
    }
    function openExternal(href) {
        if (window.__TAURI_INTERNALS__) {
            window.__TAURI_INTERNALS__.invoke('open_external', { url: href });
        }
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
    // Capture phase so we beat the app's own click handlers.
    document.addEventListener('click', function (e) {
        const a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
        if (a && a.target === '_blank') {
            const u = parseUrl(a.href);
            if (u && route(u)) {
                e.preventDefault();
                e.stopPropagation();
            }
        }
    }, true);
    const origOpen = window.open;
    window.open = function (url) {
        const u = url ? parseUrl(url) : null;
        if (u && route(u)) {
            return null;
        }
        return origOpen.apply(window, arguments);
    };

    // --- Notifications: allow them even when window is hidden ---
    const OrigNotification = window.Notification;
    class PatchedNotification extends OrigNotification {
        constructor(title, options) {
            super(title, options);
        }
        static get permission() { return OrigNotification.permission; }
        static requestPermission(cb) { return OrigNotification.requestPermission(cb); }
    }
    window.Notification = PatchedNotification;
})();
"#;
