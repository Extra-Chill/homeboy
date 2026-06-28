use serde_json::json;

use crate::core::fuzz::*;

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
    let result = FuzzSequenceResult::from_value(json!({
        "schema": "homeboy/fuzz-sequence-result/v1",
        "version": 1,
        "id": "result-1",
        "status": "failed",
        "cases": [{
            "id": "case-1",
            "steps": [{
                "id": "step-1",
                "kind": "exercise",
                "observed": { "status": "failed" }
            }]
        }]
    }))
    .expect("sequence result");

    assert_eq!(result.schema, FUZZ_SEQUENCE_RESULT_SCHEMA);
    assert_eq!(result.status, "failed");
    assert_eq!(result.cases.len(), 1);
    assert_eq!(result.cases[0].steps.len(), 1);
}
