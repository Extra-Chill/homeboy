use serde_json::json;

use crate::{
    parse_fuzz_observation_set_value, FuzzObservationFamily, FuzzObservationSet,
    FUZZ_OBSERVATION_SET_SCHEMA,
};

#[test]
fn finds_and_parses_observation_set_in_result_metadata() {
    let metadata = json!({
        "observation_set": {
            "schema": FUZZ_OBSERVATION_SET_SCHEMA,
            "version": 1,
            "id": "db-api-observations",
            "observations": [
                {
                    "id": "case-1-query-count",
                    "family": "query",
                    "case_id": "case-1",
                    "subject": "SELECT wp_posts",
                    "metric": "query_count",
                    "value": 12,
                    "unit": "count"
                }
            ]
        }
    });

    let set = parse_fuzz_observation_set_value(&metadata).expect("observation set");

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
