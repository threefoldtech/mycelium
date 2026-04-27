//! C FFI surface for the mycelium node.
//!
//! Compiled only with `--features ffi`. The crate then also produces a
//! `cdylib` (`libmycelium.so` / `.dylib`) and `staticlib` (`libmycelium.a`),
//! and `build.rs` runs `cbindgen` to write `include/mycelium.h`.
//!
//! The surface is process-singleton: a single `NodeHandle` lives in
//! `NODE` and every entry point operates against it. Two processes that
//! both load the library each get their own `NODE` (writable globals are
//! per-process under copy-on-write), so this is exactly what you want when
//! the library is wrapped by an external daemon.

mod conversions;
mod error;
mod exports;
mod types;

use std::sync::{Mutex, OnceLock};

use crate::node_handle::NodeHandle;

static NODE: OnceLock<Mutex<Option<NodeHandle>>> = OnceLock::new();

pub(crate) fn node_slot() -> &'static Mutex<Option<NodeHandle>> {
    NODE.get_or_init(|| Mutex::new(None))
}

pub use error::{
    mycelium_last_error_message, MYCELIUM_ERR_INTERNAL, MYCELIUM_ERR_INVALID_ARG,
    MYCELIUM_ERR_INVALID_STATE, MYCELIUM_OK,
};
pub use exports::*;
pub use types::*;
