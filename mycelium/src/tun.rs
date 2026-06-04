//! The tun module implements a platform independent Tun interface.

#[cfg(any(
    target_os = "linux",
    all(target_os = "macos", not(feature = "mactunfd")),
    target_os = "windows",
    all(target_os = "android", not(feature = "androidtunfd")),
))]
use crate::subnet::Subnet;

#[cfg(any(
    target_os = "linux",
    all(target_os = "macos", not(feature = "mactunfd")),
    target_os = "windows",
    all(target_os = "android", not(feature = "androidtunfd")),
))]
pub struct TunConfig {
    pub name: String,
    pub node_subnet: Subnet,
    pub route_subnet: Subnet,
}

#[cfg(any(
    target_os = "ios",
    all(target_os = "macos", feature = "mactunfd"),
    all(target_os = "android", feature = "androidtunfd"),
))]
pub struct TunConfig {
    pub tun_fd: i32,
}

// Android without the `androidtunfd` feature uses the same Linux uAPI as
// `target_os = "linux"`, so it compiles `tun/linux.rs`.
#[cfg(any(
    target_os = "linux",
    all(target_os = "android", not(feature = "androidtunfd")),
))]
mod linux;

#[cfg(any(
    target_os = "linux",
    all(target_os = "android", not(feature = "androidtunfd")),
))]
pub use linux::new;

#[cfg(all(target_os = "macos", not(feature = "mactunfd")))]
mod darwin;

#[cfg(all(target_os = "macos", not(feature = "mactunfd")))]
pub use darwin::new;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::new;

#[cfg(all(target_os = "android", feature = "androidtunfd"))]
mod android;
#[cfg(all(target_os = "android", feature = "androidtunfd"))]
pub use android::new;

#[cfg(any(target_os = "ios", all(target_os = "macos", feature = "mactunfd")))]
mod ios;
#[cfg(any(target_os = "ios", all(target_os = "macos", feature = "mactunfd")))]
pub use ios::new;
