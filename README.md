# YouTube Music Desktop

A Windows desktop app for YouTube Music, built with Rust, Tauri 2 and Microsoft Edge WebView2.

![YouTube Music](icon.png)

## Downloads

[Download v0.1.2](https://github.com/nwn900/yt-music-desktop/releases/tag/v0.1.2) — Windows x64 NSIS installer, compiled locally.

## Features

- System tray, single instance, and hide on close.
- Manual launch opens the window; Windows startup uses the explicit `--autostart` argument.
- Windows media keys, mini-player and track-change notifications while the main window is inactive.
- Native playback controls and album artwork.
- New transparent application and tray icons.

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
