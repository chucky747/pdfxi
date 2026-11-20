//! Cooperative cancellation shared between the UI thread and a request.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A cloneable cancellation handle.
///
/// Every request checks the flag before it opens a connection, between event
/// stream reads and between answer polls. Cancelling is idempotent and never
/// blocks the caller.
#[derive(Debug, Clone, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    /// Creates a flag that is not cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests cancellation. Safe to call from any thread, any number of times.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl From<Arc<AtomicBool>> for CancelFlag {
    fn from(shared: Arc<AtomicBool>) -> Self {
        Self(shared)
    }
}

#[cfg(test)]
mod tests {
    use super::CancelFlag;

    #[test]
    fn clones_share_the_flag() {
        let flag = CancelFlag::new();
        let clone = flag.clone();
        assert!(!clone.is_cancelled());
        flag.cancel();
        assert!(clone.is_cancelled());
    }
}
