use crate::core::extension::runner_contract::RunnerStepFilter;

#[test]
fn test_runner_contract_external_smoke() {
    let filter = RunnerStepFilter {
        step: Some("lint,test".to_string()),
        skip: Some("test".to_string()),
    };

    assert!(filter.should_run("lint"));
    assert!(!filter.should_run("test"));
}
