//! Window lifecycle and background-playback helpers.
//!
//! Two failures the sibling ChatGPT desktop wrapper hit apply here too:
//!
//! * `show()`/`set_focus()` called from the tray handler run on the
//!   event-loop thread, and `ShowWindow`/`SetForegroundWindow` send
//!   synchronous messages to explorer.exe. A wedged shell froze that whole
//!   app. Restore now happens on a worker thread.
//! * A killed run leaves `msedgewebview2.exe` children behind. They fight
//!   the next launch over the same EBWebView profile, which shows up as a
//!   blank or crawling page. They are reaped at startup.
//!
//! It also opts the app and its WebView2 tree out of Windows power
//! throttling (EcoQoS). That is the OS half of "the music stutters when the
//! window is in the background": Windows 11 parks background processes on a
//! low-power core budget, and an audio thread that misses its deadline
//! underruns. Chromium's own backgrounding is handled by the browser flags
//! in `main.rs`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager};

/// Image name of every WebView2 child process (never the Edge browser,
/// which runs as `msedge.exe`).
const WEBVIEW2_EXE: &str = "msedgewebview2.exe";

/// How often the event loop is probed for a hang.
const HANG_PROBE: Duration = Duration::from_secs(10);

/// How often the power-throttling opt-out is re-asserted. Chromium re-applies
/// its own throttling when a renderer is backgrounded, so one shot is not
/// enough; the call is a handful of syscalls.
const POWER_REASSERT: Duration = Duration::from_secs(20);

static EVENT_LOOP_TICKS: AtomicU64 = AtomicU64::new(0);

/// Append one line to `<app-log-dir>/lifecycle.log`. Diagnostics only: every
/// failure here is swallowed so logging can never take the app down.
fn log(app: &AppHandle, msg: &str) {
    let Ok(dir) = app.path().app_log_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("lifecycle.log"))
    else {
        return;
    };
    use std::io::Write;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = writeln!(file, "[{secs}] {msg}");
}

/// Show and focus a window without blocking the event-loop thread.
///
/// The Win32 calls run on a worker because `ShowWindow` and
/// `SetForegroundWindow` send synchronous messages to the shell. Tauri's own
/// calls are made from the same worker so they queue onto the event loop
/// instead of running inline on it.
pub(crate) fn restore_window(window: tauri::WebviewWindow) {
    std::thread::spawn(move || {
        #[cfg(windows)]
        if let Ok(hwnd) = window.hwnd() {
            win::restore(hwnd.0);
        }
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    });
}

/// True when `exe` is a WebView2 child whose parent is no longer running.
/// Pure so the predicate is unit-tested without touching the process table.
fn should_reap(exe: &str, pid: u32, ppid: u32, me: u32, live: &HashSet<u32>) -> bool {
    pid != me && is_webview2_process(exe) && !live.contains(&ppid)
}

pub(crate) fn is_webview2_process(exe_file_name: &str) -> bool {
    exe_file_name.eq_ignore_ascii_case(WEBVIEW2_EXE)
}

/// True when `pid` (whose own parent is `ppid`) descends from `root`
/// through the `child -> parent` map. Pure logic, unit-tested.
fn descends_from(parents: &HashMap<u32, u32>, pid: u32, ppid: u32, root: u32) -> bool {
    if pid == root {
        return true;
    }
    let mut current = ppid;
    for _ in 0..64 {
        if current == root {
            return true;
        }
        match parents.get(&current) {
            Some(&next) if next != current => current = next,
            _ => return false,
        }
    }
    false
}

/// Terminate WebView2 processes left behind by a previous run. Called once at
/// startup, before this process builds its own webview.
pub(crate) fn reap_orphans(app: &AppHandle) {
    #[cfg(windows)]
    {
        let mut reaped = 0usize;
        // Killing a browser process orphans its own children, so repeat until
        // a pass finds nothing new. Bounded so a pathological tree cannot spin.
        for _ in 0..4 {
            let procs = win::snapshot();
            let live: HashSet<u32> = procs.iter().map(|p| p.pid).collect();
            let me = win::current_pid();
            let mut killed = 0usize;
            for entry in &procs {
                if !should_reap(&entry.exe, entry.pid, entry.ppid, me, &live) {
                    continue;
                }
                if win::terminate(entry.pid) {
                    killed += 1;
                }
            }
            reaped += killed;
            if killed == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        if reaped > 0 {
            log(
                app,
                &format!("reaped {reaped} orphaned WebView2 process(es)"),
            );
        }
    }
    #[cfg(not(windows))]
    let _ = app;
}

/// Log when the event-loop thread stops pumping messages. A blocked UI thread
/// cannot be recovered in-process, so the value here is the evidence: the
/// timestamp plus the hidden/shown history tells the next investigation what
/// wedged it.
pub(crate) fn start_watchdog(app: AppHandle) {
    std::thread::spawn(move || {
        let mut strikes = 0u32;
        loop {
            std::thread::sleep(HANG_PROBE);
            let before = EVENT_LOOP_TICKS.load(Ordering::SeqCst);
            if app
                .run_on_main_thread(|| {
                    EVENT_LOOP_TICKS.fetch_add(1, Ordering::SeqCst);
                })
                .is_err()
            {
                log(&app, "event-loop probe could not be queued");
                continue;
            }
            std::thread::sleep(Duration::from_millis(500));
            if EVENT_LOOP_TICKS.load(Ordering::SeqCst) == before {
                strikes += 1;
                log(
                    &app,
                    &format!("event loop did not pump within 500 ms (strike {strikes})"),
                );
            } else {
                strikes = 0;
            }
        }
    });
}

/// Keep the app and its WebView2 tree out of Windows power throttling.
///
/// `PROCESS_POWER_THROTTLING_EXECUTION_SPEED` is what Windows 11's efficiency
/// mode sets on a background process; clearing it is the documented opt-out
/// and is what Chromium itself does for a renderer that is playing audio.
pub(crate) fn start_power_guard(app: AppHandle) {
    std::thread::spawn(move || {
        let mut logged = false;
        loop {
            let pids = webview2_tree_pids();
            let cleared = win::clear_power_throttling(&pids);
            if !logged && cleared > 0 {
                log(
                    &app,
                    &format!("cleared Windows power throttling on {cleared} process(es)"),
                );
                logged = true;
            }
            std::thread::sleep(POWER_REASSERT);
        }
    });
}

#[cfg(windows)]
fn webview2_tree_pids() -> Vec<u32> {
    let procs = win::snapshot();
    let parents: HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.ppid)).collect();
    let root = win::current_pid();
    let mut pids: Vec<u32> = procs
        .iter()
        .filter(|p| is_webview2_process(&p.exe) && descends_from(&parents, p.pid, p.ppid, root))
        .map(|p| p.pid)
        .collect();
    pids.push(root);
    pids
}

#[cfg(not(windows))]
fn webview2_tree_pids() -> Vec<u32> {
    Vec::new()
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    type Dword = u32;
    type Bool = i32;
    type Handle = *mut c_void;

    const SNAP_PROCESS: Dword = 0x0000_0002;
    const INVALID_HANDLE: Handle = -1isize as Handle;
    const PROCESS_TERMINATE: Dword = 0x0001;
    const PROCESS_SET_INFORMATION: Dword = 0x0200;
    const SW_SHOW: i32 = 5;
    const SW_RESTORE: i32 = 9;
    const PROCESS_POWER_THROTTLING_CLASS: i32 = 4;
    const POWER_THROTTLING_VERSION: Dword = 1;
    const POWER_THROTTLING_EXECUTION_SPEED: Dword = 0x1;
    const POWER_THROTTLING_IGNORE_TIMER_RESOLUTION: Dword = 0x4;

    // Field order mirrors PROCESSENTRY32W; repr(C) pads identically and
    // dw_size is set from our own size_of.
    #[allow(dead_code)]
    #[repr(C)]
    struct ProcessEntry {
        dw_size: Dword,
        cnt_usage: Dword,
        pid: Dword,
        heap_id: usize,
        module_id: Dword,
        threads: Dword,
        ppid: Dword,
        pri_base: i32,
        flags: Dword,
        exe: [u16; 260],
    }

    #[repr(C)]
    struct PowerThrottlingState {
        version: Dword,
        control_mask: Dword,
        state_mask: Dword,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: Dword, pid: Dword) -> Handle;
        fn Process32FirstW(snap: Handle, entry: *mut ProcessEntry) -> Bool;
        fn Process32NextW(snap: Handle, entry: *mut ProcessEntry) -> Bool;
        fn CloseHandle(obj: Handle) -> Bool;
        fn OpenProcess(access: Dword, inherit: Bool, pid: Dword) -> Handle;
        fn GetCurrentProcessId() -> Dword;
        fn TerminateProcess(handle: Handle, code: u32) -> Bool;
        fn SetProcessInformation(
            handle: Handle,
            class: i32,
            info: *mut c_void,
            size: Dword,
        ) -> Bool;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn ShowWindow(hwnd: Handle, cmd: i32) -> Bool;
        fn IsIconic(hwnd: Handle) -> Bool;
        fn BringWindowToTop(hwnd: Handle) -> Bool;
        fn SetForegroundWindow(hwnd: Handle) -> Bool;
    }

    pub struct Proc {
        pub pid: u32,
        pub ppid: u32,
        pub exe: String,
    }

    pub fn current_pid() -> u32 {
        unsafe { GetCurrentProcessId() }
    }

    pub fn snapshot() -> Vec<Proc> {
        let mut out = Vec::new();
        unsafe {
            let snap = CreateToolhelp32Snapshot(SNAP_PROCESS, 0);
            if snap.is_null() || snap == INVALID_HANDLE {
                return out;
            }
            let mut entry: ProcessEntry = std::mem::zeroed();
            entry.dw_size = std::mem::size_of::<ProcessEntry>() as Dword;
            if Process32FirstW(snap, &mut entry) != 0 {
                loop {
                    let len = entry
                        .exe
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.exe.len());
                    out.push(Proc {
                        pid: entry.pid,
                        ppid: entry.ppid,
                        exe: String::from_utf16_lossy(&entry.exe[..len]),
                    });
                    if Process32NextW(snap, &mut entry) == 0 {
                        break;
                    }
                }
            }
            CloseHandle(snap);
        }
        out
    }

    pub fn terminate(pid: u32) -> bool {
        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if handle.is_null() {
                return false;
            }
            let ok = TerminateProcess(handle, 1) != 0;
            CloseHandle(handle);
            ok
        }
    }

    pub fn restore(hwnd: Handle) {
        unsafe {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            } else {
                ShowWindow(hwnd, SW_SHOW);
            }
            BringWindowToTop(hwnd);
            SetForegroundWindow(hwnd);
        }
    }

    /// Clear execution-speed throttling on each pid. Returns how many
    /// processes accepted the change.
    pub fn clear_power_throttling(pids: &[u32]) -> usize {
        let mut cleared = 0usize;
        for &pid in pids {
            unsafe {
                let handle = OpenProcess(PROCESS_SET_INFORMATION, 0, pid);
                if handle.is_null() {
                    continue;
                }
                let size = std::mem::size_of::<PowerThrottlingState>() as Dword;
                let mut state = PowerThrottlingState {
                    version: POWER_THROTTLING_VERSION,
                    control_mask: POWER_THROTTLING_EXECUTION_SPEED
                        | POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
                    state_mask: POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
                };
                let mut ok = SetProcessInformation(
                    handle,
                    PROCESS_POWER_THROTTLING_CLASS,
                    &mut state as *mut _ as *mut c_void,
                    size,
                ) != 0;
                if !ok {
                    // Older Windows rejects IGNORE_TIMER_RESOLUTION; retry with
                    // the execution-speed bit alone.
                    state.control_mask = POWER_THROTTLING_EXECUTION_SPEED;
                    state.state_mask = 0;
                    ok = SetProcessInformation(
                        handle,
                        PROCESS_POWER_THROTTLING_CLASS,
                        &mut state as *mut _ as *mut c_void,
                        size,
                    ) != 0;
                }
                CloseHandle(handle);
                if ok {
                    cleared += 1;
                }
            }
        }
        cleared
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(ids: &[u32]) -> HashSet<u32> {
        ids.iter().copied().collect()
    }

    #[test]
    fn reaps_a_webview2_child_whose_parent_is_gone() {
        assert!(should_reap(
            "msedgewebview2.exe",
            42,
            999,
            1,
            &live(&[1, 42])
        ));
    }

    #[test]
    fn spares_a_webview2_child_with_a_live_parent() {
        assert!(!should_reap(
            "msedgewebview2.exe",
            42,
            1,
            1,
            &live(&[1, 42])
        ));
    }

    #[test]
    fn spares_itself_and_foreign_images() {
        assert!(!should_reap("msedgewebview2.exe", 1, 999, 1, &live(&[1])));
        assert!(!should_reap("msedge.exe", 42, 999, 1, &live(&[1, 42])));
        assert!(!should_reap("explorer.exe", 42, 999, 1, &live(&[1, 42])));
    }

    #[test]
    fn webview2_image_match_ignores_case() {
        assert!(is_webview2_process("MSEDGEWEBVIEW2.EXE"));
        assert!(is_webview2_process("msedgewebview2.exe"));
        assert!(!is_webview2_process("msedgewebview2.exe.bak"));
    }

    #[test]
    fn descends_from_walks_the_parent_chain() {
        let mut parents = HashMap::new();
        parents.insert(10u32, 1u32); // 10 -> 1
        parents.insert(11u32, 10u32); // 11 -> 10 -> 1
        parents.insert(12u32, 11u32); // 12 -> 11 -> 10 -> 1
        assert!(descends_from(&parents, 10, 1, 1));
        assert!(descends_from(&parents, 12, 11, 1));
        assert!(!descends_from(&parents, 12, 11, 99));
        assert!(descends_from(&parents, 1, 0, 1));
    }

    #[test]
    fn descends_from_survives_a_parent_cycle() {
        let mut parents = HashMap::new();
        parents.insert(20u32, 21u32);
        parents.insert(21u32, 20u32);
        assert!(!descends_from(&parents, 20, 21, 1));
    }
}
