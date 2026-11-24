# YouTube Music Desktop

A Windows desktop app for YouTube Music, built with Rust, Tauri 2 and Microsoft Edge WebView2.

![YouTube Music](icon.png)

## Downloads

[Download v0.1.3](https://github.com/nwn900/yt-music-desktop/releases/tag/v0.1.3) — Windows x64 NSIS installer, compiled locally.

## Features

- System tray, single instance, and hide on close.
- Manual launch opens the window; Windows startup uses the explicit `--autostart` argument.
- Windows media keys, mini-player and track-change notifications while the main window is inactive.
- Native playback controls and album artwork.
- Playback keeps running when the window is hidden, minimised or covered by another window.
- Tray **Reload page** as an escape hatch when the page wedges itself.
- New transparent application and tray icons.

## Background playback

Chromium and Windows both deprioritise a window nobody is looking at, and either one can starve the audio thread. The app removes both halves.

- Browser flags disable background timer throttling, renderer backgrounding, occluded-window backgrounding, the Windows occlusion tracker and intensive wake-up throttling. YouTube Music refills its MediaSource buffer from JavaScript, so a clamped timer is an empty buffer and a stall.
- The injected bridge watches the media element and resumes playback that was interrupted while the app was in the background. A pause the user asked for — media key, mini-player button, or a click in the page — is never overridden, and a focused window is left alone.
- The app and its WebView2 process tree are opted out of Windows power throttling (EcoQoS), which is what parks a background process on a low-power core budget.

Playback metadata uses a player-bar observer; elapsed position updates run once per second while playing.

## Build locally

Install Rust's MSVC toolchain, Visual Studio C++ Build Tools, WebView2, and the Tauri CLI (`cargo install tauri-cli --locked`). Use a Visual Studio developer shell with `CC=cl.exe` and `CXX=cl.exe`.

```powershell
cd src-tauri
cargo test --locked
cargo tauri build -- --locked
```

The installer is generated in `target/release/bundle/nsis/`. To limit disk usage, set `CARGO_TARGET_DIR` to a dedicated directory on a drive with free space, set `CARGO_INCREMENTAL=0`, and build one app at a time. Copy completed installers outside the target directory before running `cargo clean`.

Run JavaScript regression tests with `node --test tests/bridge.test.cjs`.

Releases are uploaded from local builds. Tag pushes do not run a GitHub installer build.

## Verification limits

Automated tests cover wrapper logic and simulated page signals. Signed-in provider flows, real response completion, Windows notification visibility, and rendering timings require live verification. Windows notification settings and Do Not Disturb can suppress visible banners.

Background playback was verified by driving the running build over the DevTools protocol: the browser process carries every anti-throttling switch, the page keeps its timers while the window is hidden, and the keep-alive resumes a pause injected from outside the page. Audible continuity on a battery-limited laptop was not measured.
