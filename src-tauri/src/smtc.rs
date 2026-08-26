//! SMTC (System Media Transport Controls) wrapper around `souvlaki`.
//!
//! Owns the `MediaControls` instance and the last-sent [`MediaState`]
//! so updates are diffed before touching the OS (metadata changes are
//! rare; the 1 Hz position tick must not rewrite metadata).

use std::sync::Mutex;
use std::time::Duration;

use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use tauri::Manager;

/// Snapshot of the YTM player as reported by the injected bridge script.
#[derive(Clone, PartialEq, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct MediaState {
    pub title: Option<String>,
    pub album: Option<String>,
    pub artist: Option<String>,
    pub cover_url: Option<String>,
    pub playing: bool,
    /// Whole seconds.
    pub position: u64,
    /// Whole seconds; `None` when unknown (ad, live, no video element).
    pub duration: Option<u64>,
}

impl MediaState {
    /// Non-position fields differ -> metadata must be resent.
    fn metadata_differs(&self, other: &Self) -> bool {
        self.title != other.title
            || self.album != other.album
            || self.artist != other.artist
            || self.cover_url != other.cover_url
            || self.playing != other.playing
            || self.duration != other.duration
    }

    /// Position moved at least `threshold` seconds while the rest is stable.
    fn position_moved(&self, other: &Self, threshold: u64) -> bool {
        self.playing == other.playing
            && self.duration == other.duration
            && self.position.abs_diff(other.position) >= threshold
    }
}

/// Process-wide SMTC state, stored via `tauri::Builder::manage`.
pub struct Smtc {
    controls: Mutex<Option<MediaControls>>,
    last: Mutex<MediaState>,
}

impl Default for Smtc {
    fn default() -> Self {
        Self {
            controls: Mutex::new(None),
            last: Mutex::new(MediaState::default()),
        }
    }
}

impl Smtc {
    /// Inject the souvlaki controls once the main window (and its HWND) exist.
    /// SMTC binding failure must never take the app down: log- and continue.
    pub fn init(&self, hwnd: *mut std::ffi::c_void) {
        let mut slot = self.controls.lock().expect("smtc controls lock");
        if slot.is_some() {
            return;
        }
        let mut controls = match MediaControls::new(PlatformConfig {
            display_name: "YouTube Music",
            dbus_name: "yt-music-desktop",
            hwnd: Some(hwnd),
        }) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[smtc] init failed, media keys disabled: {e}");
                return;
            }
        };
        if let Err(e) = controls.attach(|_| {}) {
            eprintln!("[smtc] attach failed, media keys disabled: {e}");
            return;
        }
        *slot = Some(controls);
    }

    /// Diff `state` against the last-sent one and update SMTC if needed.
    /// Returns `true` when the state is a meaningful change (callers
    /// rebroadcast to the mini-player on that signal).
    pub fn update(&self, state: &MediaState) -> bool {
        let mut last = self.last.lock().expect("smtc last-state lock");

        let metadata_changed = last.metadata_differs(state);
        // Forward position when playing even without metadata changes, but
        // clamp to >= 2s deltas so a paused/hidden window doesn't spam SMTC.
        let pos_tick = !metadata_changed
            && state.playing
            && last.position_moved(state, 2)
            && state.duration.is_some();

        if !metadata_changed && !pos_tick {
            return false;
        }

        let result = self.send(state, metadata_changed);
        *last = state.clone();
        result
    }

    fn send(&self, state: &MediaState, metadata_changed: bool) -> bool {
        let mut slot = self.controls.lock().expect("smtc controls lock");
        let Some(controls) = slot.as_mut() else {
            return false;
        };

        let progress = MediaPosition(Duration::from_secs(state.position));

        if metadata_changed {
            let mut metadata = MediaMetadata::default();
            if state.duration.is_some() {
                metadata.duration = Some(Duration::from_secs(state.duration.unwrap_or(0)));
            }
            if let Some(title) = state.title.as_deref() {
                metadata.title = Some(title);
            }
            if let Some(artist) = state.artist.as_deref() {
                metadata.artist = Some(artist);
            }
            if let Some(album) = state.album.as_deref() {
                metadata.album = Some(album);
            }
            if let Some(cover) = state.cover_url.as_deref() {
                metadata.cover_url = Some(cover);
            }
            if let Err(e) = controls.set_metadata(metadata) {
                eprintln!("[smtc] set_metadata failed: {e}");
                return false;
            }
        }

        let playback =
            if !state.playing && (state.title.is_none() || state.title.as_deref() == Some("")) {
                MediaPlayback::Stopped
            } else if state.playing {
                MediaPlayback::Playing {
                    progress: Some(progress),
                }
            } else {
                MediaPlayback::Paused {
                    progress: Some(progress),
                }
            };

        match controls.set_playback(playback) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("[smtc] set_playback failed: {e}");
                false
            }
        }
    }

    /// Forward an OS media-control event into the main webview. The JS side
    /// clicks YTM's own transport buttons, so no duplicative state plumbing.
    #[allow(dead_code)]
    pub fn forward_event(&self, event: MediaControlEvent) {
        let app = match crate::APP_HANDLE.lock() {
            Ok(guard) => guard.clone(),
            Err(_) => return,
        };
        let Some(app) = app else { return };
        let Some(window) = app.get_webview_window("main") else {
            return;
        };
        let js = match event {
            MediaControlEvent::Play => "clickTransport('play')",
            MediaControlEvent::Pause => "clickTransport('pause')",
            MediaControlEvent::Toggle => "clickTransport('toggle')",
            MediaControlEvent::Next => "clickTransport('next')",
            MediaControlEvent::Previous => "clickTransport('previous')",
            MediaControlEvent::SetPosition(pos) => {
                return seek(&window, pos.0.as_secs());
            }
            _ => return,
        };
        let _ = window.eval(js);
    }
}

/// Seek the YTM video via the injected helper.
#[allow(dead_code)]
fn seek(window: &tauri::WebviewWindow, seconds: u64) {
    let _ = window.eval(format!("window.__ytmSeek && window.__ytmSeek({seconds});"));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(title: &str, playing: bool, pos: u64, dur: u64) -> MediaState {
        MediaState {
            title: Some(title.to_string()),
            album: None,
            artist: Some("artist".to_string()),
            cover_url: None,
            playing,
            position: pos,
            duration: Some(dur),
        }
    }

    #[test]
    fn metadata_change_is_a_change() {
        let a = state("Song A", true, 10, 240);
        let b = state("Song B", true, 10, 240);
        assert!(a.metadata_differs(&b));
        assert!(!a.metadata_differs(&a));
    }

    #[test]
    fn position_move_over_threshold_is_detected() {
        let a = state("Song A", true, 100, 300);
        let b = state("Song A", true, 103, 300);
        let c = state("Song A", true, 101, 300);
        assert!(a.position_moved(&b, 2));
        assert!(!a.position_moved(&c, 2));
    }

    #[test]
    fn playing_flag_change_is_a_metadata_change() {
        let a = state("Song A", true, 100, 300);
        let b = state("Song A", false, 100, 300);
        assert!(a.metadata_differs(&b));
    }
}
