use super::*;
use crate::cli_surface::{Cli, Commands};
use crate::commands::agent_task::fake_pi_test::FakePi;
use clap::Parser;

fn issue_lookup_fixture() -> (tempfile::TempDir, homeboy::core::test_support::EnvVarGuard) {
    let bin = tempfile::tempdir().unwrap();
    homeboy_test_support::write_executable(&bin.path().join("gh"), "#!/bin/sh\ncase \"$*\" in api*closedByPullRequestsReferences*) printf '[]' ;; 'pr list'*) printf '[]' ;; *) exit 1 ;; esac\n", 0o755).unwrap();
    let _path = homeboy::core::test_support::EnvVarGuard::set(
        "PATH",
        format!(
            "{}:{}",
            bin.path().display(),
            std::env::var("PATH").unwrap()
        ),
    );
    (bin, _path)
}

/// Lab placement binds on the runner: preview names no selected route and
/// says the runner decides instead of presenting the configured primary
/// as the readiness result (#15854).
#[test]
fn lab_preview_readiness_projection_defers_binding_to_the_runner() {
    let request: AgentTaskRequest = serde_json::from_value(serde_json::json!({
        "task_id": "cook",
        "executor": { "backend": "pi", "model": "anthropic/claude-sonnet-5" },
        "instructions": "run",
    }))
    .expect("request fixture");
    let plan = AgentTaskPlan::new("lab-preview", vec![request]);

    let lab = cook_provider_readiness_projection(&plan, &plan, true);
    assert_eq!(lab["binding_owner"], "runner");
    assert!(lab["selected_route"].is_null(), "{lab}");
    assert!(lab["statement"]
        .as_str()
        .expect("runner statement")
        .contains("decided on the lab runner"));
    assert!(cook_provider_readiness_disclosure(&plan, true)
        .expect("lab disclosure")
        .contains("not a readiness result"));

    let local = cook_provider_readiness_projection(&plan, &plan, false);
    assert_eq!(local["binding_owner"], "controller");
    assert_eq!(
        local["selected_route"]["model"],
        "anthropic/claude-sonnet-5"
    );
    assert!(cook_provider_readiness_disclosure(&plan, false).is_none());
}
use homeboy::agents::agent_tasks::scheduler::AgentTaskExecutorAdapter;
use homeboy::agents::agent_tasks::{
    AgentTaskExecutionContext, AgentTaskOutcome, AgentTaskOutcomeStatus, AgentTaskRequest,
};

struct UnmaterializedPreviewExecutor;

impl AgentTaskExecutorAdapter for UnmaterializedPreviewExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
            ..Default::default()
        }
    }
}

fn cook(argv: &[&str]) -> AgentTaskCookArgs {
    let cli = Cli::try_parse_from(argv).expect("parse Cook preview");
    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("agent-task command");
    };
    let super::super::AgentTaskCommand::Cook(cook) = agent_task.command else {
        panic!("Cook command");
    };
    *cook
}

/// These fixtures test preview/replay semantics, not the host volume's
/// free space: the volume holding `root` reports ample capacity instead
/// of whatever the machine running the suite has left, and the configured
/// absolute reserve is waived. Capacity-specific tests declare their own.
fn declare_preview_fixture_capacity(
    root: &std::path::Path,
) -> homeboy::core::observation::disk_budget::test_seam::DiskCapacityFixtureGuard {
    let mut config = homeboy::core::defaults::load_config();
    config.retention.reconstructable_artifact_reserve_bytes = 0;
    homeboy::core::defaults::save_config(&config).expect("fixture capacity policy");
    homeboy::core::observation::disk_budget::test_seam::ample_disk_capacity_for_test(root)
}

fn linked_preview_workspace() -> (tempfile::TempDir, std::path::PathBuf) {
    let (repository, primary) = crate::test_support::shared_committed_git_repo_fixture("primary");
    let workspace = repository.path().join("task-worktree");
    crate::test_support::run_git_command(
        &primary,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "task",
            workspace.to_str().expect("UTF-8 workspace"),
        ],
    );
    (repository, workspace)
}

#[cfg(unix)]
#[test]
fn cook_repository_resolution_has_a_bounded_git_spawn_count() {
    let name = homeboy::core::test_support::harness_test_name(
        module_path!(),
        "cook_repository_resolution_git_spawn_child",
    );
    let output = homeboy::core::test_support::run_child_test(
        Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg(&name)
            .arg("--ignored"),
        &name,
    );
    assert!(output.status.success(), "git spawn regression: {output:?}");
}

#[test]
fn agent_task_component_preflight_materializes_repository_not_invocation_cwd() {
    crate::test_support::with_isolated_home(|_| {
        let (_fixture, checkout) =
            crate::test_support::shared_committed_git_repo_fixture("fixture");
        homeboy::core::component::write_standalone_component_config(
            &homeboy::core::component::Component {
                id: "fixture".into(),
                local_path: checkout.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
        let mut args = cook(&[
            "homeboy",
            "agent-task",
            "cook",
            "--repo",
            "fixture",
            "--task-url",
            "https://github.com/example/fixture/issues/17481",
            "--prompt",
            "x",
            "--verify",
            "homeboy review test fixture",
        ]);
        args.repository_identity = Some(serde_json::json!({
            "component_id": "fixture", "component_registered": true
        }));
        let workspace = cook_gate_preflight_workspace(&args).unwrap();
        assert_eq!(workspace, checkout);
        assert!(workspace.join(".git").exists());
        // Exercise the filesystem handoff, not just a serialized identity.
        crate::test_support::run_git_command(&workspace, &["rev-parse", "HEAD"]);
        let transfer = tempfile::NamedTempFile::new().unwrap();
        crate::test_support::run_git_command(
            &workspace,
            &["archive", "HEAD", "-o", transfer.path().to_str().unwrap()],
        );
        let materialized = tempfile::tempdir().unwrap();
        assert!(Command::new("tar")
            .arg("-xf")
            .arg(transfer.path())
            .arg("-C")
            .arg(materialized.path())
            .status()
            .unwrap()
            .success());
        assert!(std::fs::read_dir(materialized.path())
            .unwrap()
            .next()
            .is_some());
    });
}

#[cfg(unix)]
#[test]
#[ignore = "isolated PATH and CWD owned by parent test"]
fn cook_repository_resolution_git_spawn_child() {
    crate::test_support::with_isolated_home(|_| {
        let _lookup = issue_lookup_fixture();
        let (fixture, checkout) = crate::test_support::shared_committed_git_repo_fixture("fixture");
        let git = |args: &[&str]| crate::test_support::run_git_command(&checkout, args);
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/example/fixture.git",
        ]);
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ]);
        std::fs::write(checkout.join("homeboy.json"), r#"{"id":"fixture"}"#).unwrap();
        homeboy::core::component::write_standalone_component_config(
            &homeboy::core::component::Component {
                id: "fixture".into(),
                local_path: checkout.display().to_string(),
                remote_url: Some("https://github.com/example/fixture.git".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let real_git = Command::new("which").arg("git").output().unwrap();
        let real_git = String::from_utf8(real_git.stdout).unwrap();
        let bin = fixture.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let log = fixture.path().join("git.log");
        homeboy_test_support::write_executable(
            &bin.join("git"),
            &format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
                log.display(),
                real_git.trim(),
            ),
            0o755,
        )
        .unwrap();
        let _path = homeboy::core::test_support::EnvVarGuard::set(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap(),),
        );
        std::env::set_current_dir(&checkout).unwrap();
        let started = std::time::Instant::now();
        let (resolved, preview) = {
            let mut input = cook(&[
                "homeboy",
                "agent-task",
                "cook",
                "--repo",
                "fixture",
                "--task-url",
                "https://github.com/example/fixture/issues/16358",
                "--prompt",
                "x",
                "--no-finalize",
            ]);
            match crate::commands::agent_task::run::resolve_cook_repository(&mut input) {
                Ok(repository) => crate::commands::agent_task::run::resolve_cook_destination(
                    input,
                    repository.as_ref(),
                )
                .and_then(|args| {
                    crate::commands::agent_task::run::provision_cook_destination(
                        &args,
                        repository.as_ref(),
                        true,
                    )
                    .map(|provision| (args, provision))
                }),
                Err(error) => Err(error),
            }
        }
        .expect("plan destination");
        assert_eq!(resolved.base.as_deref(), Some("main"));
        assert_eq!(preview["action"], "planned_create");
        assert_eq!(
            preview["path"],
            fixture
                .path()
                .join("fixture@fix-issue-16358-fixture")
                .display()
                .to_string()
        );
        eprintln!("Cook preview elapsed: {:?}", started.elapsed());
        let calls = std::fs::read_to_string(log).unwrap();
        let count = calls.lines().count();
        eprintln!("Cook preview git spawns: {count}\n{calls}");
        assert!(
                count <= 10,
                "expected at most ten git spawns including source history preflight, got {count}:\n{calls}"
            );
        assert_eq!(
            calls
                .lines()
                .filter(|call| *call == "rev-parse --show-toplevel")
                .count(),
            1
        );
        assert_eq!(
            calls
                .lines()
                .filter(|call| *call == "remote get-url origin")
                .count(),
            2
        );
        std::env::set_current_dir(fixture.path()).unwrap();
    });
}

#[test]
fn preview_replay_rewrites_repository_and_component_identity_without_losing_flags() {
    let mut args = cook(&[
        "homeboy",
        "agent-task",
        "cook",
        "--repo",
        "php-transformer",
        "--prompt",
        "fix it",
    ]);
    args.dispatch.repo = Some("blocks-engine".to_string());
    args.component = Some("php-transformer".to_string());
    let mut replay = vec![
        "homeboy".to_string(),
        "--placement=lab".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--repo=php-transformer".to_string(),
        "--component".to_string(),
        "stale-component".to_string(),
        "--provider-argv".to_string(),
        "--repo".to_string(),
    ];

    rewrite_cook_identity_replay_argv(&mut replay, &args);

    assert!(replay.iter().any(|arg| arg == "--repo=blocks-engine"));
    assert!(replay
        .windows(2)
        .any(|parts| parts == ["--component", "php-transformer"]));
    assert!(replay
        .windows(2)
        .any(|parts| parts == ["--provider-argv", "--repo"]));
    assert!(replay.iter().any(|arg| arg == "--placement=lab"));
}

#[test]
fn preview_reports_destination_blockers_without_creating_homeboy_state() {
    crate::test_support::with_isolated_home(|home| {
        let before = std::fs::read_dir(home).expect("read isolated home").count();
        let error = preview_cook(
            cook(&[
                "homeboy",
                "agent-task",
                "cook",
                "--preview",
                "--prompt",
                "implement the issue",
                "--backend",
                "fixture",
                "--no-finalize",
            ]),
            None,
        )
        .expect_err("missing destination must block preview");

        assert_eq!(
            error.code,
            homeboy::core::ErrorCode::ValidationMissingArgument
        );
        assert_eq!(
            error.details["args"],
            serde_json::json!(["--repo <repo> is required when --to-worktree is omitted"])
        );
        assert_eq!(
            std::fs::read_dir(home).expect("read isolated home").count(),
            before,
            "preview must not create Homeboy state"
        );
    });
}

#[test]
fn preview_replay_argv_is_executable_cook_argv_without_preview() {
    let _lookup = issue_lookup_fixture();
    let args = cook(&[
        "homeboy",
        "agent-task",
        "cook",
        "--base",
        "main",
        "--preview",
        "--prompt",
        "implement the issue",
        "--repo",
        "homeboy",
        "--task-url",
        "https://github.com/Extra-Chill/homeboy/issues/12478",
        "--verify",
        "cargo test -p homeboy-cli",
    ]);
    let resolved = {
        let mut input = args;
        match crate::commands::agent_task::run::resolve_cook_repository(&mut input) {
            Ok(repository) => resolve_cook_destination(input, repository.as_ref()),
            Err(error) => Err(error),
        }
    }
    .expect("resolve issue destination");
    let replay = cook_preview_replay_argv(&resolved);
    assert!(
        !replay.argv.iter().any(|part| part == "--preview"),
        "{replay:?}"
    );
    assert!(replay
        .requires
        .iter()
        .any(|requirement| requirement.contains("runner placement admission is deferred")));
    Cli::try_parse_from(&replay.argv).expect("replay argv parses as Cook");
}

#[test]
fn preview_replay_omits_generated_lifecycle_ids() {
    let mut args = cook(&[
        "homeboy",
        "agent-task",
        "cook",
        "--preview",
        "--prompt",
        "implement the issue",
    ]);
    bind_cook_preview_lifecycle(&mut args);
    assert!(args.dispatch.run_id.is_some());
    assert!(args.attempt_run_id.is_some());

    let mut argv = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--prompt".to_string(),
        "implement the issue".to_string(),
    ];
    append_preview_lifecycle_replay_argv(&mut argv, &args);
    assert!(!preview_replay_has_flag(&argv, "--run-id"), "{argv:?}");
    assert!(
        !preview_replay_has_flag(&argv, "--attempt-run-id"),
        "{argv:?}"
    );

    let replay = finalize_cook_preview_replay(argv, &args);
    assert!(
        !preview_replay_has_flag(&replay.argv, "--run-id"),
        "{replay:?}"
    );
    assert!(
        !preview_replay_has_flag(&replay.argv, "--attempt-run-id"),
        "{replay:?}"
    );
}

#[test]
fn preview_replay_echoes_explicit_run_id() {
    let mut args = cook(&[
        "homeboy",
        "agent-task",
        "cook",
        "--preview",
        "--prompt",
        "implement the issue",
        "--run-id",
        "cook-42",
    ]);
    bind_cook_preview_lifecycle(&mut args);
    assert_eq!(args.dispatch.run_id.as_deref(), Some("cook-42"));

    let mut argv = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--prompt".to_string(),
        "implement the issue".to_string(),
    ];
    append_preview_lifecycle_replay_argv(&mut argv, &args);
    assert!(
        argv.windows(2)
            .any(|parts| parts == ["--run-id", "cook-42"]),
        "{argv:?}"
    );
    assert!(
        !preview_replay_has_flag(&argv, "--attempt-run-id"),
        "{argv:?}"
    );

    let replay = finalize_cook_preview_replay(argv, &args);
    assert!(
        replay
            .argv
            .windows(2)
            .any(|parts| parts == ["--run-id", "cook-42"]),
        "{replay:?}"
    );
    assert!(
        !preview_replay_has_flag(&replay.argv, "--attempt-run-id"),
        "{replay:?}"
    );
}

#[test]
fn cook_validation_uses_valid_fallback_without_mutating_durable_primary() {
    use homeboy::agents::agent_task_scheduler::{
        AgentTaskProviderRotationEntry, AgentTaskProviderRotationPolicy,
    };
    use homeboy::agents::agent_tasks::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPlan, AgentTaskPolicy, AgentTaskRequest,
        AgentTaskWorkspace, AGENT_TASK_REQUEST_SCHEMA,
    };

    let fake = FakePi::with_probes("exit 99", "printf 'test fallback\\n'");
    let provider = fake.provider();
    let catalog = homeboy::agents::agent_tasks::provider::AgentTaskProviderCatalog {
        providers: vec![provider],
        ..Default::default()
    };
    let request = AgentTaskRequest {
        schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
        task_id: "cook-model-fallback".to_string(),
        group_key: None,
        parent_plan_id: None,
        executor: AgentTaskExecutor {
            backend: "pi".to_string(),
            selector: None,
            runtime_selection: None,
            required_capabilities: Vec::new(),
            secret_env: Vec::new(),
            model: Some("invalid-primary".to_string()),
            config: fake.config(),
        },
        instructions: "run".to_string(),
        inputs: Value::Null,
        source_refs: Vec::new(),
        workspace: AgentTaskWorkspace::default(),
        component_contracts: Vec::new(),
        policy: AgentTaskPolicy::default(),
        limits: AgentTaskLimits::default(),
        expected_artifacts: Vec::new(),
        artifact_declarations: Vec::new(),
        output_declarations: Vec::new(),
        runtime_tools: Vec::new(),
        metadata: Value::Null,
    };
    let mut plan = AgentTaskPlan::new("cook-model-fallback", vec![request]);
    plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
        entries: vec![AgentTaskProviderRotationEntry {
            model: Some("test/fallback".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    });

    let selected = validate_cook_provider_execution_plan(&plan, &catalog)
        .expect("fallback route validates Cook");
    assert_eq!(
        selected.tasks[0].executor.model(),
        Some("test/fallback"),
        "compile validation returns the route readiness bound"
    );
    assert_eq!(
        selected.tasks[0].metadata["provider_readiness_routing"]["skipped"][0]["model"],
        "invalid-primary"
    );
    assert_eq!(plan.tasks[0].executor.model(), Some("invalid-primary"));
}

#[test]
fn preview_applies_argument_provenance_before_destination_resolution() {
    let mut provenance = crate::cli_surface::CommandArgumentProvenance::default();
    provenance.set(
        "no_finalize",
        crate::cli_surface::ArgumentSource::Configuration,
    );
    let error = preview_cook(
        cook(&[
            "homeboy",
            "agent-task",
            "cook",
            "--preview",
            "--prompt",
            "implement the issue",
            "--to-worktree",
            "missing@worktree",
            "--no-finalize",
        ]),
        Some(&provenance),
    )
    .expect_err("preview must enforce no-finalize provenance");
    assert!(
        error.message.contains("explicitly authorized"),
        "{}",
        error.message
    );
}

#[test]
fn preview_replay_redacts_opaque_provider_values_and_arbitrary_credential_names() {
    let replay = redact_preview_replay_argv(std::iter::once("homeboy".to_string()).chain([
        "agent-task".to_string(),
        "cook".to_string(),
        "--gate-env".to_string(),
        "TOKEN=secret-value".to_string(),
        "--provider-config".to_string(),
        r#"{"token":"secret-value"}"#.to_string(),
        "--private-verify".to_string(),
        "printf secret-value".to_string(),
        "--provider-argv".to_string(),
        "credential-without-a-secret-name".to_string(),
        "--runner-env=TOKEN=secret-value".to_string(),
    ]));
    assert!(
        !replay.argv.join(" ").contains("secret-value"),
        "{replay:?}"
    );
    assert_eq!(replay.argv[4], "REDACTED=<redacted:--gate-env>");
    assert_eq!(replay.argv[6], "<redacted:--provider-config>");
    assert_eq!(replay.argv[7], "--private-verify");
    assert_eq!(replay.argv[8], "<redacted:--private-verify>");
    assert_eq!(replay.argv[10], "<redacted:--provider-argv>");
    assert_eq!(replay.argv[11], "--runner-env=<redacted:--runner-env>");
    assert_eq!(replay.requires.len(), 5);
    Cli::try_parse_from(&replay.argv).expect("redacted replay remains parseable");
}

#[test]
fn preview_replay_truncates_only_at_parseable_clap_unit_boundaries() {
    let argv = std::iter::once("homeboy".to_string())
        .chain(["agent-task".to_string(), "cook".to_string()])
        .chain(
            (0..MAX_PREVIEW_REPLAY_ARGS)
                .flat_map(|index| ["--verify".to_string(), format!("true-{index}")]),
        )
        .collect::<Vec<_>>();
    let replay = redact_preview_replay_argv(argv);
    assert!(replay.argv.len() < MAX_PREVIEW_REPLAY_ARGS);
    assert!(replay
        .requires
        .iter()
        .any(|item| item.contains("safety budget")));
    for boundary in (3..=replay.argv.len()).step_by(2) {
        Cli::try_parse_from(&replay.argv[..boundary])
            .unwrap_or_else(|error| panic!("boundary {boundary} is not parseable: {error}"));
    }
}

#[test]
fn every_generated_cook_value_option_is_atomic_and_never_dangles() {
    for flag in cook_value_flags() {
        let (_, missing_requires) = clap_replay_units(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            flag.clone(),
        ]);
        assert!(
            missing_requires
                .iter()
                .any(|requirement| requirement.starts_with(&flag)),
            "missing value-taking flag {flag} was not withheld atomically"
        );

        let (units, requires) = clap_replay_units(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            flag.clone(),
            "placeholder".to_string(),
        ]);
        assert!(
            requires.is_empty(),
            "complete {flag} unexpectedly needs input"
        );
        assert!(
            units
                .iter()
                .any(|unit| unit == &[flag.clone(), "placeholder".to_string()]),
            "value-taking flag {flag} was not retained as one unit"
        );
    }
}

#[test]
fn root_value_options_and_aliases_are_atomic_in_separated_and_equals_forms() {
    let root_values = [
        ("--output", "preview.json"),
        ("--notification-transport", "webhook"),
        ("--notification-route", "https://example.test/hook"),
        ("--placement", "auto"),
        ("--artifact-root", "artifacts"),
        ("--runner", "runner-1"),
        ("--runner-env", "TOKEN=value"),
        ("--runner-secret-env", "TOKEN"),
        ("--lab-env-json", "{}"),
        ("--runner-workspace-root", "workspace"),
    ];
    let value_flags = cook_value_flags();
    for (flag, value) in root_values {
        assert!(value_flags.contains(flag), "missing root value flag {flag}");
        let (separated, requires) = clap_replay_units(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            flag.to_string(),
            value.to_string(),
        ]);
        assert!(requires.is_empty(), "{flag} separated form needs input");
        assert!(
            separated
                .iter()
                .any(|unit| unit == &[flag.to_string(), value.to_string()]),
            "{flag} separated form was not atomic"
        );

        let (equals, requires) = clap_replay_units(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            format!("{flag}={value}"),
        ]);
        assert!(requires.is_empty(), "{flag} equals form needs input");
        assert!(
            equals
                .iter()
                .any(|unit| unit == &[format!("{flag}={value}")]),
            "{flag} equals form was not atomic"
        );
    }
}

#[test]
fn root_value_units_are_never_split_at_any_replay_budget_boundary() {
    for flag in [
        "--output",
        "--notification-route",
        "--runner-env",
        "--lab-env-json",
    ] {
        let argv = std::iter::once("homeboy".to_string())
            .chain(["agent-task".to_string(), "cook".to_string()])
            .chain(
                (0..MAX_PREVIEW_REPLAY_ARGS)
                    .flat_map(|index| [flag.to_string(), format!("value-{index}")]),
            )
            .collect::<Vec<_>>();
        let replay = redact_preview_replay_argv(argv);
        assert_eq!(replay.argv.len() % 2, 1, "{flag} was split at the budget");
        assert!(replay
            .requires
            .iter()
            .any(|item| item.contains("safety budget")));
    }
}

#[test]
fn valid_root_value_forms_parse_before_and_after_cook() {
    for (flag, value) in [
        ("--output", "preview.json"),
        ("--placement", "local"),
        ("--artifact-root", "artifacts"),
        ("--runner-env", "TOKEN=value"),
        ("--runner-secret-env", "TOKEN"),
        ("--lab-env-json", "{}"),
        ("--runner-workspace-root", "workspace"),
    ] {
        Cli::try_parse_from(["homeboy", flag, value, "agent-task", "cook"])
            .unwrap_or_else(|error| panic!("separated {flag} must parse: {error}"));
        let equals = format!("{flag}={value}");
        Cli::try_parse_from(["homeboy", "agent-task", "cook", &equals])
            .unwrap_or_else(|error| panic!("equals {flag} must parse: {error}"));
    }
    for args in [
        vec![
            "homeboy",
            "--notification-transport",
            "webhook",
            "--notification-route",
            "https://example.test/hook",
            "agent-task",
            "cook",
        ],
        vec![
            "homeboy",
            "agent-task",
            "cook",
            "--notification-transport=webhook",
            "--notification-route=https://example.test/hook",
        ],
    ] {
        Cli::try_parse_from(args)
            .unwrap_or_else(|error| panic!("notification pair must parse: {error}"));
    }
}

#[test]
fn compiled_plan_preview_emits_placement_admission_schema() {
    crate::test_support::with_isolated_home(|_| {
        let (repository, workspace) = linked_preview_workspace();
        let _capacity = declare_preview_fixture_capacity(repository.path());
        let cli = Cli::try_parse_from([
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--base".to_string(),
            "main".to_string(),
            "--preview".to_string(),
            "--backend".to_string(),
            "fixture".to_string(),
            "--repo".to_string(),
            "fixture-repository".to_string(),
            "--prompt".to_string(),
            "-".to_string(),
            "--to-worktree".to_string(),
            workspace.to_str().expect("UTF-8 workspace").to_string(),
            "--no-finalize".to_string(),
            "--verify".to_string(),
            "true".to_string(),
        ])
        .expect("parse preview");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let super::super::AgentTaskCommand::Cook(mut args) = agent_task.command else {
            panic!("Cook command");
        };
        args.prompt_snapshot = Some(super::super::args::CookPromptSnapshot {
            content: "Inspect the task workspace.".to_string(),
            source: "stdin".to_string(),
            sha256: "sha256:fixture".to_string(),
            size_bytes: "Inspect the task workspace.".len(),
        });
        let (preview, exit_code) = preview_cook(*args, None).expect("compile preview");

        assert_eq!(exit_code, 0);
        assert_eq!(preview["schema"], "homeboy/agent-task-cook-preview/v1");
        assert_eq!(preview["mutates"], false);
        assert_eq!(
            preview["progress"],
            serde_json::json!([
                { "event": "cook_preview_progress", "phase": "prompt_input" },
                { "event": "cook_preview_progress", "phase": "input_validation" },
                { "event": "cook_preview_progress", "phase": "destination_resolution" },
                { "event": "cook_preview_progress", "phase": "base_preparation" },
                { "event": "cook_preview_progress", "phase": "placement_projection" },
                { "event": "cook_preview_progress", "phase": "gate_contract_validation" },
                { "event": "cook_preview_progress", "phase": "provider_preflight" },
                { "event": "cook_preview_progress", "phase": "plan_compilation" },
            ])
        );
        assert!(preview["resolved"]["provider"].is_object());
        assert_eq!(
            preview["resolved"]["placement"]["admission"]["schema"],
            "homeboy/cook-preview-placement-admission/v1"
        );
        assert!(matches!(
            preview["resolved"]["placement"]["admission"]["state"].as_str(),
            Some("admissible" | "blocked" | "indeterminate")
        ));
        assert_eq!(
            preview["resolved"]["placement"]["admission"]["revalidate_before_execution"],
            true
        );
        assert_eq!(
            preview["resolved"]["placement"]["admission"]["deferred_to"],
            "execution_placement_admission"
        );
    });
}

/// #17709: preview without `--prompt` runs the `--task-url` task exactly
/// as cook-batch children do.
#[test]
fn cook_preview_without_prompt_uses_the_task_url_prompt() {
    crate::test_support::with_isolated_home(|_| {
        let (repository, workspace) = linked_preview_workspace();
        let _capacity = declare_preview_fixture_capacity(repository.path());
        let task_url = "https://example.test/issues/17709";
        let args = cook(&[
            "homeboy",
            "agent-task",
            "cook",
            "--base",
            "main",
            "--preview",
            "--backend",
            "fixture",
            "--repo",
            "fixture-repository",
            "--task-url",
            task_url,
            "--to-worktree",
            workspace.to_str().expect("UTF-8 workspace"),
            "--no-finalize",
            "--verify",
            "true",
        ]);
        assert!(args.dispatch.prompt.is_none());
        let (preview, exit_code) = preview_cook(args.clone(), None).expect("compile preview");
        assert_eq!(exit_code, 0, "{preview}");

        // A queued admission replays the raw argv: preparation alone
        // derives the same prompt the preview ran, including on the Lab
        // controller, which carries the compiled attempt plan.
        let expected = super::super::cook_batch::default_task_prompt(task_url);
        for attempt_plan in [None, Some("{}".to_string())] {
            let mut live = args.clone();
            live.attempt_plan = attempt_plan;
            prepare_cook_request(&mut live, None).expect("prepare replayed Cook");
            assert_eq!(live.dispatch.prompt.as_deref(), Some(expected.as_str()));
        }
    });
}

/// #18467: `--goal` with `--task-url` dispatches the goal, not the URL,
/// in both the preview and the live (replayed) preparation paths.
#[test]
fn cook_with_task_url_and_goal_dispatches_the_goal() {
    crate::test_support::with_isolated_home(|_| {
        let (repository, workspace) = linked_preview_workspace();
        let _capacity = declare_preview_fixture_capacity(repository.path());
        let goal = "Fix only the second checklist item";
        let args = cook(&[
            "homeboy",
            "agent-task",
            "cook",
            "--base",
            "main",
            "--preview",
            "--backend",
            "fixture",
            "--repo",
            "fixture-repository",
            "--task-url",
            "https://example.test/issues/18467",
            "--goal",
            goal,
            "--to-worktree",
            workspace.to_str().expect("UTF-8 workspace"),
            "--no-finalize",
            "--verify",
            "true",
        ]);
        let mut preview_args = args.clone();
        default_cook_prompt_from_task_url(&mut preview_args);
        assert_eq!(
            resolved_dispatch_args_for_cook(&preview_args)
                .expect("resolve preview dispatch")
                .prompt
                .as_deref(),
            Some(goal)
        );
        let (preview, exit_code) = preview_cook(args.clone(), None).expect("compile preview");
        assert_eq!(exit_code, 0, "{preview}");

        for attempt_plan in [None, Some("{}".to_string())] {
            let mut live = args.clone();
            live.attempt_plan = attempt_plan;
            prepare_cook_request(&mut live, None).expect("prepare replayed Cook");
            assert_eq!(
                resolved_dispatch_args_for_cook(&live)
                    .expect("resolve live dispatch")
                    .prompt
                    .as_deref(),
                Some(goal)
            );
        }
    });
}

/// #17653: a Cook without a repro warns in its preview that its gates do
/// not prove the issue; a declared repro is shown instead of the warning.
#[test]
fn preview_warns_when_no_repro_gate_is_declared() {
    crate::test_support::with_isolated_home(|_| {
        let (repository, workspace) = linked_preview_workspace();
        let _capacity = declare_preview_fixture_capacity(repository.path());
        let preview = |extra: &[&str]| {
            let mut argv = vec![
                "homeboy",
                "agent-task",
                "cook",
                "--base",
                "main",
                "--preview",
                "--backend",
                "fixture",
                "--repo",
                "fixture-repository",
                "--prompt",
                "-",
                "--to-worktree",
                workspace.to_str().expect("UTF-8 workspace"),
                "--no-finalize",
                "--verify",
                "true",
            ];
            argv.extend_from_slice(extra);
            let cli = Cli::try_parse_from(argv).expect("parse preview");
            let Commands::AgentTask(agent_task) = cli.command else {
                panic!("agent-task command");
            };
            let super::super::AgentTaskCommand::Cook(mut args) = agent_task.command else {
                panic!("Cook command");
            };
            args.prompt_snapshot = Some(super::super::args::CookPromptSnapshot {
                content: "Inspect the task workspace.".to_string(),
                source: "stdin".to_string(),
                sha256: "sha256:fixture".to_string(),
                size_bytes: "Inspect the task workspace.".len(),
            });
            preview_cook(*args, None).expect("compile preview").0
        };

        let unproven = preview(&[]);
        let gates = &unproven["resolved"]["gates"];
        assert_eq!(gates["repro"], serde_json::json!([]), "{gates}");
        let warning = gates["repro_warning"].as_str().unwrap_or_default();
        assert!(
            warning.contains("do not prove this issue") && warning.contains("repro: none"),
            "{gates}"
        );

        let proven = preview(&["--repro", "test -f FIXED"]);
        let gates = &proven["resolved"]["gates"];
        assert_eq!(
            gates["repro"],
            serde_json::json!(["test -f FIXED"]),
            "{gates}"
        );
        assert!(gates["repro_warning"].is_null(), "{gates}");
    });
}

#[path = "run_preview_admission_tests.rs"]
mod admission;
