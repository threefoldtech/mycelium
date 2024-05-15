//! The tun module implements a platform independent Tun interface.

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use crate::subnet::Subnet;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub struct TunConfig {
    pub name: String,
    pub node_subnet: Subnet,
    pub route_subnet: Subnet,
}

#[cfg(any(target_os = "android", target_os = "ios"))]
pub struct TunConfig {
    pub tun_fd: i32,
}
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::new;

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "macos")]
pub use darwin::new;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::new;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::new;

#[cfg(target_os = "ios")]
mod ios;
#[cfg(target_os = "ios")]
pub use ios::new;
