use super::validate_exec_apply_boundary;

#[test]
fn fleet_exec_requires_apply_for_real_execution() {
    let command = vec!["wp".to_string(), "plugin".to_string(), "list".to_string()];

    let err = validate_exec_apply_boundary("production", &command, false, false)
        .expect_err("real fleet exec should require --apply");

    assert!(err.message.contains("requires explicit --apply"));
    assert!(err.message.contains("Use --check to preview"));
    assert!(err
        .message
        .contains("homeboy fleet exec production --apply"));
}

#[test]
fn fleet_exec_check_and_applied_execution_pass_apply_guard() {
    let command = vec!["wp".to_string(), "plugin".to_string(), "list".to_string()];

    validate_exec_apply_boundary("production", &command, true, false)
        .expect("--check should not require --apply");
    validate_exec_apply_boundary("production", &command, false, true)
        .expect("--apply should pass guard");
}
