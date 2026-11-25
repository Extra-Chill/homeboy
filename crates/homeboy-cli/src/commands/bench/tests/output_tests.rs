use super::*;

#[test]
fn bench_output_serializes_as_tagged_variant_and_payload() {
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
        actionable: None,
    };
    let value = serde_json::to_value(BenchOutput::Single(Box::new(single))).unwrap();
    assert_eq!(value["variant"], "single");
    assert_eq!(value["payload"]["passed"], true);
    assert_eq!(value["payload"]["component"], "studio");

    let (cmp, _) = aggregate_comparison("studio".to_string(), 10, Vec::new());
    let value = serde_json::to_value(BenchOutput::Comparison(cmp)).unwrap();
    assert_eq!(value["variant"], "comparison");
    assert_eq!(value["payload"]["comparison"], "cross_rig");
}
