//! Normalized TEST-FAILURE record shaping behind `emit-test-failure.sh`.

use super::json::Json;
use homeboy_engine_primitives::content_hash::sha256_hex;

const EXCERPT_LINES: usize = 40;
const EXCERPT_CHARS: usize = 4000;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TestFailureInput {
    pub test_id: String,
    pub suite: String,
    pub file: String,
    pub line: String,
    pub message: String,
    pub failure_type: String,
    /// Explicit fingerprint identity. `None` derives the canonical identity.
    pub identity: Option<String>,
    pub stdout_excerpt: String,
    pub stderr_excerpt: String,
}

/// Build the record with its fixed key order:
/// test_id, suite, file, line, message, failure_type, fingerprint,
/// stdout_excerpt, stderr_excerpt.
pub fn record(input: &TestFailureInput) -> Json {
    let failure_type = if input.failure_type.is_empty() {
        "test_failure"
    } else {
        input.failure_type.as_str()
    };
    let line = input.line.trim().parse::<i64>().ok();
    let identity = match &input.identity {
        Some(identity) => identity.clone(),
        None => {
            // WordPress make_failure_fingerprint: NUL-joined identity tuple.
            let first_message_line = input.message.split('\n').next().unwrap_or_default();
            let line_part = line.map(|line| line.to_string()).unwrap_or_default();
            [
                input.test_id.as_str(),
                input.file.as_str(),
                line_part.as_str(),
                failure_type,
                first_message_line,
            ]
            .join("\0")
        }
    };
    let optional = |value: &str| {
        if value.is_empty() {
            Json::Null
        } else {
            Json::string(value)
        }
    };
    Json::Object(vec![
        ("test_id".into(), Json::string(&input.test_id)),
        ("suite".into(), optional(&input.suite)),
        ("file".into(), optional(&input.file)),
        ("line".into(), line.map_or(Json::Null, Json::int)),
        ("message".into(), Json::string(&input.message)),
        ("failure_type".into(), Json::string(failure_type)),
        (
            "fingerprint".into(),
            Json::String(sha256_hex(identity.as_bytes())),
        ),
        ("stdout_excerpt".into(), Json::string(&input.stdout_excerpt)),
        ("stderr_excerpt".into(), Json::string(&input.stderr_excerpt)),
    ])
}

/// Canonical sidecar excerpt bound (WordPress make_output_excerpt): first 40
/// lines, surrounding whitespace trimmed, capped at 4000 characters with a
/// trailing ellipsis.
pub fn bound_excerpt(raw: &str) -> String {
    let head = raw
        .split('\n')
        .take(EXCERPT_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    // The \x1c-\x1f information separators also count as trimmable whitespace.
    let excerpt =
        head.trim_matches(|ch: char| ch.is_whitespace() || ('\x1c'..='\x1f').contains(&ch));
    if excerpt.chars().count() > EXCERPT_CHARS {
        let mut capped = excerpt.chars().take(EXCERPT_CHARS - 3).collect::<String>();
        capped.push_str("...");
        capped
    } else {
        excerpt.to_string()
    }
}
