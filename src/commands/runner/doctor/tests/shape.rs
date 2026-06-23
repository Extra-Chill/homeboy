use super::super::*;
use std::collections::BTreeMap;
use types::RunnerDoctorStatus;

#[test]
fn local_alias_report_has_stable_top_level_shape() {
    let (report, exit_code) = run("local").expect("local doctor report");
    assert_eq!(exit_code, 0);
    let value = serde_json::to_value(report).expect("serialize report");
    assert_eq!(value["command"], "runner.doctor");
    assert_eq!(value["runner_id"], "local");
    assert!(value.get("status").is_some());
    assert!(value.get("capabilities").is_some());
    assert!(value.get("resources").is_some());
    assert!(value
        .get("checks")
        .and_then(|checks| checks.as_array())
        .is_some());
}

#[test]
fn doctor_options_default_to_general_read_only_scope() {
    let options = RunnerDoctorOptions::default();

    assert_eq!(options.scope, RunnerDoctorScope::General);
    assert!(!options.repair);
}

#[test]
fn doctor_output_omits_empty_repairs() {
    let (report, _) = run("local").expect("local doctor report");
    let value = serde_json::to_value(report).expect("serialize report");

    assert!(value.get("repairs").is_none());
}

#[test]
fn overall_status_promotes_errors_over_warnings() {
    let checks = vec![
        checks::warning("optional", "optional missing".to_string(), None),
        checks::error(
            "required",
            "required missing".to_string(),
            None,
            BTreeMap::new(),
        ),
    ];
    assert_eq!(checks::overall_status(&checks), RunnerDoctorStatus::Error);
}
