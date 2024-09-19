//! The tun module implements a platform independent Tun interface.

#[cfg(any(
    target_os = "linux",
    all(target_os = "macos", not(feature = "mactunfd")),
    target_os = "windows"
))]
use crate::subnet::Subnet;

#[cfg(any(
    target_os = "linux",
    all(target_os = "macos", not(feature = "mactunfd")),
    target_os = "windows"
))]
pub struct TunConfig {
    pub name: String,
    pub node_subnet: Subnet,
    pub route_subnet: Subnet,
}

#[cfg(any(
    target_os = "android",
    target_os = "ios",
    all(target_os = "macos", feature = "mactunfd"),
))]
pub struct TunConfig {
    pub tun_fd: i32,
}
#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
pub use linux::new;

#[cfg(all(target_os = "macos", not(feature = "mactunfd")))]
mod darwin;

#[cfg(all(target_os = "macos", not(feature = "mactunfd")))]
pub use darwin::new;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::new;

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::new;

#[cfg(any(target_os = "ios", all(target_os = "macos", feature = "mactunfd")))]
mod ios;
#[cfg(any(target_os = "ios", all(target_os = "macos", feature = "mactunfd")))]
pub use ios::new;
