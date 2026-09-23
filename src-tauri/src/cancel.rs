//! Shared cancellation flag for long-running fleet jobs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Process-wide cancel flag shared with Tauri managed state.
#[derive(Clone, Default)]
pub struct JobCancel {
    flag: Arc<AtomicBool>,
}

impl JobCancel {
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Clear cancel before starting a new job; returns the shared flag for workers.
    pub fn begin(&self) -> Arc<AtomicBool> {
        self.flag.store(false, Ordering::SeqCst);
        Arc::clone(&self.flag)
    }

    pub fn request_cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(flag: &AtomicBool) -> bool {
        flag.load(Ordering::SeqCst)
    }
}
