use super::*;

#[test]
fn bench_output_single_serializes_with_tagged_payload() {
    let single = BenchCommandOutput {
        passed: true,
        status: "passed".to_string(),
        component: "studio".to_string(),
        exit_code: 0,
        iterations: 10,
        artifacts: Vec::new(),
        results: None,
        budget_findings: Vec::new(),
        gate_results: Vec::new(),
        gate_failures: Vec::new(),
        baseline_comparison: None,
        hints: None,
        rig_state: None,
        failure: None,
        diagnostics: Vec::new(),
        ci_context: None,
        persisted_run: None,
    };
    let value = serde_json::to_value(BenchOutput::Single(single)).unwrap();
    assert_eq!(
        value.get("variant"),
        Some(&serde_json::Value::String("single".to_string()))
    );
    let payload = value
        .get("payload")
        .expect("single bench output has payload");
    assert_eq!(payload.get("passed"), Some(&serde_json::Value::Bool(true)));
    assert_eq!(
        payload.get("component"),
        Some(&serde_json::Value::String("studio".to_string()))
    );
}

#[test]
fn bench_output_comparison_serializes_with_tagged_payload() {
    let (cmp, _) = aggregate_comparison("studio".to_string(), 10, Vec::new());
    let value = serde_json::to_value(BenchOutput::Comparison(cmp)).unwrap();
    assert_eq!(
        value.get("variant"),
        Some(&serde_json::Value::String("comparison".to_string()))
    );
    assert_eq!(
        value
            .get("payload")
            .and_then(|payload| payload.get("comparison")),
        Some(&serde_json::Value::String("cross_rig".to_string()))
    );
}
