use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let lib = manifest
        .join("../third_party/librime/dist/lib")
        .canonicalize()
        .unwrap_or_else(|_| manifest.join("../third_party/librime/dist/lib"));

    if !lib.join("rime.lib").exists() {
        panic!(
            "未找到 {}。请先获取 librime 预编译产物，见 third_party/README.md。",
            lib.join("rime.lib").display()
        );
    }
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=dylib=rime");
    println!("cargo:rerun-if-changed=build.rs");
}
