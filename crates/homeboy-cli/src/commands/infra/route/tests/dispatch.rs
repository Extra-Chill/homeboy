#![cfg(test)]

use super::*;

#[test]
fn explicit_local_promotion_defers_target_resolution_to_promotion() {
    let args = [
        "homeboy",
        "--placement",
        "local",
        "agent-task",
        "promote",
        "/tmp/aggregate.json",
        "--to-worktree",
        "fixture@dirty-candidate",
    ];
    let cli = Cli::parse_from(args);
    let normalized = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();

    assert_eq!(route_after_parse(&cli, &normalized, None).unwrap(), None);
}
use clap::Parser;
use homeboy::command_contract::{lab_runner_supports_contract_label, LabCommandPortability};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use tempfile::tempdir;

use super::*;

#[test]
fn lab_cook_dispatcher_recipe_round_trips_exact_transport() {
    let dispatcher = LabCookAttemptDispatcher {
        runner_id: "homeboy-lab".to_string(),
        allow_local_fallback: true,
        allow_dirty_lab_workspace: false,
        skip_deps_hydration: true,
        detach_after_handoff: true,
        source_path: Some(PathBuf::from("/controller/source")),
        job_overrides: runners::LabJobOverrides {
            env: [("MODE".to_string(), "test".to_string())].into(),
            secret_env_names: vec!["TOKEN".to_string()],
            workspace_root: Some("/runner/workspaces".to_string()),
        },
    };
    let recipe = crate::core::agent_task_service::AgentTaskCookAttemptDispatcher::durable_recipe(
        &dispatcher,
    )
    .unwrap();

    let reconstructed = reconstruct_cook_attempt_dispatcher(&recipe)
        .unwrap()
        .expect("Lab dispatcher reconstructed");

    assert_eq!(reconstructed.durable_recipe().unwrap(), recipe);

    let mut legacy_recipe = recipe;
    legacy_recipe
        .as_object_mut()
        .expect("dispatcher recipe")
        .remove("detach_after_handoff");
    let legacy = reconstruct_cook_attempt_dispatcher(&legacy_recipe)
        .unwrap()
        .expect("legacy Lab dispatcher reconstructed");
    assert_eq!(
        legacy.durable_recipe().unwrap()["detach_after_handoff"],
        false
    );
}

#[test]
fn non_lab_command_continues_local_dispatch() {
    // route_after_parse mutates the process-global LAB_OFFLOAD_METADATA_ENV,
    // so hold the env lock to serialize against tests that assert on it.
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    let cli = Cli::parse_from(["homeboy", "status"]);

    let outcome = route_after_parse(&cli, &["homeboy".into(), "status".into()], None).unwrap();

    assert_eq!(outcome, None);
}

#[test]
fn changed_scope_lint_is_lab_portable() {
    let cli = Cli::parse_from([
        "homeboy",
        "review",
        "lint",
        "--changed-since",
        "origin/main",
    ]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "review lint");
    assert!(command.is_portable());
}

#[test]
fn nested_review_quality_subcommands_use_specific_lab_labels() {
    for (args, expected_label) in [
        (
            vec!["homeboy", "review", "audit", "data-machine"],
            "review audit",
        ),
        (
            vec!["homeboy", "review", "lint", "data-machine"],
            "review lint",
        ),
        (
            vec!["homeboy", "review", "test", "data-machine"],
            "review test",
        ),
        (
            vec!["homeboy", "review", "build", "data-machine"],
            "review build",
        ),
        (
            vec![
                "homeboy",
                "review",
                "ci",
                "run",
                "data-machine",
                "--job",
                "lint",
            ],
            "review ci",
        ),
    ] {
        let cli = Cli::parse_from(args);
        let command = cli.command.lab_contract().unwrap();

        assert_eq!(command.hot_label, expected_label);
    }
}

#[test]
fn nested_review_lint_dispatch_uses_matching_lab_label() {
    let cli = Cli::parse_from(["homeboy", "review", "lint", "data-machine"]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(command.hot_label, "review lint");
    assert!(command.is_portable());
}

#[test]
fn nested_review_quality_subcommand_resolves_effective_component() {
    let cli = Cli::parse_from(["homeboy", "review", "lint", "data-machine"]);
    let Commands::Review(args) = cli.command else {
        panic!("expected review command");
    };

    assert_eq!(
        args.effective_component_args().component.as_deref(),
        Some("data-machine")
    );
}

#[test]
fn nested_review_quality_in_dir_offload_uses_current_dir_path() {
    let dir = tempdir().expect("tempdir");
    let _cwd = CwdGuard::set(dir.path());
    let normalized = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "lint".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    let rewritten = lab_route_source_path_args(&cli.command, &normalized, false)
        .expect("review lint without component gets cwd path rewrite");
    let cwd = std::env::current_dir().expect("current dir");

    assert_eq!(rewritten[0..3], normalized);
    assert_eq!(rewritten[3], "--path");
    assert_eq!(rewritten[4], cwd.to_string_lossy());
}

#[test]
fn explicit_runner_for_changed_scope_test_is_lab_portable() {
    let cli = Cli::parse_from([
        "homeboy",
        "--runner",
        "homeboy-lab",
        "review",
        "test",
        "--changed-since",
        "origin/main",
    ]);

    let command = lab_offload_command(&cli.command).unwrap().unwrap();

    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
    assert_eq!(command.hot_label, "review test");
    assert!(command.is_portable());
}

#[test]
fn destructive_fuzz_local_execution_requires_explicit_destructive_local_override() {
    // Serialize against LAB_OFFLOAD_METADATA_ENV-asserting tests: this
    // routes through route_after_parse, which mutates that global.
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    let normalized = vec![
        "homeboy",
        "--placement",
        "local",
        "fuzz",
        "run",
        "component-a",
        "--allow-destructive",
        "--isolation",
        "isolated",
        "--isolation-proof",
        "proof.json",
    ];
    let cli = Cli::parse_from(&normalized);

    assert!(destructive_fuzz_requires_lab(&cli.command));

    let error = crate::test_support::with_isolated_home(|_| {
        route_after_parse(
            &cli,
            &normalized
                .iter()
                .map(|arg| arg.to_string())
                .collect::<Vec<_>>(),
            None,
        )
        .expect_err("destructive fuzz local route should be refused")
    });
    assert!(error
        .to_string()
        .contains("destructive fuzz refused local controller execution"));
}

#[test]
fn destructive_fuzz_local_override_is_command_specific_and_explicit() {
    let cli = Cli::parse_from([
        "homeboy",
        "--placement",
        "local",
        "fuzz",
        "run",
        "component-a",
        "--allow-destructive",
        "--allow-local-destructive-fuzz",
        "--isolation",
        "isolated",
        "--isolation-proof",
        "proof.json",
    ]);

    assert!(!destructive_fuzz_requires_lab(&cli.command));
}

#[test]
fn rig_up_dry_run_with_runner_emits_runner_exec_plan() {
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    crate::test_support::with_isolated_home(|home| {
        runners::create(
            r#"{"id":"homeboy-lab","kind":"local","homeboy_path":"/runner/bin/homeboy-patched"}"#,
            false,
        )
        .expect("runner");
        write_command_only_rig(home.path(), "script-matrix");
        let output = home.path().join("plan.json");
        let normalized = vec![
            "homeboy".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "rig".to_string(),
            "up".to_string(),
            "script-matrix".to_string(),
            "--dry-run".to_string(),
        ];
        let cli = Cli::parse_from(&normalized);

        let outcome = route_after_parse(&cli, &normalized, Some(&output.to_string_lossy()))
            .expect("route rig up plan");

        assert_eq!(outcome, Some(0));
        let plan: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(output).expect("read output plan"))
                .expect("parse output plan");
        assert_eq!(plan["variant"], "up_plan");
        assert_eq!(plan["payload"]["runner_id"], "homeboy-lab");
        assert_eq!(
            plan["payload"]["selected_homeboy_binary"],
            "/runner/bin/homeboy-patched"
        );
        assert_eq!(
            plan["payload"]["commands"][0],
            "/runner/bin/homeboy-patched runner exec homeboy-lab --cwd tools --env MATRIX=portable -- sh -c ./scripts/run-matrix.sh"
        );
    });
}

#[test]
fn lab_job_overrides_parse_env_json_and_workspace_root() {
    let cli = Cli::parse_from([
        "homeboy",
        "--runner-env",
        "STUDIO_NATIVE_TRACE_SAMPLE_RUNTIME_PLUGIN_PATH=/tmp/sample-runtime",
        "--runner-env",
        "API_TOKEN=secret-token",
        "--lab-env-json",
        r#"{"EXTRA_PATH":"/tmp/extra","EMPTY":null}"#,
        "--runner-workspace-root",
        "/srv/job-workspace",
        "review",
        "test",
        "studio-native",
    ]);

    let overrides = lab_job_overrides(&cli).expect("overrides");

    assert_eq!(
        overrides.env["STUDIO_NATIVE_TRACE_SAMPLE_RUNTIME_PLUGIN_PATH"],
        "/tmp/sample-runtime"
    );
    assert_eq!(overrides.env["EXTRA_PATH"], "/tmp/extra");
    assert_eq!(overrides.env["EMPTY"], "");
    assert_eq!(
        overrides.workspace_root.as_deref(),
        Some("/srv/job-workspace")
    );
    assert!(overrides
        .secret_env_names
        .contains(&"API_TOKEN".to_string()));
}

#[test]
fn lab_job_overrides_reject_invalid_env_shapes() {
    let cli = Cli::parse_from(["homeboy", "--runner-env", "NO_EQUALS", "review"]);
    let err = lab_job_overrides(&cli).expect_err("invalid pair");
    assert_eq!(err.code.as_str(), "validation.invalid_argument");

    let cli = Cli::parse_from(["homeboy", "--lab-env-json", "[]", "review"]);
    let err = lab_job_overrides(&cli).expect_err("invalid json object");
    assert_eq!(err.code.as_str(), "validation.invalid_argument");
}

#[test]
fn changed_since_lint_keeps_git_scope_for_lab_runner() {
    let normalized = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "lint".to_string(),
        "--changed-since".to_string(),
        "origin/main".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    let rewritten = inject_lab_changed_files(&cli.command, &normalized).unwrap();

    assert!(rewritten.is_none());
}

#[test]
fn changed_since_test_keeps_git_scope_for_lab_runner() {
    let normalized = vec![
        "homeboy".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "review".to_string(),
        "test".to_string(),
        "--changed-since=origin/main".to_string(),
        "--skip-lint".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    let rewritten = inject_lab_changed_files(&cli.command, &normalized).unwrap();

    assert!(rewritten.is_none());
}

#[test]
fn lab_offload_subprocess_skips_recursive_lab_routing() {
    let _env = EnvGuard::set(
        homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV,
        r#"{"status":"offloaded"}"#,
    );
    let cli = Cli::parse_from([
        "homeboy",
        "--runner",
        "homeboy-lab",
        "trace",
        "--rig",
        "gutenberg-pattern-preview-assets",
        "gutenberg",
        "pattern-preview-assets",
    ]);
    let normalized = [
        "homeboy".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "trace".to_string(),
        "--rig".to_string(),
        "gutenberg-pattern-preview-assets".to_string(),
        "gutenberg".to_string(),
        "pattern-preview-assets".to_string(),
    ];

    let outcome = route_after_parse(&cli, &normalized, None).unwrap();

    assert_eq!(outcome, None);
}

#[test]
fn runner_hosted_bench_exec_skips_recursive_lab_routing_without_explicit_runner() {
    let _env = EnvGuard::set_many(&[
        (homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV, None),
        (homeboy::core::runner::RUNNER_HOSTED_EXEC_ENV, Some("1")),
        (
            homeboy::core::runner::RUNNER_PLACEMENT_RESOLVED_ENV,
            Some("1"),
        ),
        (homeboy::core::runner::RUNNER_ID_ENV, Some("homeboy-lab")),
    ]);
    let normalized = vec![
        "homeboy".to_string(),
        "--placement".to_string(),
        "local".to_string(),
        "bench".to_string(),
        "--extension".to_string(),
        "wordpress".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    let outcome = route_after_parse(&cli, &normalized, None)
        .expect("runner-hosted bench execution should stay local");

    assert_eq!(outcome, None);
}

#[test]
fn ambient_resolved_marker_cannot_bypass_explicit_lab_placement() {
    let _env = EnvGuard::set_many(&[
        (homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV, None),
        (homeboy::core::runner::RUNNER_HOSTED_EXEC_ENV, None),
        (homeboy::core::runner::RUNNER_ID_ENV, None),
        (
            homeboy::core::runner::RUNNER_PLACEMENT_RESOLVED_ENV,
            Some("1"),
        ),
    ]);
    let normalized = vec![
        "homeboy".to_string(),
        "--placement".to_string(),
        "lab".to_string(),
        "review".to_string(),
        "lint".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    let err = crate::test_support::with_isolated_home(|_| {
        route_after_parse(&cli, &normalized, None)
            .expect_err("ambient marker must not bypass required Lab placement")
    });

    assert_ne!(err.code.as_str(), "internal.unexpected");
}

#[test]
fn managed_runner_context_bypasses_auto_routing_once() {
    let _env = EnvGuard::set_many(&[
        (homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV, None),
        (homeboy::core::runner::RUNNER_HOSTED_EXEC_ENV, Some("1")),
        (
            homeboy::core::runner::RUNNER_PLACEMENT_RESOLVED_ENV,
            Some("1"),
        ),
        (homeboy::core::runner::RUNNER_ID_ENV, Some("homeboy-lab")),
    ]);
    let normalized = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "lint".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    assert_eq!(
        route_after_parse(&cli, &normalized, None).expect("managed context"),
        None
    );
}

#[test]
fn agent_task_doctor_runner_option_routes_locally() {
    let _env = EnvGuard::remove(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV);
    let normalized = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "doctor".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "--repair".to_string(),
    ];
    let cli = Cli::parse_from(&normalized);

    assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));

    let outcome = route_after_parse(&cli, &normalized, None)
        .expect("agent-task doctor owns --runner and should not be Lab-routed");

    assert_eq!(outcome, None);
    assert!(std::env::var(homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV).is_err());
}

#[test]
fn trace_lab_dispatch_timeout_reads_env_override() {
    let _env = EnvGuard::set(lab_routing::LAB_TRACE_DISPATCH_TIMEOUT_ENV, "7");

    assert_eq!(
        lab_routing::lab_trace_dispatch_timeout(),
        std::time::Duration::from_secs(7)
    );
}

#[test]
fn lab_route_dispatch_timeout_plumbs_core_timeout() {
    let trace_cli = Cli::parse_from(["homeboy", "trace", "list"]);
    let lint_cli = Cli::parse_from(["homeboy", "review", "lint"]);

    assert_eq!(
        lab_route_dispatch_timeout(&trace_cli.command),
        Some(lab_routing::lab_trace_dispatch_timeout())
    );
    assert_eq!(lab_route_dispatch_timeout(&lint_cli.command), None);
}

#[test]
fn detached_agent_task_handoffs_do_not_use_trace_dispatch_timeout() {
    let cook = Cli::parse_from([
        "homeboy",
        "--detach-after-handoff",
        "agent-task",
        "cook",
        "--repo",
        "homeboy",
        "--goal",
        "Fix the detached handoff",
        "--to-worktree",
        "homeboy@fix-7971",
        "--run-id",
        "cook-7971",
        "--runner",
        "homeboy-lab",
        "--placement",
        "lab",
    ]);
    let batch = Cli::parse_from([
        "homeboy",
        "--detach-after-handoff",
        "agent-task",
        "fanout",
        "cook-batch",
        "--repo",
        "homeboy",
        "--verify",
        "cargo test --lib",
        "--run-plan",
        "https://github.com/Extra-Chill/homeboy/issues/7167",
    ]);
    let retry = Cli::parse_from([
        "homeboy",
        "--detach-after-handoff",
        "agent-task",
        "retry",
        "failed-run",
        "--run",
        "--runner",
        "homeboy-lab",
        "--placement",
        "lab",
    ]);

    for cli in [&cook, &batch, &retry] {
        assert_eq!(lab_route_dispatch_timeout(&cli.command), None);
    }
}

#[test]
fn cook_retry_lab_source_is_the_derived_baseline_not_the_controller_workspace() {
    let baseline = tempfile::tempdir().expect("baseline");
    let controller = tempfile::tempdir().expect("controller");
    let capability = crate::core::agent_task_service::test_derived_cook_baseline_capability(
        baseline.path().to_path_buf(),
        "baseline-commit".to_string(),
        "baseline-tree".to_string(),
        "task",
        Some(serde_json::json!({"workspace_snapshot_identity": "snapshot:parent"})),
    );

    assert_eq!(
        super::cook_attempt_source_path(Some(&capability), Some(controller.path())),
        Some(capability.canonical_path())
    );
    assert_eq!(
        capability.verified_baseline_provenance(),
        serde_json::json!({
            "source_run_id": "test-source-run",
            "source_task_id": "task",
            "promoted_patch_artifact_sha256": "test-artifact-sha256",
            "baseline_commit": "baseline-commit",
            "baseline_tree": "baseline-tree",
            "parent_snapshot_identity": "snapshot:parent",
        })
    );
}

#[test]
fn detached_retry_materializes_failed_plan_and_persists_bounded_preacceptance_failure() {
    crate::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path());
        let source_plan = homeboy::core::agent_tasks::scheduler::AgentTaskPlan::new(
            "failed-retry-source",
            vec![serde_json::from_value(serde_json::json!({
                "task_id": "retry-task",
                "executor": {
                    "backend": "fixture",
                    "config": { "workspace_root": workspace.path() }
                },
                "instructions": "retry",
                "workspace": { "root": workspace.path() }
            }))
            .expect("task")],
        );
        agent_task_lifecycle::submit_plan(&source_plan, Some("failed-run"))
            .expect("source run submitted");
        let source_plan = agent_task_lifecycle::load_plan("failed-run").expect("source plan");
        let failure = Error::internal_unexpected("provider exited before completion");
        agent_task_lifecycle::record_pre_execution_failure(
            "failed-run",
            &source_plan,
            "provider_execution",
            &failure,
        )
        .expect("source failure persisted");

        let normalized = [
            "homeboy",
            "--detach-after-handoff",
            "--cwd",
            "/controller/homeboy",
            "agent-task",
            "retry",
            "failed-run",
            "--run",
            "--new-run-id",
            "failed-run-retry-on-lab",
            "--runner",
            "homeboy-lab",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let cli = Cli::parse_from([
            "homeboy",
            "--detach-after-handoff",
            "agent-task",
            "retry",
            "failed-run",
            "--run",
            "--new-run-id",
            "failed-run-retry-on-lab",
            "--runner",
            "homeboy-lab",
        ]);
        let handoff = materialize_agent_task_retry_handoff(&cli, &normalized)
            .expect("retry handoff materialized")
            .expect("retry handoff");

        assert!(!handoff.args.iter().any(|arg| arg == "--cwd"));
        assert_eq!(
            handoff.primary_workspace,
            workspace
                .path()
                .canonicalize()
                .expect("canonical workspace")
        );
        assert!(!handoff.args.iter().any(|arg| arg == "/controller/homeboy"));
        let agent_task_index = handoff
            .args
            .iter()
            .position(|arg| arg == "agent-task")
            .expect("agent task");
        assert_eq!(handoff.args[agent_task_index + 1], "run-plan");
        assert_eq!(handoff.args[agent_task_index + 2], "--plan");
        assert_eq!(handoff.args[agent_task_index + 4], "--record-run-id");
        assert_eq!(handoff.args[agent_task_index + 5], handoff.run_id);
        assert_eq!(handoff.run_id, "failed-run-retry-on-lab");
        let remote_cli = Cli::try_parse_from(&handoff.args).expect("portable run-plan argv");
        let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command: crate::commands::agent_task::AgentTaskCommand::RunPlan(remote),
        }) = &remote_cli.command
        else {
            panic!("Lab handoff must execute the materialized plan, not discover a retry record");
        };
        assert_eq!(
            remote.record_run_id.as_deref(),
            Some(handoff.run_id.as_str())
        );
        let remote_plan: homeboy::core::agent_tasks::scheduler::AgentTaskPlan =
            serde_json::from_str(&remote.plan).expect("serialized retry plan");
        assert_eq!(remote_plan, handoff.plan);
        assert_eq!(
            remote_plan.tasks[0].workspace.root.as_deref(),
            Some(workspace.path().to_str().expect("workspace utf8"))
        );
        // The emitted command is accepted by the real CLI parser without
        // inventing a global --cwd. The route carries the selected task
        // checkout separately, and workspace staging maps it to the job cwd.
        assert!(remote_cli.detach_after_handoff);
        let replacement = agent_task_lifecycle::status(&handoff.run_id).expect("replacement");
        assert_eq!(replacement.metadata["retry_of"], "failed-run");

        let error = persist_retry_handoff_preacceptance_failure(
            &handoff,
            Error::internal_unexpected("runner preflight rejected the handoff"),
        );
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message.contains("agent-task retry")
                && hint.message.contains(&handoff.run_id)));
        let replacement = agent_task_lifecycle::status(&handoff.run_id).expect("failed retry");
        assert_eq!(
            replacement.state,
            homeboy::core::agent_tasks::lifecycle::AgentTaskRunState::Failed
        );
        assert_eq!(
            replacement.metadata["pre_execution_failure"]["phase"],
            "detached_lab_handoff_preacceptance"
        );
    });
}

#[test]
fn controller_owned_run_materializes_plan_for_lab_execution() {
    crate::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        let plan = homeboy::core::agent_tasks::scheduler::AgentTaskPlan::new(
            "queued-controller-run",
            vec![serde_json::from_value(serde_json::json!({
                "task_id": "task",
                "executor": { "backend": "fixture" },
                "instructions": "run this queued task",
                "workspace": { "root": workspace.path() }
            }))
            .expect("task")],
        );
        agent_task_lifecycle::submit_plan(&plan, Some("controller-queued"))
            .expect("submit controller run");
        let args = [
            "homeboy",
            "agent-task",
            "run",
            "controller-queued",
            "--timeout-ms",
            "1200",
            "--runner",
            "homeboy-lab",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let cli = Cli::parse_from(&args);

        let handoff = materialize_agent_task_run_handoff(&cli, &args)
            .expect("materialize run handoff")
            .expect("controller-owned handoff");
        let remote_cli = Cli::try_parse_from(&handoff.args).expect("portable run-plan argv");
        let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
            command: crate::commands::agent_task::AgentTaskCommand::RunPlan(remote),
        }) = remote_cli.command
        else {
            panic!("controller run must execute its materialized plan on Lab");
        };

        assert_eq!(remote.record_run_id.as_deref(), Some("controller-queued"));
        assert_eq!(remote.timeout_ms, Some(1200));
        assert_eq!(
            serde_json::from_str::<homeboy::core::agent_tasks::scheduler::AgentTaskPlan>(
                &remote.plan
            )
            .expect("serialized plan"),
            plan
        );
        assert_eq!(
            handoff.primary_workspace,
            workspace.path().canonicalize().unwrap()
        );
    });
}

#[test]
fn controller_owned_run_refuses_to_handoff_a_plan_without_a_workspace() {
    crate::test_support::with_isolated_home(|_| {
        let plan = homeboy::core::agent_tasks::scheduler::AgentTaskPlan::new(
            "queued-controller-run-without-workspace",
            vec![serde_json::from_value(serde_json::json!({
                "task_id": "task",
                "executor": { "backend": "fixture" },
                "instructions": "run this queued task"
            }))
            .expect("task")],
        );
        agent_task_lifecycle::submit_plan(&plan, Some("controller-queued-without-workspace"))
            .expect("submit controller run");
        let args = [
            "homeboy",
            "agent-task",
            "run",
            "controller-queued-without-workspace",
            "--runner",
            "homeboy-lab",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let cli = Cli::parse_from(&args);

        let error = materialize_agent_task_run_handoff(&cli, &args)
            .expect_err("workspace-less controller plan must fail before Lab handoff");

        assert_eq!(error.details["field"], "workspace");
        assert!(error
            .message
            .contains("requires exactly one task workspace"));
    });
}

#[test]
fn controller_owned_run_refuses_an_ambiguous_plan_workspace() {
    let first = tempfile::tempdir().expect("first workspace");
    let second = tempfile::tempdir().expect("second workspace");
    let plan = homeboy::core::agent_tasks::scheduler::AgentTaskPlan::new(
        "ambiguous-controller-run",
        vec![
            serde_json::from_value(serde_json::json!({
                "task_id": "first",
                "executor": { "backend": "fixture" },
                "instructions": "first task",
                "workspace": { "root": first.path() }
            }))
            .expect("first task"),
            serde_json::from_value(serde_json::json!({
                "task_id": "second",
                "executor": { "backend": "fixture" },
                "instructions": "second task",
                "workspace": { "root": second.path() }
            }))
            .expect("second task"),
        ],
    );

    let error = plan_primary_workspace(&plan)
        .expect_err("multiple controller plan workspaces must fail before handoff");

    assert_eq!(error.details["field"], "workspace");
    assert!(error.message.contains("multiple task workspaces"));
}

#[test]
fn lab_owned_retry_is_not_read_from_the_controller_store() {
    crate::test_support::with_isolated_home(|_| {
        let args = [
            "homeboy",
            "agent-task",
            "retry",
            "lab-owned-failed-run",
            "--run",
            "--runner",
            "homeboy-lab",
        ]
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
        let cli = Cli::parse_from(&args);

        assert!(materialize_agent_task_retry_handoff(&cli, &args)
            .expect("runner-owned retry stays portable")
            .is_none());
    });
}

#[test]
fn explicit_local_cook_does_not_enter_lab_attempt_dispatch() {
    let cli = Cli::parse_from([
        "homeboy",
        "--placement",
        "local",
        "agent-task",
        "cook",
        "--to-worktree",
        "fixture@local",
        "--verify",
        "true",
        "--prompt",
        "keep this local",
    ]);

    assert!(
        run_split_placement_cook(&cli, &[], None, Some("homeboy-lab"))
            .expect("local placement bypasses Lab cook dispatch")
            .is_none()
    );
}

#[test]
fn cook_to_worktree_provider_workspace_survives_failed_attempt_and_lab_retry() {
    crate::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path());
        let provider_dir = tempfile::tempdir().expect("provider dir");
        let provider = provider_dir.path().join("provider");
        let payload = serde_json::json!({
            "worktrees": [{
                "handle": "blocks-engine@cook-six-fixture-generic-parity",
                "path": workspace.path(),
                "branch": "main",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            }]
        });
        fs::write(
            &provider,
            format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", payload),
        )
        .expect("write provider");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&provider)
                .expect("provider metadata")
                .permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&provider, permissions).expect("make provider executable");
        }
        let mut config = homeboy::core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: false,
                commands: homeboy::core::defaults::WorktreeProviderCommands {
                    resolve: Some(vec![provider.display().to_string(), "{handle}".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: Some(
                    homeboy::core::defaults::WorktreeProviderListResultMapping {
                        items: "$.worktrees".to_string(),
                        handle: "$.handle".to_string(),
                        path: "$.path".to_string(),
                        branch: "$.branch".to_string(),
                        dirty: "$.safety.dirty".to_string(),
                        unpushed: "$.safety.unpushed".to_string(),
                        primary: "$.safety.primary".to_string(),
                    },
                ),
            },
        );
        homeboy::core::defaults::save_config(&config).expect("save provider config");

        let cook_cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--to-worktree",
            "blocks-engine@cook-six-fixture-generic-parity",
            "--verify",
            "true",
            "--backend",
            "fixture",
            "--prompt",
            "retry this task",
        ]);
        let plan = materialize_agent_task_cook_plan(&cook_cli)
            .expect("materialize cook plan")
            .expect("cook plan");
        let expected_root = workspace.path().display().to_string();
        assert_eq!(
            plan.tasks[0].workspace.root.as_deref(),
            Some(expected_root.as_str())
        );
        agent_task_lifecycle::submit_plan(&plan, Some("failed-run")).expect("submit plan");
        agent_task_lifecycle::record_pre_execution_failure(
            "failed-run",
            &plan,
            "lab_handoff_preacceptance",
            &Error::internal_unexpected("Lab rejected the initial attempt"),
        )
        .expect("persist failed attempt");

        let retry_args = ["homeboy", "agent-task", "retry", "failed-run", "--run"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let retry_cli = Cli::parse_from(&retry_args);
        let handoff = materialize_agent_task_retry_handoff(&retry_cli, &retry_args)
            .expect("materialize retry handoff")
            .expect("retry handoff");

        assert_eq!(
            handoff.primary_workspace,
            workspace.path().canonicalize().expect("root")
        );
        assert_eq!(
            handoff.plan.tasks[0].workspace.root.as_deref(),
            Some(expected_root.as_str())
        );
    });
}

#[test]
fn retry_handoff_refuses_multiple_task_workspaces() {
    crate::test_support::with_isolated_home(|_| {
        let first = tempfile::tempdir().expect("first workspace");
        let second = tempfile::tempdir().expect("second workspace");
        git_init(first.path());
        git_init(second.path());
        let task = |id: &str, root: &Path| {
            serde_json::from_value(serde_json::json!({
                "task_id": id,
                "executor": { "backend": "fixture" },
                "instructions": "retry",
                "workspace": { "root": root }
            }))
            .expect("task")
        };
        let plan = homeboy::core::agent_tasks::scheduler::AgentTaskPlan::new(
            "multiple-workspaces",
            vec![task("first", first.path()), task("second", second.path())],
        );
        agent_task_lifecycle::submit_plan(&plan, Some("failed-run")).expect("source plan");
        let source_plan = agent_task_lifecycle::load_plan("failed-run").expect("source plan");
        agent_task_lifecycle::record_pre_execution_failure(
            "failed-run",
            &source_plan,
            "provider_execution",
            &Error::internal_unexpected("failed"),
        )
        .expect("source failure");
        let normalized = ["homeboy", "agent-task", "retry", "failed-run", "--run"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let cli = Cli::parse_from(&normalized);

        let error = match materialize_agent_task_retry_handoff(&cli, &normalized) {
            Ok(_) => panic!("multiple workspaces must fail closed"),
            Err(error) => error,
        };
        assert!(error.message.contains("multiple task workspaces"));
        assert!(agent_task_lifecycle::status("failed-run-retry-1").is_err());
    });
}

#[test]
fn retry_handoff_identifies_an_original_plan_without_a_workspace() {
    crate::test_support::with_isolated_home(|_| {
        let plan = homeboy::core::agent_tasks::scheduler::AgentTaskPlan::new(
            "missing-workspace",
            vec![serde_json::from_value(serde_json::json!({
                "task_id": "retry-task",
                "executor": { "backend": "fixture" },
                "instructions": "retry"
            }))
            .expect("task")],
        );
        agent_task_lifecycle::submit_plan(&plan, Some("failed-run")).expect("source plan");
        let source_plan = agent_task_lifecycle::load_plan("failed-run").expect("source plan");
        agent_task_lifecycle::record_pre_execution_failure(
            "failed-run",
            &source_plan,
            "provider_execution",
            &Error::internal_unexpected("failed"),
        )
        .expect("source failure");
        let normalized = ["homeboy", "agent-task", "retry", "failed-run", "--run"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let cli = Cli::parse_from(&normalized);

        let error = match materialize_agent_task_retry_handoff(&cli, &normalized) {
            Ok(_) => panic!("missing original workspace must fail before creating a retry"),
            Err(error) => error,
        };

        assert!(error.message.contains("original persisted plan has none"));
        assert!(agent_task_lifecycle::status("failed-run-retry-1").is_err());
    });
}

#[test]
fn agent_task_fanout_dispatch_id_uses_explicit_or_stable_default() {
    let cli = Cli::parse_from([
        "homeboy",
        "--detach-after-handoff",
        "agent-task",
        "fanout",
        "cook-batch",
        "--repo",
        "homeboy",
        "--fanout-id",
        "wave-7167",
        "--verify",
        "cargo test --lib",
        "--run-plan",
        "https://github.com/Extra-Chill/homeboy/issues/7167",
    ]);
    let Commands::AgentTask(crate::commands::agent_task::AgentTaskArgs {
        command:
            crate::commands::agent_task::AgentTaskCommand::Fanout(
                crate::commands::agent_task::AgentTaskFanoutArgs {
                    command: crate::commands::agent_task::AgentTaskFanoutCommand::CookBatch(args),
                },
            ),
    }) = cli.command
    else {
        panic!("cook-batch command");
    };

    assert_eq!(agent_task_fanout_cook_batch_dispatch_id(&args), "wave-7167");

    let mut default_args = args;
    default_args.fanout_id = None;
    assert_eq!(
        agent_task_fanout_cook_batch_dispatch_id(&default_args),
        "cook-batch-homeboy-issue-7167-1"
    );
}

#[test]
fn agent_task_fanout_finish_metadata_preserves_discoverability_commands() {
    let metadata = agent_task_fanout_finish_metadata(
        serde_json::json!({
            "lab_dispatch": {
                "status": "error",
                "runner_id": "homeboy-lab",
            },
        }),
        "dispatch-run-7167",
        "cook-batch-homeboy-issue-7167-1",
        RunStatus::Error,
    );

    assert_eq!(
        metadata["agent_task_lab_dispatch"]["fanout_id"],
        "cook-batch-homeboy-issue-7167-1"
    );
    assert_eq!(metadata["agent_task_lab_dispatch"]["status"], "error");
    assert_eq!(
        metadata["follow_commands"]["dispatch_status"],
        "homeboy runs show dispatch-run-7167"
    );
    assert_eq!(
        metadata["follow_commands"]["dispatch_evidence"],
        "homeboy runs evidence --run dispatch-run-7167"
    );
    assert_eq!(
        metadata["follow_commands"]["fanout_status"],
        "homeboy agent-task fanout status cook-batch-homeboy-issue-7167-1"
    );
}

#[test]
fn offloaded_stdout_write_preserves_bytes_for_output_file() {
    let dir = tempdir().unwrap();
    let output_path = dir.path().join("out.json");

    write_offloaded_stdout(&output_path.to_string_lossy(), "{\"ok\":true}\n").unwrap();

    assert_eq!(
        std::fs::read_to_string(output_path).unwrap(),
        "{\"ok\":true}\n"
    );
}

#[test]
fn runner_rig_source_management_remote_preflight_strips_controller_globals() {
    let normalized = vec![
        "homeboy".to_string(),
        "rig".to_string(),
        "sources".to_string(),
        "list".to_string(),
        "--runner".to_string(),
        "homeboy-lab".to_string(),
        "--output=./sources.json".to_string(),
        "--placement".to_string(),
        "lab-or-local".to_string(),
        "--placement=lab".to_string(),
        "--detach-after-handoff".to_string(),
    ];

    let command = runner_rig_source_management_command("/usr/local/bin/homeboy", &normalized);
    let preflight = strip_rig_source_management_local_wrapper_flags(&command);

    assert_eq!(
        preflight,
        vec![
            "/usr/local/bin/homeboy".to_string(),
            "rig".to_string(),
            "sources".to_string(),
            "list".to_string(),
        ]
    );
}

#[test]
fn runner_rig_source_management_translates_local_subdir_paths() {
    let command = vec![
        "/runner/bin/homeboy".to_string(),
        "rig".to_string(),
        "install".to_string(),
        "/Users/chubes/Developer/homeboy-rigs@run/WordPress/static-site-importer".to_string(),
    ];

    let translated = translate_command_path_prefix(
        &command,
        std::path::Path::new("/Users/chubes/Developer/homeboy-rigs@run"),
        "/home/chubes/Developer/_lab_workspaces/homeboy-rigs-run-abc",
    );

    assert_eq!(
        translated[3],
        "/home/chubes/Developer/_lab_workspaces/homeboy-rigs-run-abc/WordPress/static-site-importer"
    );
}

#[test]
fn rig_install_source_arg_finds_positional_source_after_flags() {
    let command = vec![
        "/runner/bin/homeboy".to_string(),
        "rig".to_string(),
        "install".to_string(),
        "--id".to_string(),
        "static-site-importer".to_string(),
        "--reinstall".to_string(),
        "/Users/chubes/Developer/homeboy-rigs@run/WordPress/static-site-importer".to_string(),
        "--all".to_string(),
    ];

    assert_eq!(
        rig_install_source_arg(&command).as_deref(),
        Some("/Users/chubes/Developer/homeboy-rigs@run/WordPress/static-site-importer")
    );
}

#[test]
fn rig_install_source_arg_ignores_non_install_commands() {
    let command = vec![
        "/runner/bin/homeboy".to_string(),
        "rig".to_string(),
        "sources".to_string(),
        "list".to_string(),
    ];

    assert_eq!(rig_install_source_arg(&command), None);
}

#[test]
fn rig_install_source_sync_root_resolves_existing_local_package() {
    let source_dir = tempdir().expect("source dir");
    let source_path = source_dir
        .path()
        .canonicalize()
        .expect("canonical temp dir")
        .join("static-site-importer");
    fs::create_dir_all(&source_path).expect("create source package");
    let command = vec![
        "/runner/bin/homeboy".to_string(),
        "rig".to_string(),
        "install".to_string(),
        source_path.to_string_lossy().to_string(),
    ];

    let sync_root = rig_install_source_sync_root(&command).expect("sync root");

    // The temp dir is not a git repo, so the package directory itself is the
    // materialization root.
    assert_eq!(sync_root, source_path);
}

#[test]
fn rig_install_source_sync_root_skips_git_url_and_missing_paths() {
    let git_url = vec![
        "/runner/bin/homeboy".to_string(),
        "rig".to_string(),
        "install".to_string(),
        "https://github.com/Extra-Chill/homeboy-rigs.git".to_string(),
    ];
    assert_eq!(rig_install_source_sync_root(&git_url), None);

    let missing = vec![
        "/runner/bin/homeboy".to_string(),
        "rig".to_string(),
        "install".to_string(),
        "/Users/chubes/Developer/does-not-exist-rig-package-6964".to_string(),
    ];
    assert_eq!(rig_install_source_sync_root(&missing), None);
}
