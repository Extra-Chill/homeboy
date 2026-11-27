//! Shared wall-clock timestamps. A clock before the Unix epoch is treated as
//! the epoch rather than panicking; these timestamps are not monotonic timers.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn elapsed(time: SystemTime) -> Duration {
    time.duration_since(UNIX_EPOCH).unwrap_or_default()
}

/// Current Unix time in whole seconds, or zero on a clock error.
pub fn now_secs() -> u64 {
    elapsed(SystemTime::now()).as_secs()
}

/// Current Unix time in milliseconds, or zero on a clock error.
pub fn now_ms() -> u64 {
    elapsed(SystemTime::now()).as_millis().min(u64::MAX as u128) as u64
}

/// Current UTC time in RFC 3339 format, or the Unix epoch on a clock error.
pub fn now_rfc3339() -> String {
    format_rfc3339(SystemTime::now())
}

fn format_rfc3339(time: SystemTime) -> String {
    let time = UNIX_EPOCH + elapsed(time);
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_errors_use_the_epoch() {
        let before_epoch = UNIX_EPOCH - Duration::from_secs(1);
        assert_eq!(elapsed(before_epoch), Duration::ZERO);
        assert_eq!(format_rfc3339(before_epoch), "1970-01-01T00:00:00+00:00");
    }

    #[test]
    fn units_preserve_subsecond_precision() {
        let time = UNIX_EPOCH + Duration::from_millis(1234);
        assert_eq!(elapsed(time).as_secs(), 1);
        assert_eq!(elapsed(time).as_millis(), 1234);
        assert_eq!(format_rfc3339(time), "1970-01-01T00:00:01.234+00:00");
    }

    #[test]
    fn public_clocks_agree() {
        let before = now_secs();
        let millis = now_ms();
        let date = chrono::DateTime::parse_from_rfc3339(&now_rfc3339()).unwrap();
        let after = now_secs();
        assert!((before..=after).contains(&(millis / 1000)));
        assert!((before..=after).contains(&(date.timestamp() as u64)));
    }
}
