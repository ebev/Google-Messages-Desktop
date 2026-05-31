use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, WebviewUrl, WebviewWindowBuilder,
};

const MESSAGES_URL: &str = "https://messages.google.com/web/authentication";

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
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // Build the main window pointing at Google Messages
            let mut builder =
                WebviewWindowBuilder::new(app, "main", WebviewUrl::External(MESSAGES_URL.parse()?))
                    .title("Google Messages")
                    .inner_size(1100.0, 780.0)
                    .min_inner_size(400.0, 500.0)
                    .center()
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

            build_tray(app)?;
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
        .invoke_handler(tauri::generate_handler![set_tray_badge])
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

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().unwrap().clone())
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

    // --- Keep Messages / Calls / Voicemail in the same window ---
    // The web app links between these sections with target="_blank", which the
    // embedded webview drops (nothing happens). Intercept those and navigate in
    // place instead, so they behave like tabs in one window.
    function isMessagesUrl(url) {
        try {
            return new URL(url, location.href).host === 'messages.google.com';
        } catch (e) {
            return false;
        }
    }
    // Capture phase so we beat the app's own click handlers.
    document.addEventListener('click', function (e) {
        const a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
        if (a && a.target === '_blank' && isMessagesUrl(a.href)) {
            e.preventDefault();
            e.stopPropagation();
            window.location.assign(new URL(a.href, location.href).href);
        }
    }, true);
    const origOpen = window.open;
    window.open = function (url) {
        if (url && isMessagesUrl(url)) {
            window.location.assign(new URL(url, location.href).href);
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
