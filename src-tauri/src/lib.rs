use serde::{Deserialize, Serialize};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WebviewUrl, WebviewWindowBuilder,
};
use tauri_plugin_autostart::ManagerExt;
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
            // Notifications on by default — also the fallback for settings.json
            // files written before this field existed (via #[serde(default)]).
            show_notifications: true,
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

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct WindowSize {
    width: f64,
    height: f64,
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
    let theme = parse_theme(settings.theme.as_deref());
    for window in app.webview_windows().values() {
        let _ = window.set_theme(theme);
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.eval(&format!(
            "window.__GM_SHOW_NOTIFICATIONS = {};",
            settings.show_notifications
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
        let _window = WebviewWindowBuilder::new(
            app,
            "settings",
            WebviewUrl::App("settings.html".into()),
        )
        .title("Settings")
        .inner_size(480.0, 540.0)
        .min_inner_size(360.0, 420.0)
        .theme(parse_theme(settings.theme.as_deref()))
        .center()
        .build()?;
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
            let mut settings = load_settings(app.handle());
            // Keep the persisted launch-at-login flag in sync with the OS/plugin.
            settings.launch_at_login = app.autolaunch().is_enabled().unwrap_or(false);
            save_settings(app.handle(), &settings);

            // Build the main window pointing at Google Messages. If the user
            // chose "Start in Tray", build it hidden.
            let start_url = settings
                .start_url
                .as_deref()
                .unwrap_or(DEFAULT_START_URL);
            // Restore the remembered window size, clamped to the window minimum
            // in case the persisted file was edited or corrupted.
            let (width, height) = settings
                .main_window_size
                .as_ref()
                .map(|s| (s.width.max(400.0), s.height.max(500.0)))
                .unwrap_or((1100.0, 780.0));
            let init_script = INIT_SCRIPT.replace(
                "__GM_NOTIFICATIONS_DEFAULT__",
                if settings.show_notifications { "true" } else { "false" },
            );
            let mut builder =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::External(start_url.parse()?))
                    .title("Google Messages")
                    .inner_size(width, height)
                    .min_inner_size(400.0, 500.0)
                    .theme(parse_theme(settings.theme.as_deref()))
                    .center()
                    .visible(!settings.start_in_tray)
                    .initialization_script(&init_script);
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

            build_tray(app)?;
            Ok(())
        })
        // Close the main window to tray instead of quitting; other windows
        // (Settings, Calls, Voicemail) close normally.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    save_main_window_size(window);
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            set_tray_badge,
            open_external,
            get_settings,
            set_settings,
            open_settings
        ])
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

// Persists the main window's current logical size so it can be restored on
// the next launch. Called when the window closes to tray and on quit.
fn save_main_window_size(window: &tauri::Window) {
    if let (Ok(size), Ok(scale)) = (window.inner_size(), window.scale_factor()) {
        let logical = size.to_logical::<f64>(scale);
        let app = window.app_handle();
        let mut settings = load_settings(app);
        settings.main_window_size = Some(WindowSize {
            width: logical.width,
            height: logical.height,
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
    #[cfg(target_os = "macos")]
    let tray_icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon.png"))?;
    #[cfg(not(target_os = "macos"))]
    let tray_icon =
        tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon-color.png"))?;

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
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
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
    #[cfg(debug_assertions)]
    eprintln!("[ipc] set_tray_badge({count})");
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
//  - Adds a floating gear button that opens the app settings window
//  - Routes target="_blank" links (in-window for Messages, OS browser otherwise)
//  - Wraps the Notification API to honor the "Show notifications" setting
// The __GM_NOTIFICATIONS_DEFAULT__ token is replaced with the persisted
// setting before injection.
const INIT_SCRIPT: &str = r#"
(function () {
    function invoke(cmd, args) {
        if (window.__TAURI_INTERNALS__) {
            return window.__TAURI_INTERNALS__.invoke(cmd, args);
        }
        return Promise.reject(new Error('Tauri IPC unavailable'));
    }

    // --- Unread badge via title observation ---
    function parseUnread(title) {
        const m = title.match(/\((\d+)\)/);
        return m ? parseInt(m[1], 10) : 0;
    }

    function syncBadge() {
        const count = parseUnread(document.title);
        invoke('set_tray_badge', { count }).catch(function () {});
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
        'width:28px;height:28px;padding:0;border:none;border-radius:50%;' +
        'background:rgba(128,128,128,.25);color:inherit;font-size:16px;' +
        'line-height:28px;text-align:center;cursor:pointer;opacity:.6;' +
        'transition:opacity .15s ease;';
    function makeGear() {
        const btn = document.createElement('button');
        btn.id = 'gm-app-settings';
        btn.type = 'button';
        btn.title = 'App settings';
        btn.textContent = '⚙';
        btn.addEventListener('mouseenter', function () { btn.style.opacity = '1'; });
        btn.addEventListener('mouseleave', function () { btn.style.opacity = '.6'; });
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
                'display:inline-block;vertical-align:middle;align-self:center;margin:0 8px;';
            avatar.parentElement.insertBefore(btn, avatar);
            btn.dataset.anchored = '1';
        } else if (!btn.isConnected) {
            btn.style.cssText = GEAR_BASE_CSS +
                'position:fixed;top:16px;right:64px;z-index:2147483647;';
            btn.dataset.anchored = '0';
            document.body.appendChild(btn);
        }
    }
    // Cheap when already anchored (three property checks), so observing the
    // whole body is fine — it keeps the gear alive across SPA re-renders.
    const gearObserver = new MutationObserver(placeGear);

    function onReady() {
        observeTitle();
        placeGear();
        gearObserver.observe(document.body, { childList: true, subtree: true });
    }
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', onReady);
    } else {
        onReady();
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
        invoke('open_external', { url: href })
            .catch(function (e) { console.error('[gm] open_external failed:', e); });
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

    // --- Notifications: honor the "Show notifications" app setting ---
    // The flag is seeded from settings.json at injection time and updated live
    // by set_settings. When off, hand back an inert stand-in so the page's own
    // notification code keeps working without anything being shown.
    window.__GM_SHOW_NOTIFICATIONS = __GM_NOTIFICATIONS_DEFAULT__;
    const OrigNotification = window.Notification;
    function GMNotification(title, options) {
        if (window.__GM_SHOW_NOTIFICATIONS === false) {
            return {
                close: function () {},
                addEventListener: function () {},
                removeEventListener: function () {},
                dispatchEvent: function () { return false; }
            };
        }
        return new OrigNotification(title, options);
    }
    GMNotification.requestPermission = OrigNotification.requestPermission.bind(OrigNotification);
    Object.defineProperty(GMNotification, 'permission', {
        get: function () { return OrigNotification.permission; }
    });
    window.Notification = GMNotification;
})();
"#;
