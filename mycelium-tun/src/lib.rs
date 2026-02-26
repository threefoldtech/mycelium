//! Platform-specific TUN device implementations.
//!
//! Currently only Linux is supported. On other platforms this crate is empty.

#[cfg(target_os = "linux")]
mod checksum;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod offload;

#[cfg(target_os = "linux")]
pub use linux::{ReadHalf, Tun, WriteHalf};
