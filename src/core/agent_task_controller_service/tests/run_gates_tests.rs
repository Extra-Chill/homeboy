use super::super::*;
use super::*;

#[test]
fn run_gates_records_generic_terminal_outcomes() {
    with_isolated_home(|_| {
        let mut passed = init(ControllerInitRequest {
            loop_id: "loop-service-gate-passed".to_string(),
            phase: "verify".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        passed.gate_bundles.push(AgentTaskGateBundle {
            bundle_id: "green".to_string(),
            description: String::new(),
            checks: Vec::new(),
        });
        passed.record_action(
            AgentTaskLoopPolicyAction::RunGates {
                bundle_id: "green".to_string(),
                entity_id: None,
            },
            "run green gate",
        );
        controller::write_controller(&passed).expect("passed controller written");

        let passed_result = run_next(
            "loop-service-gate-passed",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("gate action executed");
        assert_eq!(passed_result.exit_code, 0);
        let passed_loaded = controller::load_controller("loop-service-gate-passed")
            .expect("passed controller loaded");
        assert_eq!(
            passed_loaded.terminal_outcomes[0].status,
            AgentTaskLoopTerminalStatus::Passed
        );

        let mut blocked = init(ControllerInitRequest {
            loop_id: "loop-service-gate-blocked".to_string(),
            phase: "verify".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        blocked.gate_bundles.push(AgentTaskGateBundle {
            bundle_id: "red".to_string(),
            description: String::new(),
            checks: vec![AgentTaskGateBundleCheck {
                check_id: "api-check".to_string(),
                kind: AgentTaskGateBundleCheckKind::Api,
                input: Value::Null,
                retryable: false,
            }],
        });
        blocked.record_action(
            AgentTaskLoopPolicyAction::RunGates {
                bundle_id: "red".to_string(),
                entity_id: None,
            },
            "run red gate",
        );
        controller::write_controller(&blocked).expect("blocked controller written");

        let blocked_result = run_next(
            "loop-service-gate-blocked",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("gate action executed");
        assert_eq!(blocked_result.exit_code, 1);
        let blocked_loaded = controller::load_controller("loop-service-gate-blocked")
            .expect("blocked controller loaded");
        assert_eq!(
            blocked_loaded.terminal_outcomes[0].status,
            AgentTaskLoopTerminalStatus::BlockedByGate
        );
    });
}

#[test]
fn run_gates_blocks_on_pending_manual_check() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-gate-manual".to_string(),
            phase: "verify".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        record.gate_bundles.push(AgentTaskGateBundle {
            bundle_id: "manual-only".to_string(),
            description: "manual acceptance signal".to_string(),
            checks: vec![AgentTaskGateBundleCheck {
                check_id: "external-signal".to_string(),
                kind: AgentTaskGateBundleCheckKind::Manual,
                input: json!({ "metric": "coverage" }),
                retryable: false,
            }],
        });
        record.record_action(
            AgentTaskLoopPolicyAction::RunGates {
                bundle_id: "manual-only".to_string(),
                entity_id: None,
            },
            "run manual gate",
        );
        controller::write_controller(&record).expect("controller written");

        // A manual-only bundle must NOT auto-pass as an acceptable warning.
        let result = run_next(
            "loop-service-gate-manual",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("gate action executed");
        assert_eq!(result.exit_code, 1);

        let loaded =
            controller::load_controller("loop-service-gate-manual").expect("controller loaded");
        assert_eq!(loaded.gate_results.len(), 1);
        assert_eq!(
            loaded.gate_results[0].status,
            AgentTaskGateBundleStatus::Pending
        );
        assert_eq!(
            loaded.terminal_outcomes[0].status,
            AgentTaskLoopTerminalStatus::BlockedByGate
        );

        // The acceptance gate diagnostic reports the gate as Pending (blocking)
        // with a problem, instead of a false-green warning.
        let diagnostics =
            controller::controller_status_diagnostics(&loaded).expect("controller diagnostics");
        assert_eq!(diagnostics.summary.pending_acceptance_gate_count, 1);
        assert_eq!(diagnostics.summary.failed_acceptance_gate_count, 0);
        assert!(diagnostics.acceptance_gates.iter().any(|gate| {
            gate.bundle_id == "manual-only"
                && gate.status == controller::AgentTaskLoopAcceptanceGateStatus::Pending
                && gate
                    .problems
                    .contains(&"acceptance gate is pending an external/manual result".to_string())
        }));
    });
}

#[test]
fn run_gates_executes_command_bundle_and_records_result() {
    with_isolated_home(|_| {
        let mut record = init(ControllerInitRequest {
            loop_id: "loop-service-gates".to_string(),
            phase: "validate".to_string(),
            config_version: "v1".to_string(),
        })
        .expect("controller initialized");
        record.gate_bundles.push(AgentTaskGateBundle {
            bundle_id: "quality".to_string(),
            description: "quality gates".to_string(),
            checks: vec![AgentTaskGateBundleCheck {
                check_id: "true-command".to_string(),
                kind: AgentTaskGateBundleCheckKind::Command,
                input: json!({ "command": "true" }),
                retryable: false,
            }],
        });
        record.record_action(
            AgentTaskLoopPolicyAction::RunGates {
                bundle_id: "quality".to_string(),
                entity_id: None,
            },
            "run quality gates",
        );
        controller::write_controller(&record).expect("controller written");

        let result = run_next(
            "loop-service-gates",
            CapturingExecutor::default(),
            &NoopDispatchHook,
        )
        .expect("gates executed");

        assert_eq!(result.exit_code, 0);
        let loaded = controller::load_controller("loop-service-gates").expect("controller");
        assert_eq!(loaded.gate_results.len(), 1);
        assert_eq!(
            loaded.gate_results[0].status,
            AgentTaskGateBundleStatus::Passed
        );
    });
}

#[test]
fn command_gate_check_runs_from_configured_cwd() {
    let cwd = tempfile::tempdir().expect("cwd tempdir");
    let cwd_path = cwd.path().to_string_lossy().into_owned();
    std::fs::write(cwd.path().join("gate-marker"), "ok").expect("marker file");
    let check = AgentTaskGateBundleCheck {
        check_id: "pwd-command".to_string(),
        kind: AgentTaskGateBundleCheckKind::Command,
        input: json!({
            "command": "test -f gate-marker && printf ok",
            "cwd": cwd_path,
        }),
        retryable: false,
    };

    let result = run_command_gate_check(&check).expect("command gate executed");

    assert_eq!(result.status, AgentTaskGateBundleStatus::Passed);
    assert_eq!(result.details["stdout"].as_str(), Some("ok"));
    assert_eq!(result.details["cwd"].as_str(), Some(cwd_path.as_str()));
}

#[test]
fn command_gate_check_caps_stored_stdout_and_records_truncation() {
    let check = AgentTaskGateBundleCheck {
        check_id: "large-output-command".to_string(),
        kind: AgentTaskGateBundleCheckKind::Command,
        input: json!({
            "command": "yes x | head -c 70000",
            "timeout_seconds": 5,
        }),
        retryable: false,
    };

    let result = run_command_gate_check(&check).expect("command gate executed");

    assert_eq!(result.status, AgentTaskGateBundleStatus::Passed);
    assert_eq!(result.details["stdout_truncated"].as_bool(), Some(true));
    assert_eq!(
        result.details["stdout_stored_bytes"].as_u64(),
        Some(64 * 1024)
    );
    assert_eq!(result.details["stdout_bytes"].as_u64(), Some(70000));
    assert_eq!(
        result.details["stdout"]
            .as_str()
            .expect("stdout stored")
            .len(),
        64 * 1024
    );
}
