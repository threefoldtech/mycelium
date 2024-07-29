//! This module provides some task abstractions which add custom logic to the default behavior.

/// A handle to a task, which is only used to abort the task. In case this handle is dropped, the
/// task is cancelled automatically.
pub struct AbortHandle(tokio::task::AbortHandle);

impl AbortHandle {
    /// Abort the task this `AbortHandle` is referencing. It is safe to call this method multiple
    /// times, but only the first call is actually usefull. It is possible for the task to still
    /// finish succesfully, even after abort is called.
    #[inline]
    pub fn abort(&self) {
        self.0.abort()
    }
}

impl Drop for AbortHandle {
    #[inline]
    fn drop(&mut self) {
        self.0.abort()
    }
}

impl From<tokio::task::AbortHandle> for AbortHandle {
    #[inline]
    fn from(value: tokio::task::AbortHandle) -> Self {
        Self(value)
    }
}
