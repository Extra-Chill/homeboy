use std::sync::Arc;
use std::time::{Duration, Instant};

/// Caller-owned cancellation and deadline state for cooperative providers.
///
/// Providers must poll this before starting work and between blocking steps.
/// It cannot forcibly stop arbitrary in-process third-party code; providers
/// that need a hard deadline must supervise their child processes themselves.
#[derive(Clone)]
pub struct CooperativeControl {
    deadline: Instant,
    is_cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl CooperativeControl {
    pub fn new(deadline: Instant, is_cancelled: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self {
            deadline,
            is_cancelled,
        }
    }

    pub fn is_cancelled(&self) -> bool {
        (self.is_cancelled)() || Instant::now() >= self.deadline
    }

    pub fn remaining(&self) -> Option<Duration> {
        (!self.is_cancelled()).then(|| self.deadline.saturating_duration_since(Instant::now()))
    }

    pub fn unbounded() -> Self {
        Self::new(
            Instant::now() + Duration::from_secs(365 * 24 * 60 * 60),
            Arc::new(|| false),
        )
    }
}
