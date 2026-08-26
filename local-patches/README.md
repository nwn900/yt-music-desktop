# vswhom-sys 0.1.3 — local patch

## Why

Upstream `vswhom-sys 0.1.x` lets `cc-rs` drive its C++ build, which on the
VS 2026 / MSVC 14.50 toolchain emits a `__cplusplus` COMDAT
(`IMAGE_COMDAT_SELECT_ASSOCIATIVE`) that the new `link.exe` refuses with:

```
LNK1143: invalid or corrupt file: no symbol for section COMDAT 0x5
```

## What the patch does

Two surgical changes to the vendored copy:

1. `cc.cpp_set_stdlib(None)` and pass `/std:c++14` explicitly.
2. Add `/Zc:__cplusplus-` so the compiler does not stamp the new
   `__cplusplus` macro value, which `cc-rs`'s default would otherwise add
   to the object file.

Result: `cl.exe` produces a `.obj` without the offending COMDAT; the link
succeeds.

## Activated via

`src-tauri/Cargo.toml` carries a `[patch.crates-io]` entry pointing at this
directory, so `cargo build` / `cargo tauri build` automatically use the
patched version. No manual step needed at build time.

## If you upgrade MSVC

Try removing this patch first. If `vswhom-sys` still trips, the patch is
still applicable. If upstream ships a fix, delete the `[patch.crates-io]`
entry and this directory.
