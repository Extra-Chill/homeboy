use serde_json::json;

use crate::*;

#[test]
fn sequence_plan_normalizes_generic_cases_and_steps() {
    let plan = FuzzSequencePlan::from_value(json!({
        "schema": "homeboy/fuzz-sequence-plan/v1",
        "version": 1,
        "id": " plan-1 ",
        "cases": [{
            "id": " case-1 ",
            "target_id": " target-1 ",
            "operation_id": " operation-1 ",
            "steps": [{
                "id": " step-1 ",
                "kind": " exercise ",
                "operation_id": " operation-1 ",
                "input": { "value": 1 }
            }]
        }]
    }))
    .expect("sequence plan");

    assert_eq!(plan.schema, FUZZ_SEQUENCE_PLAN_SCHEMA);
    assert_eq!(plan.id, "plan-1");
    assert_eq!(plan.cases[0].id, "case-1");
    assert_eq!(plan.cases[0].steps[0].id, "step-1");
    assert_eq!(plan.cases[0].steps[0].kind, "exercise");
}

#[test]
fn sequence_result_requires_generic_status() {
    let result = |status: &str| {
        FuzzSequenceResult::from_value(json!({
            "schema": "homeboy/fuzz-sequence-result/v1",
            "version": 1,
            "id": "result-1",
            "status": status,
            "cases": [{
                "id": "case-1",
                "steps": [{ "id": "step-1", "kind": "exercise" }]
            }]
        }))
    };

    let parsed = result(" failed ").expect("sequence result");
    assert_eq!(parsed.status, "failed");
    assert_eq!(parsed.cases[0].steps.len(), 1);

    let error = result(" ").expect_err("blank status must fail");
    assert!(error.contains("sequence_result.status"), "{error}");
}
