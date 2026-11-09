//! Timestamp formatting shared by operator-facing diagnostics.

/// Format Unix seconds as an RFC 3339 timestamp, or `None` when out of range.
pub fn unix_seconds_rfc3339(seconds: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(seconds, 0).map(|time| time.to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_unix_seconds() {
        for (seconds, expected) in [
            (1700000000, Some("2023-11-14T22:13:20+00:00")),
            (0, Some("1970-01-01T00:00:00+00:00")),
            (i64::MAX, None),
        ] {
            assert_eq!(unix_seconds_rfc3339(seconds).as_deref(), expected);
        }
    }
}
