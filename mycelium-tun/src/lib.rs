//! Platform-specific TUN device implementations.
//!
//! Linux and Android share the same implementation (Android is Linux + bionic
//! and exposes the same `/dev/net/tun` uAPI). On other platforms this crate
//! is empty.

#[cfg(any(target_os = "linux", target_os = "android"))]
mod checksum;
#[cfg(any(target_os = "linux", target_os = "android"))]
mod linux;
#[cfg(any(target_os = "linux", target_os = "android"))]
mod offload;

#[cfg(any(target_os = "linux", target_os = "android"))]
pub use linux::{ReadHalf, Tun, WriteHalf};
