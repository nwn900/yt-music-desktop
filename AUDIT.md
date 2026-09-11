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
