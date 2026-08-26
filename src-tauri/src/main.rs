// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod smtc;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use smtc::{MediaState, Smtc};
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};
use tauri_plugin_autostart::ManagerExt;

/// Set before the app quits for real (tray "Quit"); gates the
/// hide-on-close behaviour.
static IS_QUITTING: AtomicBool = AtomicBool::new(false);

/// AppHandle clone for the SMTC event-thread -> webview forwarding.
static APP_HANDLE: Mutex<Option<tauri::AppHandle>> = Mutex::new(None);

const TARGET_URL: &str = "https://music.youtube.com";

const ALLOWED_HOSTS: &[&str] = &[
    "music.youtube.com",
    "youtube.com",
    "www.youtube.com",
    "accounts.google.com",
    "google.com",
    "googleusercontent.com",
    "gstatic.com",
    "googleapis.com",
    "ytimg.com",
];

/// wry REPLACES its default browser args when this is set, so the
/// msWebOOUI/msPdfOOUI/msSmartScreenProtection disables are re-appended.
/// anti-throttling flags keep audio + the bridge alive while hidden;
/// autoplay-policy restores playback after app restart.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,OverlayScrollbarFlashAfterAnyScrollUpdate --disable-background-timer-throttling --disable-renderer-backgrounding --autoplay-policy=no-user-gesture-required";

/// Runs on every page load. Kept minimal: webdriver flag, window.open
/// same-tab redirect, _blank-link rewrite, YTM transport-click helpers.
const INIT_SCRIPT: &str = r#"
Object.defineProperty(navigator, 'webdriver', { get: () => false });
window.open = function open(url, _name, _features) {
    if (url) { window.location.assign(url); }
    return { close: function(){}, focus: function(){} };
};
document.addEventListener('click', function (e) {
    var a = e.target.closest('a');
    if (a && a.getAttribute('target') === '_blank') {
        a.setAttribute('target', '_self');
        a.removeAttribute('target');
    }
}, true);
window.__ytmSeek = function (secs) {
    var v = document.querySelector('video');
    if (v && v.duration && isFinite(secs) && secs >= 0) {
        v.currentTime = Math.min(secs, v.duration);
    }
};
window.clickTransport = function (action) {
    function btn(sel) { return document.querySelector(sel); }
    function isPaused(sel) {
        var el = btn(sel);
        if (!el) return false;
        var label = (el.getAttribute('aria-label') || '').toLowerCase();
        return label.indexOf('play') !== -1;
    }
    var b = btn('#play-pause-button');
    if (!b && action === 'next') b = btn('.next-button');
    if (!b && action === 'previous') b = btn('.previous-button');
    if (action === 'play'   && b && isPaused('#play-pause-button')) b.click();
    if (action === 'pause'  && b && !isPaused('#play-pause-button')) b.click();
    if (action === 'toggle' && b) b.click();
    if (action === 'next'     && btn('.next-button'))     btn('.next-button').click();
    if (action === 'previous' && btn('.previous-button')) btn('.previous-button').click();
};
"#;

/// Injected playback watcher (see bridge.js for selector centralization).
const BRIDGE_JS: &str = include_str!("bridge.js");

fn is_allowed_host(hostname: &str) -> bool {
    ALLOWED_HOSTS
        .iter()
        .any(|suffix| hostname == *suffix || hostname.ends_with(&format!(".{suffix}")))
}

fn is_allowed_url(url: &url::Url) -> bool {
    match url.scheme() {
        "http" | "https" => url.host_str().is_some_and(is_allowed_host),
        "about" => url.path() == "blank",
        // blob:https://music.youtube.com/<uuid> — allow only when the inner
        // host is on the allowlist.
        "blob" => url
            .path()
            .split_once(':')
            .and_then(|(scheme, rest)| match scheme {
                "http" | "https" => url::Url::parse(&format!("{scheme}:{rest}")).ok(),
                _ => None,
            })
            .is_some_and(|inner| inner.host_str().is_some_and(is_allowed_host)),
        _ => false,
    }
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn toggle_mini(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("mini") {
        if win.is_visible().unwrap_or(false) {
            let _ = win.hide();
        } else {
            let _ = win.show();
            let _ = win.set_focus();
        }
    }
}

/// Broadcast current media state to the mini-player.
#[tauri::command]
fn media_update(
    app: tauri::AppHandle,
    smtc: tauri::State<'_, Smtc>,
    state: MediaState,
) -> Result<(), String> {
    if smtc.update(&state) {
        let _ = app.emit("media-state", &state);
    }
    Ok(())
}

/// Transport action from SMTC events or the mini-player buttons.
/// `position` (seconds, optional) is for seek actions.
#[tauri::command]
fn transport(app: tauri::AppHandle, action: String, position: Option<u64>) -> Result<(), String> {
    let main = app
        .get_webview_window("main")
        .ok_or("main window missing")?;
    let js = match action.as_str() {
        "play" | "pause" | "toggle" | "next" | "previous" => {
            format!("window.clickTransport && window.clickTransport('{action}');")
        }
        "seek" => {
            let secs = position.unwrap_or(0);
            if secs == 0 {
                return Err("seek without position".into());
            }
            format!("window.__ytmSeek && window.__ytmSeek({secs});")
        }
        other => return Err(format!("unknown transport action: {other}")),
    };
    main.eval(&js).map_err(|e| e.to_string())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .manage(Smtc::default())
        .invoke_handler(tauri::generate_handler![media_update, transport])
        .setup(|app| {
            *APP_HANDLE.lock().unwrap() = Some(app.handle().clone());

            let target: url::Url = TARGET_URL.parse().unwrap();
            let main_window = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(target))
                .title("YouTube Music")
                .inner_size(1200.0, 900.0)
                .min_inner_size(400.0, 300.0)
                .initialization_script(INIT_SCRIPT)
                .initialization_script(BRIDGE_JS)
                .additional_browser_args(BROWSER_ARGS)
                .disable_drag_drop_handler()
                .on_navigation(|url| {
                    if is_allowed_url(url) {
                        return true;
                    }
                    // External links (and anything Google redirects we don't
                    // recognise) open in the system browser. Never silently
                    // kill the navigation: forward it.
                    let _ = open::that_detached(url.as_str());
                    false
                })
                .build()?;

            // SMTC is bound to this window's HWND — required on Windows.
            if let Ok(hwnd) = main_window.hwnd() {
                let smtc = app.state::<Smtc>();
                smtc.init(hwnd.0);
            } else {
                eprintln!("[smtc] could not retrieve main window HWND; media keys disabled");
            }

            // Hide to tray on close (real exit only via tray "Quit").
            let win_clone = main_window.clone();
            main_window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    if !IS_QUITTING.load(Ordering::SeqCst) {
                        api.prevent_close();
                        let _ = win_clone.hide();
                    }
                }
            });

            // Start minimized-to-tray when launched at startup.
            if app.autolaunch().is_enabled().unwrap_or(false) {
                let _ = main_window.hide();
            }

            build_tray(app.handle())?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running YouTube Music");
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let autostart_enabled = app.autolaunch().is_enabled().unwrap_or(false);
    let separator = PredefinedMenuItem::separator(app)?;

    let open_item = MenuItem::with_id(app, "open", "Open YouTube Music", true, None::<&str>)?;
    let mini_item = MenuItem::with_id(app, "mini", "Mini player", true, None::<&str>)?;
    let startup_item = CheckMenuItem::with_id(
        app,
        "startup",
        "Launch at startup",
        true,
        autostart_enabled,
        None::<&str>,
    )?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &open_item,
            &mini_item,
            &separator,
            &startup_item,
            &separator,
            &quit_item,
        ],
    )?;

    let _tray = TrayIconBuilder::new()
        .icon(app.default_window_icon().cloned().unwrap())
        .tooltip("YouTube Music")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app_handle, event| match event.id().as_ref() {
            "open" => show_main(app_handle),
            "mini" => toggle_mini(app_handle),
            "startup" => {
                let manager = app_handle.autolaunch();
                if manager.is_enabled().unwrap_or(false) {
                    let _ = manager.disable();
                } else {
                    let _ = manager.enable();
                }
            }
            "quit" => {
                IS_QUITTING.store(true, Ordering::SeqCst);
                app_handle.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_allowed_host, is_allowed_url};

    #[test]
    fn allows_ytm_and_login_hosts() {
        assert!(is_allowed_host("music.youtube.com"));
        assert!(is_allowed_host("www.youtube.com"));
        assert!(is_allowed_host("youtube.com"));
        assert!(is_allowed_host("accounts.google.com"));
        assert!(is_allowed_host("lh3.googleusercontent.com"));
        assert!(is_allowed_host("i.ytimg.com"));
        assert!(is_allowed_host("any.sub.google.com"));
    }

    #[test]
    fn rejects_unknown_hosts() {
        assert!(!is_allowed_host("example.com"));
        assert!(!is_allowed_host("chatgpt.com.example.com"));
        assert!(!is_allowed_host("youtube.com.example.com"));
        assert!(!is_allowed_host("notyoutube.com"));
    }

    #[test]
    fn url_allowlist_edge_cases() {
        let blank: url::Url = "about:blank".parse().unwrap();
        let blob: url::Url = "blob:https://music.youtube.com/abc-123".parse().unwrap();
        let blob_bad: url::Url = "blob:https://evil.com/abc-123".parse().unwrap();
        let app = "https://music.youtube.com/library/songs"
            .parse::<url::Url>()
            .unwrap();
        let blocked = "https://example.com/login".parse::<url::Url>().unwrap();

        assert!(is_allowed_url(&blank));
        assert!(is_allowed_url(&blob));
        assert!(!is_allowed_url(&blob_bad));
        assert!(is_allowed_url(&app));
        assert!(!is_allowed_url(&blocked));
    }
}
