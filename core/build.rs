use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let lib = manifest
        .join("../third_party/librime/dist/lib")
        .canonicalize()
        .unwrap_or_else(|_| manifest.join("../third_party/librime/dist/lib"));

    // Windows/macOS：必须用 third_party 官方预编译产物（MSVC 命名 rime.lib / macOS librime.dylib）。
    // Linux：官方 release 无预编译产物，优先 third_party 本地产物，否则链发行版包 librime-dev。
    if cfg!(any(target_os = "windows", target_os = "macos")) {
        let name = if cfg!(target_os = "windows") {
            "rime.lib"
        } else {
            "librime.dylib"
        };
        if !lib.join(name).exists() {
            panic!(
                "未找到 {}。请先获取 librime 预编译产物，见 third_party/README.md。",
                lib.join(name).display()
            );
        }
        println!("cargo:rustc-link-search=native={}", lib.display());
    } else if lib.join("librime.so").exists() {
        println!("cargo:rustc-link-search=native={}", lib.display());
    }
    println!("cargo:rustc-link-lib=dylib=rime");
    println!("cargo:rerun-if-changed=build.rs");
}
