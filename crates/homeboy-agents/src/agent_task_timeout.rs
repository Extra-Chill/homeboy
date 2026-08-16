use std::cell::RefCell;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_TIMEOUT_GRACE_MS: u64 = 30_000;
const MIN_TIMEOUT_GRACE_MS: u64 = 100;

/// Default provider wall-clock timeout for agent-task execution when neither the
/// task nor the plan sets an explicit timeout. Twenty minutes is generous for
/// real agent work while still preventing silent unbounded provider hangs.
///
/// The `--timeout-ms` help text names this value literally so an operator can
/// size a task against the budget without spending a run to discover it
/// (#12568). Changing it here means changing that help text too.
pub const DEFAULT_PROVIDER_TIMEOUT_MS: u64 = 1_200_000;

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub(crate) fn remaining_execution_deadline_ms(deadline_unix_ms: Option<u64>) -> Option<u64> {
    deadline_unix_ms.map(|deadline| deadline.saturating_sub(now_unix_ms()))
}

pub fn effective_provider_timeout_ms(timeout_ms: Option<u64>, max_runtime_ms: Option<u64>) -> u64 {
    timeout_ms
        .or(max_runtime_ms)
        .unwrap_or_else(default_provider_timeout_ms)
}

fn default_provider_timeout_ms() -> u64 {
    #[cfg(test)]
    if let Ok(value) = std::env::var("HOMEBOY_AGENT_TASK_TEST_DEFAULT_PROVIDER_TIMEOUT_MS") {
        if let Ok(timeout_ms) = value.parse::<u64>() {
            return timeout_ms;
        }
    }

    DEFAULT_PROVIDER_TIMEOUT_MS
}

thread_local! {
    static CURRENT_COOK_DEADLINE: RefCell<Option<CookDeadline>> = const { RefCell::new(None) };
}

/// An absolute wall-clock budget for a whole Cook, spanning every attempt and
/// every gate rather than any single provider execution or gate command.
///
/// # Why this is separate from the existing timeouts
///
/// Before this existed the only bounds were per-provider-execution
/// ([`DEFAULT_PROVIDER_TIMEOUT_MS`], 20 minutes) and per-gate
/// (`--gate-timeout-seconds`, 30 minutes). Those bound the *parts*, and the
/// parts multiply: a Cook with `--max-attempts 3` and five gates has a legal
/// upper bound near `3 x (20min + 5 x 30min)`, roughly eight and a half hours,
/// with nothing that stops it — multiplied again by the children of a fanout.
/// A budget over the whole is not derivable from budgets over the parts, so it
/// has to be stated.
///
/// # Absolute, not relative
///
/// The budget is stored as an absolute instant, resolved once by whoever set
/// it. A duration re-based at each attempt would grant the full budget again
/// on every retry, which is the bound this exists to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CookDeadline {
    deadline_unix_ms: u64,
}

impl CookDeadline {
    /// A budget expiring `max_duration_seconds` from now.
    pub fn from_duration_seconds(max_duration_seconds: u64) -> Self {
        Self {
            deadline_unix_ms: now_unix_ms().saturating_add(max_duration_seconds * 1_000),
        }
    }

    pub fn from_unix_ms(deadline_unix_ms: u64) -> Self {
        Self { deadline_unix_ms }
    }

    pub fn deadline_unix_ms(&self) -> u64 {
        self.deadline_unix_ms
    }

    /// Milliseconds left, saturating at zero.
    pub fn remaining_ms(&self) -> u64 {
        self.deadline_unix_ms.saturating_sub(now_unix_ms())
    }

    pub fn is_expired(&self) -> bool {
        self.remaining_ms() == 0
    }

    /// The operator-facing explanation recorded as a Cook's stop reason.
    ///
    /// Names the boundary the budget was noticed at, because "the Cook ran out
    /// of time" and "the Cook ran out of time before its gates could run" call
    /// for different follow-ups.
    pub fn stop_reason(&self, boundary: &str) -> String {
        format!(
            "Cook exceeded its wall-clock budget (--max-duration): the deadline at unix_ms {} \
             passed before {boundary}. No further attempt or gate was started. Re-run with a \
             larger --max-duration, or reduce --max-attempts or the number of gates.",
            self.deadline_unix_ms,
        )
    }
}

/// A deadline captured for propagation onto worker threads.
///
/// Batch workers are separate threads, and a thread-local does not cross that
/// boundary on its own. This mirrors how the notification route is captured
/// and re-bound per worker in the same batch runner.
#[derive(Debug, Clone, Copy)]
pub struct PropagatedCookDeadline(Option<CookDeadline>);

impl PropagatedCookDeadline {
    pub fn bind<T>(&self, operation: impl FnOnce() -> T) -> T {
        with_current_cook_deadline(self.0, operation)
    }

    pub fn deadline(&self) -> Option<CookDeadline> {
        self.0
    }
}

/// Capture the calling thread's deadline for propagation onto worker threads.
pub fn capture_cook_deadline() -> PropagatedCookDeadline {
    PropagatedCookDeadline(current_cook_deadline())
}

/// The deadline governing the calling thread, if any.
///
/// `None` means unbudgeted, which is the pre-existing behaviour and remains
/// the default for every caller that does not set one.
pub fn current_cook_deadline() -> Option<CookDeadline> {
    CURRENT_COOK_DEADLINE.with(|current| *current.borrow())
}

/// Run `operation` with `deadline` governing this thread, restoring the
/// previous value afterwards even on unwind.
pub fn with_current_cook_deadline<T>(
    deadline: Option<CookDeadline>,
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<CookDeadline>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT_COOK_DEADLINE.with(|current| *current.borrow_mut() = self.0);
        }
    }

    let previous = CURRENT_COOK_DEADLINE.with(|current| current.replace(deadline));
    let _restore = Restore(previous);
    operation()
}

/// The expired deadline governing this thread, if it has already passed.
///
/// Callers at a boundary use this to decide whether to stop: `Some` means the
/// budget is spent and no further work may be started.
pub fn expired_cook_deadline() -> Option<CookDeadline> {
    current_cook_deadline().filter(CookDeadline::is_expired)
}

pub(crate) fn timeout_with_grace(timeout_ms: u64) -> Duration {
    Duration::from_millis(timeout_ms.saturating_add(timeout_grace_ms(timeout_ms)))
}

fn timeout_grace_ms(timeout_ms: u64) -> u64 {
    (timeout_ms / 10)
        .clamp(MIN_TIMEOUT_GRACE_MS, MAX_TIMEOUT_GRACE_MS)
        .min(MAX_TIMEOUT_GRACE_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default has to stay "no budget", or every existing caller that
    /// never asked for one silently acquires a way to be killed.
    #[test]
    fn no_deadline_is_bound_by_default() {
        assert_eq!(current_cook_deadline(), None);
        assert_eq!(expired_cook_deadline(), None);
    }

    #[test]
    fn a_past_deadline_is_expired_and_a_future_one_is_not() {
        let past = CookDeadline::from_unix_ms(now_unix_ms().saturating_sub(1));
        assert!(past.is_expired());
        assert_eq!(past.remaining_ms(), 0);

        let future = CookDeadline::from_duration_seconds(3_600);
        assert!(!future.is_expired());
        assert!(future.remaining_ms() > 0);
    }

    #[test]
    fn a_bound_deadline_is_visible_and_is_restored_after_the_scope() {
        let deadline = CookDeadline::from_duration_seconds(3_600);
        with_current_cook_deadline(Some(deadline), || {
            assert_eq!(current_cook_deadline(), Some(deadline));
            // An expired check must not fire on a live budget.
            assert_eq!(expired_cook_deadline(), None);
        });
        assert_eq!(current_cook_deadline(), None, "scope must not leak");
    }

    #[test]
    fn an_expired_deadline_is_reported_only_once_it_has_passed() {
        let expired = CookDeadline::from_unix_ms(now_unix_ms().saturating_sub(1));
        with_current_cook_deadline(Some(expired), || {
            assert_eq!(expired_cook_deadline(), Some(expired));
        });
    }

    /// Batch children run on worker threads, so a budget that does not cross
    /// the thread boundary bounds only the coordinator and nothing it started.
    #[test]
    fn a_captured_deadline_crosses_a_thread_boundary() {
        let deadline = CookDeadline::from_duration_seconds(3_600);
        with_current_cook_deadline(Some(deadline), || {
            let propagated = capture_cook_deadline();
            assert_eq!(propagated.deadline(), Some(deadline));
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    assert_eq!(
                        current_cook_deadline(),
                        None,
                        "a fresh thread starts unbudgeted"
                    );
                    propagated.bind(|| {
                        assert_eq!(current_cook_deadline(), Some(deadline));
                    });
                });
            });
        });
    }

    #[test]
    fn the_stop_reason_names_the_boundary_and_the_flag() {
        let reason = CookDeadline::from_unix_ms(1_234).stop_reason("attempt 2 started");
        assert!(reason.contains("--max-duration"), "{reason}");
        assert!(reason.contains("attempt 2 started"), "{reason}");
        assert!(reason.contains("1234"), "{reason}");
    }

    #[test]
    fn timeout_grace_is_bounded() {
        assert_eq!(timeout_with_grace(50), Duration::from_millis(150));
        assert_eq!(timeout_with_grace(1_000), Duration::from_millis(1_100));
        assert_eq!(
            timeout_with_grace(1_800_000),
            Duration::from_millis(1_830_000)
        );
    }
}
