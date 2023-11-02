//! The tun module implements a platform independent Tun interface.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::new;
