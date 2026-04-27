//! Last-error reporting for the C FFI surface.
//!
//! Every fallible entry point returns an `int` status code; a thread-local
//! `CString` holds an optional human-readable message. Callers can fetch the
//! message via [`mycelium_last_error_message`]. The pointer is owned by the
//! library; do not free it.

use core::ffi::c_char;
use std::cell::RefCell;
use std::ffi::CString;

/// Operation succeeded.
pub const MYCELIUM_OK: i32 = 0;

/// One of the arguments was invalid (null, out-of-range, malformed UTF-8, …).
pub const MYCELIUM_ERR_INVALID_ARG: i32 = -1;

/// The node is in the wrong state for this operation (e.g. start while
/// running, or query while stopped).
pub const MYCELIUM_ERR_INVALID_STATE: i32 = -2;

/// An internal failure surfaced from the underlying node implementation.
pub const MYCELIUM_ERR_INTERNAL: i32 = -3;

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

pub(super) fn set(msg: impl Into<Vec<u8>>) {
    let cstr = CString::new(msg)
        .unwrap_or_else(|_| CString::new("<error message contained NUL>").unwrap());
    LAST_ERROR.with(|s| *s.borrow_mut() = Some(cstr));
}

pub(super) fn set_and_return(msg: impl Into<Vec<u8>>, code: i32) -> i32 {
    set(msg);
    code
}

pub(super) fn clear() {
    LAST_ERROR.with(|s| *s.borrow_mut() = None);
}

/// Returns the last error message produced on this thread, or NULL if there
/// is none. The returned pointer is owned by the library and remains valid
/// only until the next FFI call from the same thread.
#[no_mangle]
pub extern "C" fn mycelium_last_error_message() -> *const c_char {
    LAST_ERROR.with(|s| {
        s.borrow()
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null())
    })
}
