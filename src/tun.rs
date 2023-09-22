//! The tun module implements a platform independent Tun interface.

#[cfg(target_os = "linux")]
mod linux;

#[cfg(not(target_os = "linux"))]
compile_error!("The platform you are compiling on is currently not supported");

#[cfg(target_os = "linux")]
pub use linux::new;
