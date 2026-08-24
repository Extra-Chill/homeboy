//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::shared::*;

mod cancellation_tests {
    use super::*;

    #[test]
    fn cancellation_token_callbacks_fire_once_and_after_existing_cancel() {
        let token = AgentTaskCancellationToken::default();
        let callback_count = Arc::new(AtomicUsize::new(0));
        let callback_count_for_token = Arc::clone(&callback_count);
        token.on_cancel(Arc::new(move || {
            callback_count_for_token.fetch_add(1, Ordering::SeqCst);
        }));

        token.cancel();
        token.cancel();

        assert_eq!(callback_count.load(Ordering::SeqCst), 1);

        let immediate_count = Arc::new(AtomicUsize::new(0));
        let immediate_count_for_token = Arc::clone(&immediate_count);
        token.on_cancel(Arc::new(move || {
            immediate_count_for_token.fetch_add(1, Ordering::SeqCst);
        }));

        assert_eq!(immediate_count.load(Ordering::SeqCst), 1);
    }
}
