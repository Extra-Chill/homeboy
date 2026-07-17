use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_TIMEOUT_GRACE_MS: u64 = 30_000;
const MIN_TIMEOUT_GRACE_MS: u64 = 100;

/// Default provider wall-clock timeout for agent-task execution when neither the
/// task nor the plan sets an explicit timeout. Twenty minutes is generous for
/// real agent work while still preventing silent unbounded provider hangs.
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
