// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod lifecycle;
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
static LAST_TRACK: Mutex<String> = Mutex::new(String::new());

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
///
/// Everything else here exists to keep audio playing when the window is not
/// in front. Chromium has four independent ways to deprioritise a window
/// nobody is looking at, and each one has cost this app playback:
///
/// * `disable-background-timer-throttling` — clamps page timers to ~1/s.
///   YouTube Music refills its MediaSource buffer from JS, so a starved
///   timer means an empty buffer and a stall.
/// * `disable-renderer-backgrounding` — drops the renderer to background
///   process priority when the window is hidden or minimised.
/// * `disable-backgrounding-occluded-windows` — treats a window that is
///   merely *covered* as hidden, which is the "not on the forefront" case.
/// * `CalculateNativeWinOcclusion` — the Windows occlusion tracker behind
///   that decision. Disabled so a covered window is never marked hidden.
/// * `IntensiveWakeUpThrottling` — a second, separate clamp that kicks in
///   after five minutes hidden and is *not* covered by the timer switch.
///
/// `autoplay-policy` restores playback after an app restart.
const BROWSER_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,OverlayScrollbarFlashAfterAnyScrollUpdate,CalculateNativeWinOcclusion,IntensiveWakeUpThrottling --disable-background-timer-throttling --disable-renderer-backgrounding --disable-backgrounding-occluded-windows --autoplay-policy=no-user-gesture-required";

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
    if (window.__ytmNoteIntent) { window.__ytmNoteIntent(); }
    var v = document.querySelector('video');
    if (v && v.duration && isFinite(secs) && secs >= 0) {
        v.currentTime = Math.min(secs, v.duration);
    }
};
window.clickTransport = function (action) {
    // Media keys and the mini-player drive playback from outside the page;
    // the keep-alive watchdog must not undo a pause the user asked for.
    if (window.__ytmNoteIntent) { window.__ytmNoteIntent(); }
    var video = document.querySelector('video');
    if (action === 'next' || action === 'previous') {
        document.querySelector(action === 'next' ? '.next-button' : '.previous-button')?.click();
    } else if (video) {
        if (action === 'play' || (action === 'toggle' && video.paused)) video.play().catch(console.debug);
        else if (action === 'pause' || action === 'toggle') video.pause();
    }
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

pub(crate) fn build_main_window(
    app_handle: &tauri::AppHandle,
) -> tauri::Result<tauri::WebviewWindow> {
    let target: url::Url = TARGET_URL.parse().unwrap();
    let window = WebviewWindowBuilder::new(app_handle, "main", WebviewUrl::External(target))
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
            if matches!(url.scheme(), "https" | "http" | "mailto") {
                let _ = open::that_detached(url.as_str());
            }
            false
        })
        .build()?;

    // ponytail: hide-to-tray lives with construction so rebuilt windows keep it
    let hide_clone = window.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::CloseRequested { api, .. } = event {
            if !IS_QUITTING.load(Ordering::SeqCst) {
                api.prevent_close();
                let _ = hide_clone.hide();
            }
        }
    });
    Ok(window)
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        lifecycle::restore_window(window);
        return;
    }
    // ponytail: tray-only trap — main window gone, rebuild it
    if let Ok(window) = build_main_window(app) {
        lifecycle::restore_window(window);
    }
}

fn toggle_mini(app: &tauri::AppHandle) {
    if let Some(win) = app.get_webview_window("mini") {
        if win.is_visible().unwrap_or(false) {
            let _ = win.hide();
        } else {
            lifecycle::restore_window(win);
        }
    }
}

/// Reload the page. The escape hatch when YouTube Music wedges itself: the
/// window keeps its position and the session survives in the WebView2 profile.
fn reload_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.eval("location.reload()");
    }
}

/// Broadcast current media state to the mini-player.
#[tauri::command]
fn media_update(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    smtc: tauri::State<'_, Smtc>,
    state: MediaState,
) -> Result<(), String> {
    let url = window.url().map_err(|e| e.to_string())?;
    if window.label() != "main"
        || url.scheme() != "https"
        || url.host_str() != Some("music.youtube.com")
    {
        return Err("Media origin rejected".into());
    }
    if state.title.as_ref().is_some_and(|s| s.len() > 4096)
        || state.artist.as_ref().is_some_and(|s| s.len() > 4096)
    {
        return Err("Media metadata too large".into());
    }
    let track = format!(
        "{} — {}",
        state.title.as_deref().unwrap_or(""),
        state.artist.as_deref().unwrap_or("")
    );
    let mut last = LAST_TRACK.lock().map_err(|e| e.to_string())?;
    if state.playing && state.title.as_ref().is_some_and(|s| !s.is_empty()) && *last != track {
        if !last.is_empty()
            && (!window.is_focused().unwrap_or(false) || !window.is_visible().unwrap_or(false))
        {
            // WinRT toast activation can take a hundred milliseconds. Keep it
            // off the IPC thread so a track change cannot stall the page.
            let handle = app.clone();
            let identifier = app.config().identifier.clone();
            let text: String = track.chars().take(300).collect();
            std::thread::spawn(move || {
                tauri_winrt_notification::Toast::new(&identifier)
                    .title("YouTube Music")
                    .text1(&text)
                    .on_activated(move |_| {
                        show_main(&handle);
                        Ok(())
                    })
                    .show()
                    .unwrap_or_else(|error| eprintln!("Track notification failed: {error}"));
            });
        }
        *last = track;
    }
    if smtc.update(&state) {
        // Only the mini-player listens. Emitting to every window would run a
        // JS eval inside the YouTube Music page twice a second.
        let _ = app.emit_to("mini", "media-state", &state);
    }
    Ok(())
}

/// Transport action from SMTC events or the mini-player buttons.
/// `position` (seconds, optional) is for seek actions.
#[tauri::command]
fn transport(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    action: String,
    position: Option<u64>,
) -> Result<(), String> {
    if window.label() != "mini" {
        return Err("Transport requires the local mini-player".into());
    }
    let main = app
        .get_webview_window("main")
        .ok_or("main window missing")?;
    let js = match action.as_str() {
        "play" | "pause" | "toggle" | "next" | "previous" => {
            format!("window.clickTransport && window.clickTransport('{action}');")
        }
        "seek" => {
            let secs = position.ok_or("seek without position")?;
            format!("window.__ytmSeek && window.__ytmSeek({secs});")
        }
        other => return Err(format!("unknown transport action: {other}")),
    };
    main.eval(&js).map_err(|e| e.to_string())
}

#[tauri::command]
fn media_snapshot(
    window: tauri::WebviewWindow,
    smtc: tauri::State<'_, Smtc>,
) -> Result<MediaState, String> {
    if window.label() != "mini" {
        return Err("Snapshot requires mini-player".into());
    }
    Ok(smtc.snapshot())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .manage(Smtc::default())
        .invoke_handler(tauri::generate_handler![
            media_update,
            transport,
            media_snapshot
        ])
        .setup(|app| {
            if let Some(mini) = app.get_webview_window("mini") {
                let handle = mini.clone();
                mini.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = handle.hide();
                    }
                });
            }
            *APP_HANDLE.lock().unwrap() = Some(app.handle().clone());

            // Clear WebView2 processes a killed run left behind before this
            // process builds its own, so a stale tree cannot fight this launch
            // over the same EBWebView profile.
            lifecycle::reap_orphans(app.handle());
            lifecycle::start_watchdog(app.handle().clone());
            lifecycle::start_power_guard(app.handle().clone());

            let main_window = build_main_window(app.handle())?;

            // SMTC is bound to this window's HWND — required on Windows.
            if let Ok(hwnd) = main_window.hwnd() {
                let smtc = app.state::<Smtc>();
                smtc.init(hwnd.0);
            } else {
                eprintln!("[smtc] could not retrieve main window HWND; media keys disabled");
            }

            // Start minimized-to-tray when launched at startup.
            if std::env::args().any(|arg| arg == "--autostart") {
                let _ = main_window.hide();
            } else {
                // ponytail: guarantee visible window on manual launch
                let _ = main_window.show();
                let _ = main_window.unminimize();
                let _ = main_window.set_focus();
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
    let reload_item = MenuItem::with_id(app, "reload", "Reload page", true, None::<&str>)?;
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
            &reload_item,
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
            "reload" => reload_main(app_handle),
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
    use super::{is_allowed_host, is_allowed_url, BROWSER_ARGS};

    /// The mini window (tauri.conf.json) and the main window (Rust) share one
    /// WebView2 browser process; WebView2 rejects environment creation with
    /// ERROR_INVALID_STATE (0x8007139F) when their browser args differ, which
    /// kills the main window right after launch.
    #[test]
    fn mini_and_main_browser_args_match() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let mini = conf["app"]["windows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["label"] == "mini")
            .unwrap();
        assert_eq!(
            mini["additionalBrowserArgs"].as_str().unwrap(),
            BROWSER_ARGS,
            "mini window additionalBrowserArgs must equal BROWSER_ARGS in main.rs"
        );
    }

    /// Every switch that stops Chromium from deprioritising a background
    /// window must survive edits to BROWSER_ARGS: losing one is exactly the
    /// "music stutters when the app is not in front" bug.
    #[test]
    fn browser_args_keep_background_playback_alive() {
        for switch in [
            "--disable-background-timer-throttling",
            "--disable-renderer-backgrounding",
            "--disable-backgrounding-occluded-windows",
            "--autoplay-policy=no-user-gesture-required",
        ] {
            assert!(BROWSER_ARGS.contains(switch), "BROWSER_ARGS lost {switch}");
        }
        let features = BROWSER_ARGS
            .split_whitespace()
            .find_map(|arg| arg.strip_prefix("--disable-features="))
            .expect("BROWSER_ARGS must carry a --disable-features list");
        for feature in [
            "msWebOOUI",
            "msPdfOOUI",
            "msSmartScreenProtection",
            "CalculateNativeWinOcclusion",
            "IntensiveWakeUpThrottling",
        ] {
            assert!(
                features.split(',').any(|f| f == feature),
                "BROWSER_ARGS lost the {feature} feature disable"
            );
        }
    }

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
