use super::{health_indicator, validate_exec_apply_boundary};
use homeboy::core::server::health::{ServerHealth, ServerHealthState};

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

#[test]
fn fleet_health_indicator_never_marks_unproven_transport_green() {
    let healthy = ServerHealth {
        state: ServerHealthState::Healthy,
        ..Default::default()
    };
    let unhealthy = ServerHealth {
        state: ServerHealthState::Unhealthy,
        ..Default::default()
    };
    let not_checked = ServerHealth {
        state: ServerHealthState::NotChecked,
        ..Default::default()
    };

    assert_eq!(health_indicator(Some(&healthy)), "✅");
    assert_eq!(health_indicator(Some(&unhealthy)), "❌");
    assert_eq!(health_indicator(Some(&not_checked)), "❌");
}
