use super::super::*;
use std::collections::BTreeMap;
use types::{RunnerDoctorStatus, RunnerRepairAction};

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
fn bare_repair_deterministically_selects_the_lab_offload_scope() {
    assert_eq!(
        repair_scope(RunnerDoctorScope::General, true),
        RunnerDoctorScope::LabOffload
    );
    assert_eq!(
        repair_scope(RunnerDoctorScope::General, false),
        RunnerDoctorScope::General
    );
    assert_eq!(
        repair_scope(RunnerDoctorScope::SecretEnv, true),
        RunnerDoctorScope::SecretEnv
    );
}

#[test]
fn doctor_output_omits_empty_repairs() {
    let (report, _) = run("local").expect("local doctor report");
    let value = serde_json::to_value(report).expect("serialize report");

    assert!(value.get("repairs").is_none());
}

#[test]
fn compact_doctor_projection_bounds_evidence_and_renders_action() {
    let (mut report, _) = run("local").expect("local doctor report");
    report.checks = (0..(COMPACT_CHECK_LIMIT + 5))
        .map(|index| types::RunnerCheck {
            id: format!("check-{index}"),
            status: RunnerDoctorStatus::Warning,
            message: "m".repeat(4_000),
            remediation: Some("homeboy runner doctor local --repair".to_string()),
            remediation_action: None,
            details: BTreeMap::from([("large".to_string(), "d".repeat(4_000))]),
        })
        .collect();

    let compact = output_projection(report, false);
    let rendered = serde_json::to_string(&compact).expect("compact JSON");
    assert_eq!(
        compact["checks"].as_array().expect("checks").len(),
        COMPACT_CHECK_LIMIT
    );
    assert_eq!(compact["truncation"]["checks"]["omitted"], 5);
    assert!(rendered.len() < 16 * 1024, "{rendered}");
    assert_eq!(
        render_summary(&compact).as_deref(),
        Some("Runner doctor\nStatus: degraded\nChecks shown: 12\nNext: homeboy runner doctor local --full")
    );
    assert!(projection_envelope_bytes(&compact).unwrap() <= COMPACT_PROJECTION_BYTES);
}

#[test]
fn compact_doctor_projection_retains_provider_readiness() {
    let (mut report, _) = run("local").expect("local doctor report");
    report.provider_readiness = Some(types::RunnerDoctorProviderReadiness {
        ready_for: vec!["ready.provider".to_string()],
        blocked_for: vec!["blocked.provider".to_string()],
        unverified_for: vec!["unverified.provider".to_string()],
        unverified_remediation: Some("Authentication has not been verified.".to_string()),
    });

    let compact = output_projection(report, false);

    assert_eq!(
        compact["provider_readiness"]["ready_for"],
        serde_json::json!(["ready.provider"])
    );
    assert_eq!(
        compact["provider_readiness"]["blocked_for"],
        serde_json::json!(["blocked.provider"])
    );
    assert_eq!(
        compact["provider_readiness"]["unverified_for"],
        serde_json::json!(["unverified.provider"])
    );
    assert_eq!(
        compact["provider_readiness"]["guidance"],
        "Authentication has not been verified."
    );
}

#[test]
fn compact_doctor_projection_retains_all_unverified_provider_readiness() {
    let (mut report, _) = run("local").expect("local doctor report");
    report.provider_readiness = Some(types::RunnerDoctorProviderReadiness {
        ready_for: Vec::new(),
        blocked_for: Vec::new(),
        unverified_for: vec!["opencode.agent-task-executor".to_string()],
        unverified_remediation: Some(
            "Provider authentication is unverified because runner doctor cannot select a model."
                .to_string(),
        ),
    });

    let compact = output_projection(report, false);

    assert_eq!(
        compact["provider_readiness"]["unverified_for"],
        serde_json::json!(["opencode.agent-task-executor"])
    );
    assert_eq!(
        compact["provider_readiness"]["guidance"],
        "Provider authentication is unverified because runner doctor cannot select a model."
    );
    assert_eq!(compact["truncation"]["provider_readiness"]["shown"], 1);
}

#[test]
fn full_doctor_projection_retains_nonsecret_unverified_provider_ids() {
    let (mut report, _) = run("local").expect("local doctor report");
    report.provider_readiness = Some(types::RunnerDoctorProviderReadiness {
        ready_for: Vec::new(),
        blocked_for: Vec::new(),
        unverified_for: vec!["opencode.agent-task-executor".to_string()],
        unverified_remediation: None,
    });

    let full = output_projection(report, true);

    assert_eq!(
        full["provider_readiness"]["unverified_for"],
        serde_json::json!(["opencode.agent-task-executor"])
    );
}

#[test]
fn compact_doctor_puts_blockers_and_remediation_before_informational_checks() {
    let (mut report, _) = run("local").expect("local doctor report");
    report.checks = (0..COMPACT_CHECK_LIMIT)
        .map(|index| types::RunnerCheck {
            id: format!("ok-{index}"),
            status: RunnerDoctorStatus::Ok,
            message: "ready".to_string(),
            remediation: None,
            remediation_action: None,
            details: BTreeMap::new(),
        })
        .chain(std::iter::once(types::RunnerCheck {
            id: "blocked".to_string(),
            status: RunnerDoctorStatus::Error,
            message: "runner is unavailable".to_string(),
            remediation: Some("homeboy runner doctor local --repair".to_string()),
            remediation_action: None,
            details: BTreeMap::new(),
        }))
        .collect();

    let compact = output_projection(report, false);
    assert_eq!(compact["checks"][0]["id"], "blocked");
    assert_eq!(
        compact["checks"][0]["remediation"],
        "homeboy runner doctor local --repair"
    );
    assert_eq!(compact["truncation"]["checks"]["omitted"], 1);
}

#[test]
fn compact_doctor_retains_safe_typed_runner_convergence_action() {
    let (mut report, _) = run("local").expect("local doctor report");
    report.checks = vec![types::RunnerCheck {
        id: "homeboy.version_skew".to_string(),
        status: RunnerDoctorStatus::Warning,
        message: "controller is ahead".to_string(),
        remediation: Some("refresh runner".to_string()),
        remediation_action: Some(RunnerRepairAction::RefreshHomeboy {
            git_ref: Some("abc1234".to_string()),
            allow_downgrade: false,
        }),
        details: BTreeMap::new(),
    }];

    let compact = output_projection(report, false);

    assert_eq!(
        compact["checks"][0]["remediation_action"]["action"],
        "refresh_homeboy"
    );
    assert_eq!(
        compact["checks"][0]["remediation_action"]["git_ref"],
        "abc1234"
    );
    assert_eq!(
        compact["checks"][0]["remediation_action"]["allow_downgrade"],
        false
    );
}

#[test]
fn compact_doctor_retains_failed_repair_cause_and_remediation() {
    let (mut report, _) = run("local").expect("local doctor report");
    report.repairs.push(types::RunnerRepair {
        id: "repair.daemon".to_string(),
        status: RunnerDoctorStatus::Error,
        message: "promotion lease remained contended after the bounded wait".to_string(),
        commands: vec!["homeboy runner doctor local --scope lab-offload --repair".to_string()],
    });

    let compact = output_projection(report, false);

    assert_eq!(compact["repairs"][0]["id"], "repair.daemon");
    assert_eq!(
        compact["repairs"][0]["message"],
        "promotion lease remained contended after the bounded wait"
    );
    assert_eq!(
        compact["repairs"][0]["commands"][0],
        "homeboy runner doctor local --scope lab-offload --repair"
    );
}

#[test]
fn doctor_failure_projection_lifts_actionable_root_causes_in_compact_and_full_modes() {
    for (check_id, reason_code, remediation, expected_code) in [
        (
            "daemon.recovery",
            Some("pid_dead"),
            "homeboy runner connect local --adopt-orphan-lease lease-dead",
            "runner.doctor.daemon_recovery.pid_dead",
        ),
        (
            "provider.auth",
            Some("authentication_denied"),
            "homeboy runner doctor local --scope lab-offload",
            "runner.doctor.provider_auth.authentication_denied",
        ),
        (
            "daemon.exec",
            Some("runner_doctor.daemon_timeout"),
            "homeboy runner doctor local --scope lab-offload",
            "runner.doctor.daemon_exec.runner_doctor_daemon_timeout",
        ),
        (
            "inventory.stale",
            Some("stale_inventory"),
            "homeboy runner doctor local --scope lab-offload",
            "runner.doctor.inventory_stale.stale_inventory",
        ),
    ] {
        for full in [false, true] {
            let (mut report, _) = run("local").expect("local doctor report");
            report.status = RunnerDoctorStatus::Error;
            report.checks = vec![types::RunnerCheck {
                id: check_id.to_string(),
                status: RunnerDoctorStatus::Error,
                message: format!("{check_id} failed"),
                remediation: Some(remediation.to_string()),
                remediation_action: None,
                details: BTreeMap::from([(
                    "reason_code".to_string(),
                    reason_code.expect("fixture reason").to_string(),
                )]),
            }];

            let projection = output_projection(report, full);
            assert_eq!(
                projection["failure"]["code"], expected_code,
                "{check_id}, full={full}"
            );
            let data = serde_json::to_value(
                crate::commands::runner::types::RunnerCommandOutput::Doctor(Box::new(projection)),
            )
            .expect("doctor output serializes");
            let envelope = compact_command_run(Ok(data), 1)
                .with_identity(
                    &crate::commands::utils::response::CommandIdentity::with_operation(
                        "runner", "doctor",
                    ),
                )
                .stdout_envelope();
            let envelope = serde_json::to_value(envelope).expect("envelope serializes");
            assert_eq!(
                envelope["diagnostics"]["code"], expected_code,
                "{check_id}, full={full}"
            );
            assert_eq!(
                envelope["next_actions"][0]["command"], remediation,
                "{check_id}, full={full}"
            );
        }
    }
}

#[test]
fn doctor_failure_projection_names_an_invariant_violation_without_failed_checks() {
    let (mut report, _) = run("local").expect("local doctor report");
    report.status = RunnerDoctorStatus::Error;
    report.checks.clear();

    let compact = output_projection(report, false);

    assert_eq!(compact["failure"]["code"], "runner.doctor.readiness_error");
    assert_eq!(
        compact["failure"]["next_actions"][0]["command"],
        "homeboy runner doctor local --full"
    );
}

#[test]
fn doctor_failure_does_not_promote_prose_or_unredacted_secret_details() {
    let secret = "ghp_abcdefghijklmnopqrstuvwxyz1234567890";

    for full in [false, true] {
        let (mut report, _) = run("local").expect("local doctor report");
        report.status = RunnerDoctorStatus::Error;
        report.checks = vec![types::RunnerCheck {
            id: "tool.required.example".to_string(),
            status: RunnerDoctorStatus::Error,
            message: format!("probe failed with token={secret}"),
            remediation: Some("Fix the shell environment, then rerun doctor.".to_string()),
            remediation_action: None,
            details: BTreeMap::from([("probe_error".to_string(), format!("token={secret}"))]),
        }];
        let projection = output_projection(report, full);
        let rendered = projection.to_string();

        assert!(!rendered.contains(secret), "full={full}");
        assert_eq!(
            projection["failure"]["next_actions"][0]["command"],
            "homeboy runner doctor local --full",
            "full={full}"
        );
        assert_eq!(projection["failure"]["next_actions"][0]["kind"], "show");
    }
}

#[test]
fn compact_doctor_hard_bounds_oversized_identity_and_command_metadata() {
    let (mut report, _) = run("local").expect("local doctor report");
    report.runner_id = "runner-".repeat(10_000);
    report.runner.registry.as_mut().expect("registry").id = "registry-".repeat(10_000);
    report.runner.server = Some(types::RunnerServerSummary {
        id: "server-".repeat(10_000),
        host: "host-".repeat(10_000),
        user: "user-".repeat(10_000),
        port: 22,
        is_localhost: false,
    });
    report.checks = vec![types::RunnerCheck {
        id: "check-".repeat(10_000),
        status: RunnerDoctorStatus::Error,
        message: "message-".repeat(10_000),
        remediation: Some("command-".repeat(10_000)),
        remediation_action: None,
        details: BTreeMap::new(),
    }];

    let compact = output_projection(report, false);
    assert!(projection_envelope_bytes(&compact).unwrap() <= COMPACT_PROJECTION_BYTES);
    assert!(compact["runner_id"].as_str().unwrap().len() <= COMPACT_TEXT_LIMIT + 3);
    let data = serde_json::to_value(crate::commands::runner::types::RunnerCommandOutput::Doctor(
        Box::new(compact),
    ))
    .expect("doctor output serializes");
    let run = compact_command_run(Ok(data), 0).with_identity(
        &crate::commands::utils::response::CommandIdentity::with_operation("runner", "doctor"),
    );
    let wire = serde_json::to_vec(&run.stdout_envelope()).expect("doctor wire serializes");
    assert!(wire.len() <= COMPACT_PROJECTION_BYTES);
    let wire: serde_json::Value = serde_json::from_slice(&wire).expect("doctor wire round trips");
    assert_eq!(wire["command"], "runner");
    assert_eq!(wire["operation"], "doctor");
    assert_eq!(
        format!(
            "{} {}",
            wire["command"].as_str().unwrap(),
            wire["operation"].as_str().unwrap()
        ),
        "runner doctor"
    );
    assert!(wire["presentation"]["stdout"].is_string());
}

#[test]
fn compact_doctor_falls_back_when_escaped_untrusted_fields_exceed_the_wire_budget() {
    let (mut report, _) = run("local").expect("local doctor report");
    let escaped = "\"\\\n".repeat(10_000);
    report.checks = (0..COMPACT_CHECK_LIMIT)
        .map(|index| types::RunnerCheck {
            id: format!("{index}-{escaped}"),
            status: RunnerDoctorStatus::Error,
            message: escaped.clone(),
            remediation: Some(escaped.clone()),
            remediation_action: None,
            details: BTreeMap::new(),
        })
        .collect();

    let compact = output_projection(report, false);
    assert!(projection_envelope_bytes(&compact).unwrap() <= COMPACT_PROJECTION_BYTES);
    assert!(compact["checks"].as_array().unwrap().is_empty());
    assert_eq!(
        compact["truncation"]["checks"]["omitted"],
        "see_full_output"
    );
}

#[test]
fn compact_doctor_size_fallback_retains_failed_repair() {
    let (mut report, _) = run("local").expect("local doctor report");
    let escaped = "\"\\\n".repeat(10_000);
    report.checks = (0..COMPACT_CHECK_LIMIT)
        .map(|_| types::RunnerCheck {
            id: escaped.clone(),
            status: RunnerDoctorStatus::Error,
            message: escaped.clone(),
            remediation: Some(escaped.clone()),
            remediation_action: None,
            details: BTreeMap::new(),
        })
        .collect();
    report.repairs.push(types::RunnerRepair {
        id: "repair.daemon".to_string(),
        status: RunnerDoctorStatus::Error,
        message: "promotion wait exhausted; do not use generic reconnect".to_string(),
        commands: vec!["homeboy runner doctor local --scope lab-offload --repair".to_string()],
    });

    let compact = output_projection(report, false);

    assert_eq!(compact["checks"].as_array().unwrap().len(), 0);
    assert_eq!(compact["repairs"][0]["id"], "repair.daemon");
    assert_eq!(
        compact["repairs"][0]["message"],
        "promotion wait exhausted; do not use generic reconnect"
    );
    assert!(projection_envelope_bytes(&compact).unwrap() <= COMPACT_PROJECTION_BYTES);
}

#[test]
fn compact_doctor_uses_pretty_stdout_bytes_at_the_boundary() {
    let (mut report, _) = run("local").expect("local doctor report");
    let length = (1..=COMPACT_TEXT_LIMIT)
        .find(|length| {
            report.checks = (0..COMPACT_CHECK_LIMIT)
                .map(|index| types::RunnerCheck {
                    id: format!("check-{index}-{}", "x".repeat(*length)),
                    status: RunnerDoctorStatus::Error,
                    message: "m".repeat(*length),
                    remediation: Some("r".repeat(*length)),
                    remediation_action: None,
                    details: BTreeMap::new(),
                })
                .collect();
            let payload = compact_projection(&report);
            let data = serde_json::to_value(
                crate::commands::runner::types::RunnerCommandOutput::Doctor(Box::new(payload)),
            )
            .expect("doctor output serializes");
            let run = compact_command_run(Ok(data), 0).with_identity(
                &crate::commands::utils::response::CommandIdentity::with_operation(
                    "runner", "doctor",
                ),
            );
            let envelope = run.stdout_envelope();
            serde_json::to_vec(&envelope).is_ok_and(|compact| {
                compact.len() <= COMPACT_PROJECTION_BYTES
                    && envelope
                        .stdout_json()
                        .is_ok_and(|pretty| pretty.len() > COMPACT_PROJECTION_BYTES)
            })
        })
        .expect("a compact-only boundary case");
    report.checks = (0..COMPACT_CHECK_LIMIT)
        .map(|index| types::RunnerCheck {
            id: format!("check-{index}-{}", "x".repeat(length)),
            status: RunnerDoctorStatus::Error,
            message: "m".repeat(length),
            remediation: Some("r".repeat(length)),
            remediation_action: None,
            details: BTreeMap::new(),
        })
        .collect();

    let bounded = output_projection(report, false);
    assert!(projection_envelope_bytes(&bounded).unwrap() <= COMPACT_PROJECTION_BYTES);
    assert!(bounded["checks"].as_array().unwrap().is_empty());
}

#[test]
fn capabilities_are_runner_substrate_only() {
    let (report, _) = run("local").expect("local doctor report");
    let value = serde_json::to_value(report).expect("serialize report");
    let capabilities = value["capabilities"]
        .as_object()
        .expect("capabilities object");

    assert!(capabilities.contains_key("local_execution"));
    assert!(capabilities.contains_key("homeboy_available"));
    assert!(!capabilities.contains_key("github_cli"));
    assert!(!capabilities.contains_key("node"));
    assert!(!capabilities.contains_key("npm"));
    assert!(!capabilities.contains_key("php"));
    assert!(!capabilities.contains_key("docker"));
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

#[test]
fn lab_offload_readiness_keeps_a_healthy_eligible_provider_ready() {
    let checks = vec![
        live_auth_provider_check("selected.provider", RunnerDoctorStatus::Ok),
        provider_check("optional.provider", RunnerDoctorStatus::Error),
    ];
    let eligible = vec![
        "selected.provider".to_string(),
        "optional.provider".to_string(),
    ];

    let (status, readiness) = checks::lab_offload_status(&checks, &eligible);

    assert_eq!(status, RunnerDoctorStatus::Ok);
    assert_eq!(readiness.ready_for, vec!["selected.provider"]);
    assert_eq!(readiness.blocked_for, vec!["optional.provider"]);
    assert!(readiness.unverified_for.is_empty());
}

#[test]
fn lab_offload_readiness_blocks_a_failed_selected_provider() {
    let checks = vec![
        live_auth_provider_check("selected.provider", RunnerDoctorStatus::Error),
        provider_check("optional.provider", RunnerDoctorStatus::Ok),
    ];
    let eligible = vec!["selected.provider".to_string()];

    let (status, readiness) = checks::lab_offload_status(&checks, &eligible);

    assert_eq!(status, RunnerDoctorStatus::Error);
    assert!(readiness.ready_for.is_empty());
    assert_eq!(readiness.blocked_for, vec!["selected.provider"]);
    assert!(readiness.unverified_for.is_empty());
}

#[test]
fn lab_offload_readiness_error_dominates_live_auth_in_any_order() {
    let eligible = vec!["selected.provider".to_string()];
    for checks in [
        vec![
            provider_check("selected.provider", RunnerDoctorStatus::Error),
            live_auth_provider_check("selected.provider", RunnerDoctorStatus::Ok),
        ],
        vec![
            live_auth_provider_check("selected.provider", RunnerDoctorStatus::Ok),
            provider_check("selected.provider", RunnerDoctorStatus::Error),
            live_auth_provider_check("selected.provider", RunnerDoctorStatus::Ok),
        ],
    ] {
        let (status, readiness) = checks::lab_offload_status(&checks, &eligible);
        assert_eq!(status, RunnerDoctorStatus::Error);
        assert!(readiness.ready_for.is_empty());
        assert_eq!(readiness.blocked_for, eligible);
    }
}

#[test]
fn lab_offload_readiness_does_not_treat_a_resolved_require_graph_as_live_auth() {
    let checks = vec![provider_check("selected.provider", RunnerDoctorStatus::Ok)];
    let eligible = vec!["selected.provider".to_string()];

    let (status, readiness) = checks::lab_offload_status(&checks, &eligible);

    assert_eq!(status, RunnerDoctorStatus::Warning);
    assert!(readiness.ready_for.is_empty());
    assert!(readiness.blocked_for.is_empty());
    assert_eq!(readiness.unverified_for, eligible);
    assert_eq!(
        readiness.unverified_remediation.as_deref(),
        Some("Provider authentication is unverified because runner doctor cannot select a model. Run the selected task's normal preflight; doctor never changes credentials.")
    );
}

#[test]
fn lab_offload_readiness_blocks_all_providers_on_a_runner_prerequisite_error() {
    let checks = vec![checks::error(
        "extension.parity",
        "required extension is stale".to_string(),
        None,
        BTreeMap::new(),
    )];
    let eligible = vec![
        "selected.provider".to_string(),
        "optional.provider".to_string(),
    ];

    let (status, readiness) = checks::lab_offload_status(&checks, &eligible);

    assert_eq!(status, RunnerDoctorStatus::Error);
    assert!(readiness.ready_for.is_empty());
    assert_eq!(readiness.blocked_for, eligible);
    assert!(readiness.unverified_for.is_empty());
}

fn provider_check(provider_id: &str, status: RunnerDoctorStatus) -> types::RunnerCheck {
    types::RunnerCheck {
        id: format!("provider.{provider_id}"),
        status,
        message: "provider readiness".to_string(),
        remediation: None,
        remediation_action: None,
        details: BTreeMap::from([("provider_id".to_string(), provider_id.to_string())]),
    }
}

fn live_auth_provider_check(provider_id: &str, status: RunnerDoctorStatus) -> types::RunnerCheck {
    let mut check = provider_check(provider_id, status);
    check
        .details
        .insert("readiness_scope".to_string(), "live_auth".to_string());
    check
}

#[test]
fn operational_exit_code_matches_the_doctor_readiness_verdict() {
    for (scenario, status, expected_exit_code) in [
        ("healthy", RunnerDoctorStatus::Ok, 0),
        ("degraded", RunnerDoctorStatus::Warning, 0),
        // A disconnected runner with a recoverable daemon is still not ready
        // until `--repair` has rerun its probe successfully.
        ("disconnected_recoverable", RunnerDoctorStatus::Error, 1),
        ("terminal_error", RunnerDoctorStatus::Error, 1),
    ] {
        assert_eq!(
            status.operational_exit_code(),
            expected_exit_code,
            "{scenario}"
        );
    }
}
