use std::time::Duration;

const MAX_TIMEOUT_GRACE_MS: u64 = 30_000;
const MIN_TIMEOUT_GRACE_MS: u64 = 100;

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
