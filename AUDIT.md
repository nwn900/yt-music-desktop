# Desktop wrapper audit — 2026-09-11

## Scope

Reviewed Rust startup, navigation, native commands, notification integration, injected JavaScript, icons, installer configuration and release automation. This is a bounded wrapper audit, not proof that every bug in the application or upstream service has been found.

## Corrections

- Fixed strict-mode retry failure caused by arguments.callee.
- Fixed the media_update IPC argument envelope.
- Connected Windows media-key events to playback controls.
- Created the missing mini-player window, restored its initial state, corrected its artwork field and preserved it on close.
- Allowed seeking to zero and propagated seeks while paused.
- Captured video play/pause events so controls update without requiring a player-bar DOM mutation.
- Added background track-change Windows notifications with click-to-restore.
- Restricted remote IPC to media updates; transport and snapshots belong to the local mini-player.
- Distinguished actual Windows autostart from manually opening the application.
- Restored minimized windows when opening from the tray or another instance.
- Restricted external URL launching to web/mail schemes.
- Applied the supplied transparent Desktop icon to application, tray, installer and repository artwork.
- Updated Tauri and committed reproducible dependency resolution.
- Documented sequential local builds on a dedicated drive; release assets must be preserved before cleaning generated output.

## Validation and limits

Rust unit tests and Node simulated-DOM regression tests are run locally, followed by an optimized Windows x64 NSIS build. These checks cover wrapper logic and packaging, not signed-in end-to-end behavior.

No measured page-rendering speedup is claimed. The changes remove wrapper scanning/layout overhead; provider latency and WebView2 rendering still require before/after profiling. DOM selectors can drift with upstream redesigns.

Real provider authentication, streaming responses, notification banner visibility/click activation, media keys and installation upgrades still need live acceptance testing. Windows notification settings and Do Not Disturb can suppress banners. A complete dependency vulnerability audit was not performed.

## Rollback

Previous GitHub release installers remain available. Close the application through its tray menu before reinstalling the prior version if authentication, playback or notification regressions occur. Do not delete the WebView2 user profile. No profile migration or user-data cleanup is part of this release.

## v0.1.3 — background playback

### What was wrong

Playback degraded when the window was not in front. Chromium has four independent ways to deprioritise a window nobody is looking at, and the app only disabled two of them:

| mechanism | switch | before |
| --- | --- | --- |
| page timer clamp | `--disable-background-timer-throttling` | present |
| renderer process priority | `--disable-renderer-backgrounding` | present |
| occluded-window backgrounding | `--disable-backgrounding-occluded-windows` | **missing** |
| Windows occlusion tracker | `--disable-features=CalculateNativeWinOcclusion` | **missing** |
| intensive wake-up clamp (after 5 min hidden) | `--disable-features=IntensiveWakeUpThrottling` | **missing** |

YouTube Music refills its MediaSource buffer from JavaScript, so a clamped timer is an empty buffer and a stall. The occlusion tracker is the "not on the forefront" case: a window that is merely covered is marked hidden.

The OS half is Windows 11 power throttling (EcoQoS), which parks a background process on a low-power core budget. An audio thread that misses its deadline underruns, which is the stutter.

### Fix

- `BROWSER_ARGS` now carries all five switches. A unit test fails if any of them is dropped, and the existing mini/main drift test keeps `tauri.conf.json` in step.
- `bridge.js` gained a 2 s keep-alive. It resumes a pause that happened while the app was in the background, and nudges a player that stops advancing while it claims to be playing. It never overrides a pause the user asked for: the transport helpers (media keys, mini-player, seeks) and any pointer or key activity mark intent, and a focused window is left alone. A gap longer than two minutes clears the intent so the app does not start playing on its own after the machine slept.
- `lifecycle.rs` opts the app and every `msedgewebview2.exe` descendant out of `PROCESS_POWER_THROTTLING_EXECUTION_SPEED` and re-asserts it every 20 s, because Chromium re-applies its own throttling when a renderer is backgrounded.

### Ported from the sibling ChatGPT desktop wrapper

- Window restore runs on a worker thread. `ShowWindow` and `SetForegroundWindow` send synchronous messages to explorer.exe, and a wedged shell froze that app's event loop.
- Orphaned `msedgewebview2.exe` processes from a killed run are reaped at startup, before this process builds its own webview, so a stale tree cannot fight this launch over the same `EBWebView` profile.
- An event-loop watchdog logs when the UI thread stops pumping.
- Tray **Reload page**.
- Track-change toasts moved off the IPC thread, and `media-state` is emitted to the mini-player only instead of running a JS eval inside the YouTube Music page twice a second.

### Verification

- 14 Rust unit tests and 8 JavaScript bridge tests pass.
- The built binary was driven over the DevTools protocol: the browser process command line carries every switch; `document.visibilityState` stays `visible` and the 1 s position tick keeps running while the window is hidden to the tray, minimised, and covered by a maximised window; `document.hasFocus()` is the signal the keep-alive uses and it reports `false` in all three background states.
- Audible continuity on a battery-limited laptop was not measured. The switch set and the keep-alive are the parts that can be verified without one.
