use super::{
    bounded_failure_result, project_operator_output, RawFailureBudget, COMPACT_TEXT_LIMIT,
    RAW_FAILURE_BYTE_LIMIT, RAW_FAILURE_COUNT_LIMIT, RAW_FAILURE_JSON_DEPTH_LIMIT,
    RAW_FAILURE_PARSE_BYTE_LIMIT,
};

#[test]
fn small_stderr_is_inlined_instead_of_hashed() {
    let stderr = "x".repeat(1456);
    let mut payload = serde_json::json!({
        "runner": { "error": { "message": "Remote command failed", "stderr": stderr.clone() } }
    });

    project_operator_output(&mut payload);

    assert_eq!(payload["runner"]["error"]["stderr"], stderr);
}

#[test]
fn retains_code_less_payloads_for_every_liftable_failure_status() {
    for status in [
        "blocked",
        "error",
        "failed",
        "failure",
        "timed_out",
        "timeout",
    ] {
        let payload = serde_json::json!({
            "status": status,
            "message": "x".repeat(COMPACT_TEXT_LIMIT),
        })
        .to_string();
        assert!(payload.len() > COMPACT_TEXT_LIMIT);
        assert!(
            bounded_failure_result(&payload, &mut RawFailureBudget::default()).is_some(),
            "status={status}"
        );
    }
}

#[test]
fn retains_success_false_without_a_code() {
    let payload = serde_json::json!({
        "success": false,
        "message": "x".repeat(COMPACT_TEXT_LIMIT),
    })
    .to_string();
    assert!(bounded_failure_result(&payload, &mut RawFailureBudget::default()).is_some());
}

#[test]
fn redacts_nested_and_json_encoded_secrets() {
    let payload = serde_json::json!({
        "success": false,
        "token": "outer-secret",
        "nested": { "authorization": "Bearer inner-secret" },
        "encoded": serde_json::json!({ "api_key": "encoded-secret" }).to_string(),
    })
    .to_string();
    let retained = bounded_failure_result(&payload, &mut RawFailureBudget::default()).unwrap();
    for secret in ["outer-secret", "inner-secret", "encoded-secret"] {
        assert!(!retained.contains(secret));
    }
    assert!(retained.contains("[REDACTED]"));
}

#[test]
fn omits_json_encoded_content_at_the_recursion_limit() {
    let mut nested = serde_json::json!({
        "encoded": serde_json::json!({ "token": "depth-secret" }).to_string(),
    });
    for _ in 0..RAW_FAILURE_JSON_DEPTH_LIMIT {
        nested = serde_json::json!({ "nested": nested });
    }
    let payload = serde_json::json!({
        "status": "failed",
        "details": nested,
    })
    .to_string();
    let retained = bounded_failure_result(&payload, &mut RawFailureBudget::default()).unwrap();

    assert!(!retained.contains("depth-secret"));
    assert!(retained.contains("omitted JSON beyond redaction depth"));
}

#[test]
fn omits_json_encoded_content_that_exceeds_the_parser_recursion_limit() {
    let encoded = format!(
        "{}{{\"token\":\"parser-depth-secret\"}}{}",
        "[".repeat(256),
        "]".repeat(256)
    );
    let payload = serde_json::json!({
        "status": "failed",
        "encoded": encoded,
    })
    .to_string();
    let retained = bounded_failure_result(&payload, &mut RawFailureBudget::default()).unwrap();

    assert!(!retained.contains("parser-depth-secret"));
    assert!(retained.contains("omitted invalid encoded JSON"));
}

#[test]
fn enforces_one_count_and_byte_budget_across_many_payloads() {
    let payload = serde_json::json!({
        "status": "failed",
        "message": "x".repeat(RAW_FAILURE_BYTE_LIMIT / RAW_FAILURE_COUNT_LIMIT),
    })
    .to_string();
    let mut budget = RawFailureBudget::default();
    let retained = (0..RAW_FAILURE_COUNT_LIMIT * 2)
        .filter(|_| bounded_failure_result(&payload, &mut budget).is_some())
        .count();
    assert!(retained <= RAW_FAILURE_COUNT_LIMIT);
    assert!(budget.bytes <= RAW_FAILURE_BYTE_LIMIT);
}

#[test]
fn omits_oversized_failure_json_before_parsing() {
    let payload = serde_json::json!({
        "status": "failed",
        "message": "x".repeat(RAW_FAILURE_PARSE_BYTE_LIMIT),
    })
    .to_string();

    assert!(bounded_failure_result(&payload, &mut RawFailureBudget::default()).is_none());
}

#[test]
fn operator_projection_redacts_and_bounds_recursive_failure_payloads_globally() {
    let original = serde_json::json!({
        "attempts": (0..RAW_FAILURE_COUNT_LIMIT * 3).map(|index| serde_json::json!({
            "content": {
                "body": serde_json::json!({
                    "status": "failed",
                    "token": format!("secret-{index}"),
                    "message": "x".repeat(700),
                }).to_string()
            },
            "kind": "executor_result",
            "uri": format!("file:///tmp/{index}"),
            "status": "available",
        })).collect::<Vec<_>>()
    });
    let mut projected = original.clone();
    project_operator_output(&mut projected);
    let text = projected.to_string();

    assert_eq!(
        original["attempts"][0]["content"]["body"]
            .as_str()
            .is_some(),
        true
    );
    assert!(!text.contains("secret-"));
    assert!(text.matches("[omitted ").count() >= RAW_FAILURE_COUNT_LIMIT);
    assert!(text.matches("[REDACTED]").count() <= RAW_FAILURE_COUNT_LIMIT);
    assert!(text.len() < original.to_string().len());
}
