fn main() {
    #[cfg(feature = "aidl")]
    aidl_codegen();
}

#[cfg(feature = "aidl")]
fn aidl_codegen() {
    let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());

    let output = out_dir.join("mycelium_aidl.rs");
    rsbinder_aidl::Builder::new()
        .source("aidl/tech/threefold/mycelium/PeerInfo.aidl")
        .source("aidl/tech/threefold/mycelium/NodeInfo.aidl")
        .source("aidl/tech/threefold/mycelium/Route.aidl")
        .source("aidl/tech/threefold/mycelium/QueriedSubnet.aidl")
        .source("aidl/tech/threefold/mycelium/PacketStatEntry.aidl")
        .source("aidl/tech/threefold/mycelium/PacketStats.aidl")
        .source("aidl/tech/threefold/mycelium/IMyceliumService.aidl")
        .output(&output)
        .generate()
        .unwrap();
    patch_async_compat(&output);

    let sm_output = out_dir.join("service_manager_a15.rs");
    rsbinder_aidl::Builder::new()
        .source("aidl/android/os/IServiceManager.aidl")
        .output(&sm_output)
        .generate()
        .unwrap();
    patch_async_compat(&sm_output);

    println!("cargo:rerun-if-changed=aidl/");
}

/// Fix rsbinder-aidl 0.6 generated code for async-trait >= 0.1.80 on Rust >= 1.75.
///
/// The generated `IXxxAsyncService` traits use `async fn` methods annotated with
/// `#[async_trait]`. With modern async-trait the macro uses native AFIT, making
/// the trait non-dyn-compatible. We rewrite `async fn` to return `BoxFuture`.
#[cfg(feature = "aidl")]
fn patch_async_compat(path: &std::path::Path) {
    let src = std::fs::read_to_string(path).expect("read generated AIDL file");

    let patched = src.replace("#[::async_trait::async_trait]\n", "");

    let async_fn_re =
        regex::Regex::new(r"(?m)^(\s+)async fn (r#\w+)\(([^)]+)\)\s*->\s*([^;]+);$").unwrap();

    let patched = async_fn_re
        .replace_all(&patched, |caps: &regex::Captures| {
            let indent = &caps[1];
            let name = &caps[2];
            let params = caps[3]
                .replace("&self", "\x00SELF\x00")
                .replace('&', "&'a ")
                .replace("\x00SELF\x00", "&self");
            let ret = caps[4].trim();
            format!("{indent}fn {name}<'a>({params}) -> rsbinder::BoxFuture<'a, {ret}>;")
        })
        .into_owned();

    std::fs::write(path, patched).expect("write patched AIDL file");
}
