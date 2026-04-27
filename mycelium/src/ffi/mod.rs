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
//!
//! # Safety conventions
//!
//! Common to every `extern "C"` entry point in this module; per-function
//! `# Safety` sections only document the bits that go beyond these:
//!
//! - Pointer arguments are either NULL (only where the per-function doc
//!   says NULL is accepted) or valid: properly aligned, non-dangling,
//!   pointing to an initialised value of the documented type.
//! - C string inputs (`*const c_char`) must be NUL-terminated. UTF-8 is
//!   validated at runtime; invalid bytes yield `MYCELIUM_ERR_INVALID_ARG`
//!   rather than UB.
//! - Out-pointers must point to writable, properly aligned memory of the
//!   right type. Writes happen only on success (return code 0, or non-NULL
//!   handle from `mycelium_start`).
//! - `mycelium_node_t *` arguments must come from `mycelium_start` and
//!   must not have been passed to `mycelium_node_free`. Use-after-free,
//!   double-free, or freeing on one thread while another thread is using
//!   the same handle is undefined behaviour.
//! - Strings and structs that the library hands back are owned by the
//!   caller and must be released with the matching `mycelium_*_free`
//!   function. Reading the contents from multiple threads is safe; freeing
//!   must not race with reads.
//! - The library is not async-signal safe; do not call entry points from
//!   a signal handler.

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
