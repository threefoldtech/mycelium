fn main() {
    if std::env::var_os("CARGO_FEATURE_FFI").is_none() {
        return;
    }
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let header = std::path::PathBuf::from(&out_dir).join("mycelium.h");
    cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(cbindgen::Config::from_root_or_default(&crate_dir))
        .generate()
        .expect("cbindgen failed")
        .write_to_file(&header);
    // Surface the absolute path so downstream Cargo builds, packaging
    // scripts, and xtasks can locate the header without grovelling through
    // target/.../build/.../out.
    println!("cargo:rustc-env=MYCELIUM_HEADER_PATH={}", header.display());
    println!("cargo:rerun-if-changed=src/ffi");
    println!("cargo:rerun-if-changed=cbindgen.toml");
}
