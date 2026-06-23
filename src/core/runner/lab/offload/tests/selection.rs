use super::*;

#[test]
fn lab_runner_selection_keeps_explicit_runner_precedence() {
    let command = portable_lab_command("test");
    let selection = resolve_lab_runner_selection_from_default(
        &command,
        Some("lab-explicit"),
        false,
        false,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect("selection")
    .expect("explicit runner selected");

    assert_eq!(selection.runner_id, "lab-explicit");
    assert_eq!(selection.source, LabRunnerSelectionSource::Explicit);
}

#[test]
fn lab_runner_selection_force_hot_keeps_explicit_runner_precedence() {
    let command = portable_lab_command("test");
    let selection = resolve_lab_runner_selection_from_default(
        &command,
        Some("lab-explicit"),
        true,
        false,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect("selection")
    .expect("explicit runner selected");

    assert_eq!(selection.runner_id, "lab-explicit");
    assert_eq!(selection.source, LabRunnerSelectionSource::Explicit);
}

#[test]
fn lab_runner_selection_uses_default_for_supported_commands() {
    let command = portable_lab_command("test");
    let selection = resolve_lab_runner_selection_from_default(
        &command,
        None,
        false,
        false,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect("selection")
    .expect("default runner selected");

    assert_eq!(selection.runner_id, "lab-default");
    assert_eq!(selection.source, LabRunnerSelectionSource::Default);
}

#[test]
fn lab_runner_selection_ignores_default_when_auto_offload_is_disabled() {
    let mut command = portable_lab_command("extension update");
    command.routing_policy.default_lab_offload = false;

    let selection = resolve_lab_runner_selection_from_default(
        &command,
        None,
        false,
        false,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect("selection");

    assert!(selection.is_none());
}

#[test]
fn lab_runner_selection_honors_explicit_runner_when_auto_offload_is_disabled() {
    let mut command = portable_lab_command("extension update");
    command.routing_policy.default_lab_offload = false;

    let selection = resolve_lab_runner_selection_from_default(
        &command,
        Some("lab-explicit"),
        false,
        false,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect("selection")
    .expect("explicit runner selected");

    assert_eq!(selection.runner_id, "lab-explicit");
    assert_eq!(selection.source, LabRunnerSelectionSource::Explicit);
}

#[test]
fn lab_runner_selection_runs_locally_without_default_runner() {
    let command = portable_lab_command("test");

    assert!(resolve_lab_runner_selection_from_default(
        &command, None, false, false, false, false, None
    )
    .expect("selection")
    .is_none());
}

#[test]
fn lab_runner_selection_force_hot_refuses_local_when_default_runner_exists() {
    let command = portable_lab_command("test");

    let err = resolve_lab_runner_selection_from_default(
        &command,
        None,
        true,
        false,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect_err("force-hot should require explicit local override");

    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err
        .message
        .contains("--force-hot would run portable hot command"));
    assert!(err.message.contains("lab-default"));
    let tried = err.details["tried"].as_array().expect("tried");
    assert!(tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("--allow-local-hot"))));
}

#[test]
fn lab_runner_selection_force_hot_runs_locally_without_default_runner() {
    let command = portable_lab_command("test");

    assert!(resolve_lab_runner_selection_from_default(
        &command, None, true, false, false, false, None
    )
    .expect("selection")
    .is_none());
}

#[test]
fn lab_runner_selection_rejects_allow_local_hot_without_force_hot() {
    let command = portable_lab_command("rig check");

    let err = resolve_lab_runner_selection_from_default(
        &command,
        None,
        false,
        true,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect_err("allow-local-hot alone must not silently auto-offload");

    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err.message.contains("--allow-local-hot only permits"));
    assert!(err.message.contains("--force-hot"));
    assert!(err.message.contains("automatic Lab offload"));
    let tried = err.details["tried"].as_array().expect("tried");
    assert!(tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("--force-hot --allow-local-hot"))));
}

#[test]
fn lab_runner_selection_allow_local_hot_overrides_default_runner_gate() {
    let command = portable_lab_command("test");

    assert!(resolve_lab_runner_selection_from_default(
        &command,
        None,
        true,
        true,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect("selection")
    .is_none());
}

#[test]
fn lab_runner_selection_explains_hot_commands_that_stay_local() {
    let err = resolve_lab_runner_selection_from_default(
        &local_only_lab_command("current single-workspace Lab snapshot cannot safely mirror"),
        Some("lab-explicit"),
        false,
        false,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect_err("rig up rejects explicit runner");

    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err.message.contains("single-workspace Lab snapshot"));
}

#[test]
fn lab_runner_selection_denies_local_bench_when_host_policy_requires_lab() {
    let command = portable_lab_command("bench");

    let err =
        resolve_lab_runner_selection_from_default(&command, None, true, true, true, false, None)
            .expect_err("bench local execution should be denied by config policy");

    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err.message.contains("/bench/local_execution"));
    assert!(err.message.contains("denied"));
    let tried = err.details["tried"].as_array().expect("tried");
    assert!(tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("--runner <runner-id>"))));
}

#[test]
fn release_gate_force_hot_allow_local_hot_fails_closed_with_default_runner() {
    // #4605: --force-hot --allow-local-hot must not silently bypass Lab
    // routing for a release gate when a default runner is configured.
    let command = release_gate_lab_command("lint");

    let err = resolve_lab_runner_selection_from_default(
        &command,
        None,
        true,
        true,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect_err("release gate force-local bypass must fail closed");

    assert_eq!(err.code.as_str(), "validation.invalid_argument");
    assert!(err.message.contains("Release gate `lint`"));
    assert!(err.message.contains("--force-hot --allow-local-hot"));
    assert!(err.message.contains("lab-default"));
    assert!(err.message.contains("/release_gate/local_hot"));
    let tried = err.details["tried"].as_array().expect("tried");
    assert!(tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("/release_gate/local_hot"))));
    assert!(tried.iter().any(|hint| hint
        .as_str()
        .is_some_and(|hint| hint.contains("HOMEBOY_RELEASE_GATE_LOCAL_HOT"))));
}

#[test]
fn release_gate_force_hot_allow_local_hot_allowed_by_policy() {
    // When the operator opts in via /release_gate/local_hot=allowed, the
    // bypass runs locally and is recorded (None selection → local run).
    let command = release_gate_lab_command("test");

    assert!(resolve_lab_runner_selection_from_default(
        &command,
        None,
        true,
        true,
        false,
        true,
        Some("lab-default".to_string()),
    )
    .expect("selection")
    .is_none());
}

#[test]
fn release_gate_force_hot_allow_local_hot_runs_local_without_default_runner() {
    // No default runner configured → nothing to route to, so the gate runs
    // locally even under fail_closed.
    let command = release_gate_lab_command("audit");

    assert!(resolve_lab_runner_selection_from_default(
        &command, None, true, true, false, false, None
    )
    .expect("selection")
    .is_none());
}

#[test]
fn non_release_gate_command_keeps_allow_local_hot_bypass() {
    // Non-gate portable commands (e.g. agent-task) keep the existing
    // --force-hot --allow-local-hot bypass behavior.
    let command = portable_lab_command("agent-task cook/run-plan");

    assert!(resolve_lab_runner_selection_from_default(
        &command,
        None,
        true,
        true,
        false,
        false,
        Some("lab-default".to_string()),
    )
    .expect("selection")
    .is_none());
}
