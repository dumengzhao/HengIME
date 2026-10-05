use std::path::PathBuf;

fn main() {
    // macOS：可执行文件经 @rpath 找 librime.1.dylib，把 third_party lib 目录写进 LC_RPATH。
    // 注意 core/build.rs 的 rustc-link-arg 不会传播到下游二进制，这里必须重复声明。
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let lib = manifest
        .join("../../third_party/librime/dist/lib")
        .canonicalize()
        .unwrap_or_else(|_| manifest.join("../../third_party/librime/dist/lib"));
    if cfg!(target_os = "macos") && lib.join("librime.dylib").exists() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
    }
    println!("cargo:rerun-if-changed=build.rs");
}
