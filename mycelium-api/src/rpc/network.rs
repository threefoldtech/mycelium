//! `network.*` JSON-RPC namespace — Linux bridge and address management.
//!
//! Exposes bridge creation/deletion, IPv6 address management inside the
//! Mycelium 400::/7 range, and listener policy control. Only Linux has a real
//! implementation (via `rtnetlink`); all other platforms use stubs that return
//! `unsupported_platform` errors.

pub mod errors;
pub mod managed;
pub mod models;

#[cfg(target_os = "linux")]
pub mod linux_impl;
#[cfg(not(target_os = "linux"))]
pub mod stub;

#[cfg(target_os = "linux")]
pub use linux_impl as imp;
#[cfg(not(target_os = "linux"))]
pub use stub as imp;

pub use errors::NetworkError;
pub use managed::ManagedState;
pub use models::{AddressInfo, BridgeInfo, ListenerInfo, ListenerPolicy, NetworkStatus};
