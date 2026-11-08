//! Bounded diagnostics for JSON received across Lab transport boundaries.
use crate::{Error, Result};
use serde::de::DeserializeOwned;

const EXCERPT_BYTES: usize = 1024;

/// Redact before clipping so truncation cannot hide a secret's identifying prefix.
pub fn bounded_redacted(value: &str, max_chars: usize) -> String {
    crate::redaction::redact_string(value)
        .chars()
        .take(max_chars)
        .collect()
}

fn excerpt(bytes: &[u8]) -> String {
    let text = crate::redaction::redact_string(&String::from_utf8_lossy(bytes));
    if text.len() <= EXCERPT_BYTES {
        return text;
    }
    let mut head = EXCERPT_BYTES / 2 - 2;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - (EXCERPT_BYTES / 2 - 3);
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{} ... {}", &text[..head], &text[tail..])
}

/// Decode a transport payload without losing the stage or serde's error location.
pub fn decode<T: DeserializeOwned>(stage: &str, payload: &[u8]) -> Result<T> {
    serde_json::from_slice(payload).map_err(|source| {
        let category = format!("{:?}", source.classify()).to_lowercase();
        let mut error = Error::internal_json(
            bounded_redacted(&source.to_string(), 256),
            Some(stage.to_string()),
        );
        error.details["stage"] = serde_json::json!(stage);
        error.details["line"] = serde_json::json!(source.line());
        error.details["column"] = serde_json::json!(source.column());
        error.details["category"] = serde_json::json!(category);
        error.details["payload_bytes"] = serde_json::json!(payload.len());
        error.details["payload_excerpt"] = serde_json::json!(excerpt(payload));
        error
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_json_decode_diagnostics() {
        let cases = [
            ("malformed", "{\n  bad}".to_string(), "syntax"),
            ("utf8", format!("{}END", "界".repeat(600)), "syntax"),
            ("long", format!("HEAD{}TAIL", "x".repeat(4000)), "syntax"),
            (
                "secret",
                "https://user:password@example.com".into(),
                "syntax",
            ),
        ];
        for (stage, payload, category) in cases {
            let source =
                serde_json::from_slice::<serde_json::Value>(payload.as_bytes()).unwrap_err();
            let error = decode::<serde_json::Value>(stage, payload.as_bytes()).unwrap_err();
            assert_eq!(error.details["stage"], stage);
            assert_eq!(error.details["line"], source.line());
            assert_eq!(error.details["column"], source.column());
            assert_eq!(error.details["category"], category);
            let excerpt = error.details["payload_excerpt"].as_str().unwrap();
            assert!(excerpt.len() <= EXCERPT_BYTES);
            assert!(!excerpt.contains('�'));
            assert!(!excerpt.contains("password"));
            if stage == "long" {
                assert!(excerpt.starts_with("HEAD"));
                assert!(excerpt.ends_with("TAIL"));
            }
        }
    }
}
