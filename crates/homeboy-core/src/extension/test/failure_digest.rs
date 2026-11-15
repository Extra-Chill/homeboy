//! Bounded, line-oriented digest of failed tests (#18130).
//!
//! A failed test run's structured JSON can be huge: the runtime identity
//! evidence lists every executed test id, and the JSON keys are sorted, so
//! that list lands at the end of stdout. A consumer that keeps only a bounded
//! tail of stdout (a deterministic gate keeps 64 KiB) then sees passed ids
//! and never the failure. This digest is printed to stderr after the run, one
//! self-describing line per failed test, so the failing ids and a bounded
//! excerpt of their failure output survive any tail cap. Gates parse it back
//! with [`parse_test_failure_digest`].

use serde::{Deserialize, Serialize};

use super::TestAnalysisInput;

/// Prefix of the header line announcing how many tests failed.
pub const TEST_FAILURES_HEADER_PREFIX: &str = "[test-failures]";
/// Prefix of one failed-test line; the remainder is a compact JSON object.
pub const TEST_FAILURE_LINE_PREFIX: &str = "[test-failure] ";
/// Maximum failed tests listed individually.
pub const TEST_FAILURE_DIGEST_MAX_TESTS: usize = 20;
/// Maximum bytes of a failed test's message.
pub const TEST_FAILURE_MESSAGE_MAX_BYTES: usize = 600;
/// Maximum bytes of a failed test's captured output excerpt.
pub const TEST_FAILURE_OUTPUT_MAX_BYTES: usize = 1200;

/// One failed test as carried by the digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TestFailureDigestEntry {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// Tail of the test's own captured stdout/stderr (panic output, traces).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
}

/// The digest lines for a failed run, or an empty list when no failure was
/// parsed. Every line is bounded and redacted.
pub fn render_test_failure_digest(input: &TestAnalysisInput) -> Vec<String> {
    let failures: Vec<_> = input
        .failures
        .iter()
        .filter(|failure| !failure.test_name.trim().is_empty())
        .collect();
    if failures.is_empty() {
        return Vec::new();
    }
    let shown = failures.len().min(TEST_FAILURE_DIGEST_MAX_TESTS);
    let mut lines = vec![if shown < failures.len() {
        format!(
            "{TEST_FAILURES_HEADER_PREFIX} {} failed test(s); showing the first {shown}",
            failures.len()
        )
    } else {
        format!(
            "{TEST_FAILURES_HEADER_PREFIX} {} failed test(s)",
            failures.len()
        )
    }];
    for failure in failures.into_iter().take(shown) {
        let output = [failure.stdout_excerpt.trim(), failure.stderr_excerpt.trim()]
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let entry = TestFailureDigestEntry {
            id: redact(failure.test_name.trim()),
            file: redact(failure.test_file.trim()),
            message: bound_head(
                &redact(failure.message.trim()),
                TEST_FAILURE_MESSAGE_MAX_BYTES,
            ),
            output: bound_tail(&redact(&output), TEST_FAILURE_OUTPUT_MAX_BYTES),
        };
        if let Ok(json) = serde_json::to_string(&entry) {
            lines.push(format!("{TEST_FAILURE_LINE_PREFIX}{json}"));
        }
    }
    lines
}

/// Failed tests recovered from digest lines anywhere in `text`, de-duplicated
/// by id in first-seen order and capped at [`TEST_FAILURE_DIGEST_MAX_TESTS`].
pub fn parse_test_failure_digest(text: &str) -> Vec<TestFailureDigestEntry> {
    let mut entries: Vec<TestFailureDigestEntry> = Vec::new();
    for line in text.lines() {
        let Some(index) = line.find(TEST_FAILURE_LINE_PREFIX) else {
            continue;
        };
        let json = &line[index + TEST_FAILURE_LINE_PREFIX.len()..];
        let Ok(mut entry) = serde_json::from_str::<TestFailureDigestEntry>(json.trim()) else {
            continue;
        };
        entry.id = entry.id.trim().to_string();
        if entry.id.is_empty() || entries.iter().any(|seen| seen.id == entry.id) {
            continue;
        }
        entry.message = bound_head(&entry.message, TEST_FAILURE_MESSAGE_MAX_BYTES);
        entry.output = bound_tail(&entry.output, TEST_FAILURE_OUTPUT_MAX_BYTES);
        entries.push(entry);
        if entries.len() == TEST_FAILURE_DIGEST_MAX_TESTS {
            break;
        }
    }
    entries
}

fn redact(text: &str) -> String {
    homeboy_core::redaction::redact_string(text)
}

fn bound_head(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn bound_tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::test::TestFailure;

    fn failure(name: &str, message: &str, stdout: &str) -> TestFailure {
        TestFailure {
            test_name: name.to_string(),
            test_file: "src/lib.rs".to_string(),
            error_type: "panic".to_string(),
            message: message.to_string(),
            stdout_excerpt: stdout.to_string(),
            stderr_excerpt: String::new(),
            source_file: String::new(),
            source_line: 0,
        }
    }

    #[test]
    fn test_failure_digest_round_trips_ids_and_messages() {
        let input = TestAnalysisInput {
            failures: vec![failure(
                "homeboy-agents::gate::tests::keeps failing id",
                "assertion `left == right` failed\n  left: 1\n right: 2",
                "thread 'x' panicked at src/gate.rs:10:5",
            )],
            total: 14281,
            passed: 14255,
        };
        let lines = render_test_failure_digest(&input);
        assert_eq!(lines[0], "[test-failures] 1 failed test(s)");
        assert!(lines.iter().all(|line| !line.contains('\n')));
        let parsed = parse_test_failure_digest(&format!("noise\n{}\nmore", lines.join("\n")));
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].id,
            "homeboy-agents::gate::tests::keeps failing id"
        );
        assert!(parsed[0].message.contains("left: 1"));
        assert!(parsed[0].output.contains("panicked at src/gate.rs:10:5"));
    }

    #[test]
    fn test_failure_digest_bounds_entries_and_bytes() {
        let input = TestAnalysisInput {
            failures: (0..50)
                .map(|index| failure(&format!("t::{index}"), &"m".repeat(5000), &"o".repeat(5000)))
                .collect(),
            total: 50,
            passed: 0,
        };
        let lines = render_test_failure_digest(&input);
        assert_eq!(lines.len(), TEST_FAILURE_DIGEST_MAX_TESTS + 1);
        assert!(lines[0].contains("50 failed test(s); showing the first 20"));
        assert!(lines.iter().all(|line| line.len() < 2_200));
        assert!(render_test_failure_digest(&TestAnalysisInput {
            failures: Vec::new(),
            total: 1,
            passed: 1
        })
        .is_empty());
    }
}
