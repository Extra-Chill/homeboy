//! One duration parser for every command surface.
//!
//! `500ms`, `30s`, `5m`, `2h`, `7d` and the long spellings of each unit. A bare
//! number is rejected because it is ambiguous, zero is rejected because it
//! turns a poll interval into a spin, and overflow is reported rather than
//! panicking in debug builds.

use std::time::Duration;

pub const DURATION_UNITS: &[(&str, u64)] = &[
    ("ms", 1),
    ("s", 1_000),
    ("sec", 1_000),
    ("secs", 1_000),
    ("second", 1_000),
    ("seconds", 1_000),
    ("m", 60 * 1_000),
    ("min", 60 * 1_000),
    ("mins", 60 * 1_000),
    ("minute", 60 * 1_000),
    ("minutes", 60 * 1_000),
    ("h", 60 * 60 * 1_000),
    ("hr", 60 * 60 * 1_000),
    ("hrs", 60 * 60 * 1_000),
    ("hour", 60 * 60 * 1_000),
    ("hours", 60 * 60 * 1_000),
    ("d", 24 * 60 * 60 * 1_000),
    ("day", 24 * 60 * 60 * 1_000),
    ("days", 24 * 60 * 60 * 1_000),
];

/// Human-readable list of accepted units, for `--help` and error messages.
pub const DURATION_UNITS_HINT: &str = "ms, s, m, h, or d";

/// Parse a duration like `500ms`, `30s`, `5m`, `2h`, or `7d`.
///
/// Returns the plain message on failure; [`parse_duration`] wraps it into a
/// structured argument error and [`parse_duration_arg`] hands it to clap.
///
/// A bare number with no unit is rejected, as it was by all four parsers this
/// replaces -- `--timeout 30` is ambiguous and always was an error.
pub fn parse_duration_parts(raw: &str) -> Result<Duration, String> {
    let trimmed = raw.trim();
    let split = trimmed
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(trimmed.len());
    let (amount, unit) = trimmed.split_at(split);

    if amount.is_empty() || unit.is_empty() {
        return Err(format!(
            "expected duration like 500ms, 30s, 5m, 2h, or 7d (unit required: {DURATION_UNITS_HINT})"
        ));
    }

    let amount = amount
        .parse::<u64>()
        .map_err(|_| "duration amount must be a positive integer".to_string())?;

    // Preserved from three of the four parsers. `activity` was the one that
    // lacked it, so `activity --interval 0s` used to spin with no delay.
    if amount == 0 {
        return Err("duration amount must be greater than zero".to_string());
    }

    let millis_per_unit = DURATION_UNITS
        .iter()
        .find(|(name, _)| *name == unit)
        .map(|(_, millis)| *millis)
        .ok_or_else(|| format!("duration unit must be one of {DURATION_UNITS_HINT}"))?;

    // The parsers this replaces multiplied unchecked, so `9999999999999999999d`
    // panicked in debug builds. Report it instead.
    amount
        .checked_mul(millis_per_unit)
        .map(Duration::from_millis)
        .ok_or_else(|| "duration is too large".to_string())
}

/// Parse a duration into a structured argument error attributed to `field`.
///
/// `field` is the name the user typed (`since`, `duration`, `--timeout`) so the
/// error points at the flag that was wrong.
pub fn parse_duration(field: &str, raw: &str) -> crate::error::Result<Duration> {
    parse_duration_parts(raw).map_err(|message| {
        crate::error::Error::validation_invalid_argument(
            field,
            message,
            Some(raw.to_string()),
            None,
        )
    })
}
