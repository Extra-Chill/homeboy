//! Interruptible waiting.
//!
//! A runner that must wait -- for a daemon to reach a terminal state, for a
//! staging poll interval to elapse -- cannot simply `thread::sleep` the whole
//! interval: a cancellation arriving one millisecond in would not be observed
//! until the interval ended. Both call sites solved this the same way, by
//! slicing the interval and re-checking cancellation between slices, and both
//! chose the same 200ms slice. This is that loop, written once.
//!
//! Cancellation is passed as a closure rather than a token type because the two
//! callers carry different ones (`&dyn Fn() -> bool` and a
//! `LabStagingCancellationToken`). Adapting at the call site keeps this module
//! free of either.

use std::time::Duration;

/// How long to sleep before re-checking cancellation.
///
/// Short enough that a cancelled wait unwinds promptly, long enough that a
/// multi-second wait is not thousands of wakeups. Both callers independently
/// picked this value before it was shared.
const CANCELLATION_POLL_SLICE: Duration = Duration::from_millis(200);

/// Sleep up to `interval`, re-checking `cancelled` every
/// [`CANCELLATION_POLL_SLICE`]. Returns `true` if the wait was cancelled.
///
/// Cancellation is checked *before* the first sleep, so an already-cancelled
/// wait returns immediately without sleeping at all, and again after the last
/// slice, so a cancellation that lands during the final slice is still
/// reported. A zero `interval` is therefore a pure cancellation check.
pub(crate) fn sleep_unless_cancelled(interval: Duration, cancelled: impl Fn() -> bool) -> bool {
    let mut remaining = interval;
    while !remaining.is_zero() {
        if cancelled() {
            return true;
        }
        let slice = remaining.min(CANCELLATION_POLL_SLICE);
        std::thread::sleep(slice);
        remaining = remaining.saturating_sub(slice);
    }
    cancelled()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    #[test]
    fn an_already_cancelled_wait_returns_without_sleeping() {
        let started = Instant::now();
        assert!(sleep_unless_cancelled(Duration::from_secs(30), || true));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a cancelled wait must not sleep the interval"
        );
    }

    #[test]
    fn an_uncancelled_wait_sleeps_the_whole_interval_and_reports_not_cancelled() {
        let started = Instant::now();
        assert!(!sleep_unless_cancelled(Duration::from_millis(300), || {
            false
        }));
        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    #[test]
    fn a_zero_interval_is_a_pure_cancellation_check() {
        assert!(!sleep_unless_cancelled(Duration::ZERO, || false));
        assert!(sleep_unless_cancelled(Duration::ZERO, || true));
    }

    #[test]
    fn cancellation_is_rechecked_between_slices_not_only_at_the_end() {
        let checks = AtomicUsize::new(0);
        // Long enough to require several slices; cancels on the third check.
        let cancelled = sleep_unless_cancelled(Duration::from_secs(30), || {
            checks.fetch_add(1, Ordering::SeqCst) >= 2
        });
        assert!(cancelled);
        assert!(
            checks.load(Ordering::SeqCst) < 10,
            "cancellation must be observed within a few slices, not after the interval"
        );
    }
}
