#[cfg(target_os="windows")]
extern crate cc;


#[cfg(not(target_os="windows"))]
fn main() {}

#[cfg(target_os="windows")]
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=ext/vswhom.cpp");

    let mut cc = cc::Build::new();
    cc.cpp(true);
    // Force the MSVC toolchain — on this box cc-rs otherwise falls back to
    // g++ because no CXX env var is set. Locate cl.exe + the bundled SDK
    // include/lib paths via vswhom's own discovery routine.
    cc.compiler("cl.exe");
    // Disable C++ standard selection. The default cc-rs behaviour stamps a
    // __cplusplus macro and emits C++-style COMDATs (IMAGE_COMDAT_SELECT_ASSOCIATIVE)
    // that newer link.exe (VS 2026 / MSVC 14.50) refuse with LNK1143.
    // Force the older c++14 standard *and* set /Zc:__cplusplus off so the
    // preprocessor does not add the 202002L (or newer) value that the linker
    // sees as a different COMDAT group.
    cc.cpp_set_stdlib(None);
    cc.flag("/std:c++14");
    cc.flag("/Zc:__cplusplus-");
    cc.flag("/Zm2000");
    cc.file("ext/vswhom.cpp");
    cc.compile("vswhom");

    println!("cargo:rustc-link-lib=dylib=OleAut32");
    println!("cargo:rustc-link-lib=dylib=Ole32");
}
