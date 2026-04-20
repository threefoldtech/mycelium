//! Android AIDL (Binder IPC) interface for the mycelium node.
//!
//! This module is only compiled when the `aidl` feature is enabled. It provides
//! [`MyceliumService`] which implements the `IMyceliumService` AIDL interface,
//! and [`add_service`] to register the service with Android's ServiceManager.

// Pull in the AIDL-generated Rust bindings (tech::threefold::mycelium module).
include!(concat!(env!("OUT_DIR"), "/mycelium_aidl.rs"));

mod conversions;
mod service;
mod service_manager;

pub use service::MyceliumService;
pub use service_manager::add_service;

// Re-export the generated BnMyceliumService for the binary to use.
pub use tech::threefold::mycelium::IMyceliumService::BnMyceliumService;
pub use tech::threefold::mycelium::IMyceliumService::IMyceliumService;
