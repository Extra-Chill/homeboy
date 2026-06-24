use serde_json::json;

use crate::core::fuzz::{
    parse_fuzz_observation_set_value, FuzzObservationFamily, FuzzObservationSet,
    FUZZ_OBSERVATION_SET_SCHEMA,
};

#[test]
fn parses_generic_fuzz_observation_set() {
    let set = FuzzObservationSet::from_value(json!({
        "schema": FUZZ_OBSERVATION_SET_SCHEMA,
        "version": 1,
        "id": "db-api-observations",
        "observations": [
            {
                "id": "case-1-query-count",
                "family": "query",
                "case_id": "case-1",
                "target_id": "rest.products",
                "operation_id": "rest.products.read",
                "phase": "execute",
                "subject": "SELECT wp_posts",
                "metric": "query_count",
                "value": 12,
                "unit": "count",
                "fingerprint": "select-wp-posts",
                "sample_count": 1
            }
        ]
    }))
    .expect("observation set");

    assert_eq!(set.id, "db-api-observations");
    assert_eq!(set.observations[0].family, FuzzObservationFamily::Query);
    assert_eq!(set.observations[0].metric, "query_count");
}

#[test]
fn rejects_invalid_observation_values() {
    let invalid = json!({
        "schema": FUZZ_OBSERVATION_SET_SCHEMA,
        "version": 1,
        "id": "bad-observations",
        "observations": [
            {
                "id": "bad-value",
                "family": "timing",
                "subject": "request",
                "metric": "elapsed_ms",
                "value": "not-a-number",
                "unit": "ms"
            }
        ]
    });

    assert!(FuzzObservationSet::from_value(invalid).is_err());
}

#[test]
fn finds_observation_set_in_result_metadata() {
    let envelope = json!({
        "metadata": {
            "observation_set": {
                "schema": FUZZ_OBSERVATION_SET_SCHEMA,
                "version": 1,
                "id": "observations",
                "observations": []
            }
        }
    });

    assert!(parse_fuzz_observation_set_value(&envelope["metadata"]).is_some());
}
