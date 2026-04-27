//! C FFI surface for the mycelium node.
//!
//! Compiled only with `--features ffi`. The crate then also produces a
//! `cdylib` (`libmycelium.so` / `.dylib`) and `staticlib` (`libmycelium.a`),
//! and `build.rs` runs `cbindgen` to write `include/mycelium.h`.
//!
//! The surface is opaque-handle: `mycelium_start` returns a
//! `mycelium_node_t *` that the caller passes back into every other entry
//! point. Lifecycle (singleton-or-not, lifetime, threading) is the
//! consumer's choice, not ours.

mod conversions;
mod error;
mod exports;
mod types;

pub use error::{
    mycelium_last_error_message, MYCELIUM_ERR_INTERNAL, MYCELIUM_ERR_INVALID_ARG,
    MYCELIUM_ERR_INVALID_STATE, MYCELIUM_OK,
};
pub use exports::*;
pub use types::*;
