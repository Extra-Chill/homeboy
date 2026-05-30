use homeboy::core::redaction::{redact_json, redact_string, redact_url, RedactionPolicy};
use serde_json::json;

#[test]
fn redaction_public_api_supports_downstream_evidence_hygiene() {
    let policy = RedactionPolicy::new()
        .with_sensitive_key("tenant")
        .with_sensitive_header("x-private-token")
        .with_replacement("***");

    assert!(policy.is_sensitive_key("tenant_id"));
    assert!(policy.is_sensitive_header("X-Private-Token"));
    assert_eq!(policy.replacement(), "***");
    assert_eq!(
        policy.redact_string("Authorization: Bearer abc tenant=acme"),
        "Authorization: Bearer *** tenant=***"
    );
    assert_eq!(
        policy.redact_url("/path?tenant=acme&ok=1"),
        "/path?tenant=***&ok=1"
    );
    assert_eq!(
        policy.redact_json(&json!({ "x-private-token": "abc" })),
        json!({ "x-private-token": "***" })
    );
}

#[test]
fn redaction_free_functions_use_default_policy() {
    assert_eq!(redact_string("token=abc"), "token=[REDACTED]");
    assert_eq!(
        redact_url("/path?nonce=abc&ok=1"),
        "/path?nonce=[REDACTED]&ok=1"
    );
    assert_eq!(
        redact_json(&json!({ "password": "secret" })),
        json!({ "password": "[REDACTED]" })
    );
}
