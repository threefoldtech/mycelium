// Pull in the AIDL-generated Rust bindings (tech::threefold::mycelium module).
include!(concat!(env!("OUT_DIR"), "/mycelium_aidl.rs"));

mod node;
mod service;
mod service_manager;

use service::MyceliumService;

fn main() {
    setup_logging();

    let args: Vec<String> = std::env::args().collect();
    let key_file = args
        .iter()
        .position(|a| a == "--key-file")
        .map(|i| args[i + 1].clone());
    let peers: Vec<String> = args
        .iter()
        .position(|a| a == "--peers")
        .map(|i| {
            args[i + 1..]
                .iter()
                .take_while(|a| !a.starts_with("--"))
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    rsbinder::ProcessState::init_default();
    rsbinder::ProcessState::start_thread_pool();

    let svc = MyceliumService::new();

    if let Some(path) = &key_file {
        let key_data = std::fs::read(path).expect("failed to read key file");
        use tech::threefold::mycelium::IMyceliumService::IMyceliumService;
        svc.start(&peers, &key_data, false)
            .expect("failed to start mycelium node");
    }

    let binder = tech::threefold::mycelium::IMyceliumService::BnMyceliumService::new_binder(svc);

    service_manager::add_service(
        "tech.threefold.mycelium.IMyceliumService",
        binder.as_binder(),
    )
    .expect("failed to register mycelium service with ServiceManager");

    tracing::info!("mycelium service registered, entering Binder thread pool");

    let _ = rsbinder::ProcessState::join_thread_pool();
}

// ── Logging setup ─────────────────────────────────────────────────────────────
// NOTE: This is copied from mobile.
// TODO: Is this needed, i.e. do we care about capturing logs?

#[cfg(target_os = "android")]
fn setup_logging() {
    use tracing::level_filters::LevelFilter;
    use tracing_subscriber::filter::Targets;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let targets = Targets::new()
        .with_default(LevelFilter::INFO)
        .with_target("mycelium::router", LevelFilter::WARN);

    tracing_subscriber::registry()
        .with(tracing_android::layer("mycelium-android").expect("failed to setup Android logger"))
        .with(targets)
        .init();
}

// Convenience when building on linux
#[cfg(not(target_os = "android"))]
fn setup_logging() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
}
