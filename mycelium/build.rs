fn main() {
    if std::env::var_os("CARGO_FEATURE_FFI").is_none() {
        return;
    }
    let crate_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let out = std::path::PathBuf::from(&crate_dir)
        .join("include")
        .join("mycelium.h");
    std::fs::create_dir_all(out.parent().unwrap()).expect("create include dir");
    cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(cbindgen::Config::from_root_or_default(&crate_dir))
        .generate()
        .expect("cbindgen failed")
        .write_to_file(&out);
    println!("cargo:rerun-if-changed=src/ffi");
    println!("cargo:rerun-if-changed=cbindgen.toml");
}
