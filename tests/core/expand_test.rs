//! Tests for shared token expansion helpers.

use crate::expand::{expand_tokens, expand_with_tilde};

#[test]
fn test_expand_tokens() {
    let out = expand_tokens("/a/${known}/b/${unknown}", |token| {
        (token == "known").then(|| "value".to_string())
    });

    assert_eq!(out, "/a/value/b/${unknown}");
}

#[test]
fn test_expand_with_tilde() {
    let home = std::env::var("HOME").unwrap_or_default();
    let out = expand_with_tilde("~/${name}", |token| {
        (token == "name").then(|| "repo".to_string())
    });

    assert!(out.starts_with(&home));
    assert!(out.ends_with("/repo"));
}

#[test]
fn unknown_and_unterminated_tokens_stay_literal() {
    assert_eq!(expand_tokens("${unknown}", |_| None), "${unknown}");
    assert_eq!(expand_tokens("${unterminated", |_| None), "${unterminated");
}
