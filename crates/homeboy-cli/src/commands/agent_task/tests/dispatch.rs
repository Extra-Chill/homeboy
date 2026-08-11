//! Agent-task command from-spec dispatch defaults and controller dispatch arg tests.

use super::support::*;
use clap::Parser;
use homeboy::agents::agent_task_service::{
    AgentTaskCookAttemptDispatcher, DerivedCookBaselineCapability,
};
use homeboy::core::{Error, Result};

use crate::cli_surface::{ArgumentSource, Cli, Commands};

use super::super::AgentTaskCommand;

#[derive(Debug)]
struct RecipeOnlyDispatcher;

impl AgentTaskCookAttemptDispatcher for RecipeOnlyDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(json!({ "kind": "recipe-only" }))
    }

    fn dispatch_attempt(
        &self,
        _plan: AgentTaskPlan,
        _run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        Err(Error::internal_unexpected("stop after durable recipe"))
    }
}

fn cook_args_from_cli(args: Vec<String>) -> AgentTaskCookArgs {
    let cli = Cli::parse_from(args);
    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("agent-task command");
    };
    let AgentTaskCommand::Cook(cook) = agent_task.command else {
        panic!("cook command");
    };
    *cook
}

fn register_component(id: &str, path: &std::path::Path, remote_url: &str) {
    homeboy::core::component::write_standalone_component_config(
        &homeboy::core::component::Component {
            id: id.to_string(),
            local_path: path.display().to_string(),
            remote_url: Some(remote_url.to_string()),
            ..Default::default()
        },
    )
    .expect("register component");
}

fn add_remote(path: &std::path::Path, name: &str, remote_url: &str) {
    let status = Command::new("git")
        .args(["remote", "add", name, remote_url])
        .current_dir(path)
        .status()
        .expect("add fixture remote");
    assert!(status.success(), "git remote add {name} failed");
}

#[test]
fn cook_derives_issue_destination_and_preserves_explicit_override() {
    with_isolated_home(|_| {
        let derived = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the issue".to_string(),
            "--repo".to_string(),
            "homeboy".to_string(),
            "--task-url".to_string(),
            "https://github.com/Extra-Chill/homeboy/issues/11225".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("derive Cook destination");
        assert_eq!(
            derived.to_worktree.as_deref(),
            Some("homeboy@fix-issue-11225-homeboy")
        );
        assert_eq!(derived.head.as_deref(), Some("fix/issue-11225-homeboy"));

        let suffixed = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the issue".to_string(),
            "--repo".to_string(),
            "homeboy".to_string(),
            "--task-url".to_string(),
            "https://github.com/Extra-Chill/homeboy/issues/11225?source=cook#details".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("derive Cook destination from suffixed issue URL");
        assert_eq!(suffixed.to_worktree, derived.to_worktree);
        assert_eq!(suffixed.head, derived.head);

        let explicit = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the issue".to_string(),
            "--to-worktree".to_string(),
            "homeboy@caller-selected".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("preserve explicit destination");
        assert_eq!(
            explicit.to_worktree.as_deref(),
            Some("homeboy@caller-selected")
        );
        assert_eq!(explicit.head, None);
    });
}

#[test]
fn cook_infers_repo_from_an_explicit_git_workspace_and_persists_its_provenance() {
    with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source checkout");
        init_runtime_component_checkout(source.path());
        let remote = "https://github.com/example/inferred.git";
        add_remote(source.path(), "origin", remote);
        register_component("inferred", source.path(), remote);
        let destination_root = tempfile::tempdir().expect("destination root");
        let destination = destination_root.path().join("task");
        let status = Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "fix/inferred",
                destination.to_str().expect("destination path"),
                "HEAD",
            ])
            .current_dir(source.path())
            .status()
            .expect("create linked worktree");
        assert!(status.success(), "create linked worktree");

        let args = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--workspace".to_string(),
            source.path().display().to_string(),
            "--to-worktree".to_string(),
            destination.display().to_string(),
            "--backend".to_string(),
            "fixture".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("infer repository from workspace");
        assert_eq!(args.dispatch.repo.as_deref(), Some("inferred"));
        assert_eq!(
            args.repository_identity.as_ref().expect("identity")["remote_identity"],
            "git://github.com/example/inferred"
        );
        assert_eq!(
            args.repository_identity.as_ref().expect("identity")["provenance"],
            "--workspace:git-remote:origin"
        );

        let plan = super::super::run::compile_cook_plan(&args, json!({ "path": destination }))
            .expect("compile Cook plan");
        assert_eq!(
            plan.metadata["cook_repository_identity"],
            args.repository_identity.expect("identity")
        );
    });
}

#[test]
fn cook_rejects_ambiguous_or_mismatching_workspace_repository_identity() {
    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        add_remote(
            workspace.path(),
            "origin",
            "https://github.com/example/one.git",
        );
        add_remote(
            workspace.path(),
            "upstream",
            "https://github.com/example/two.git",
        );
        register_component(
            "one",
            workspace.path(),
            "https://github.com/example/one.git",
        );
        register_component(
            "two",
            workspace.path(),
            "https://github.com/example/two.git",
        );

        let ambiguous = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--workspace".to_string(),
            workspace.path().display().to_string(),
            "--to-worktree".to_string(),
            workspace.path().display().to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect_err("multiple configured remotes are ambiguous");
        assert!(
            ambiguous
                .message
                .contains("cannot select one conflicting checkout"),
            "{}",
            ambiguous.message
        );
        assert!(
            ambiguous.message.contains("git://github.com/example/one"),
            "{}",
            ambiguous.message
        );
        assert_eq!(ambiguous.details["field"], "workspace");

        let mismatch_workspace = tempfile::tempdir().expect("mismatch workspace");
        init_runtime_component_checkout(mismatch_workspace.path());
        add_remote(
            mismatch_workspace.path(),
            "origin",
            "https://github.com/example/one.git",
        );
        let mismatch = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--workspace".to_string(),
            mismatch_workspace.path().display().to_string(),
            "--to-worktree".to_string(),
            mismatch_workspace.path().display().to_string(),
            "--repo".to_string(),
            "other".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect_err("explicit repo must match workspace identity");
        assert!(
            mismatch.message.contains("does not match"),
            "{}",
            mismatch.message
        );
        assert!(
            mismatch
                .message
                .contains("one (git://github.com/example/one"),
            "{}",
            mismatch.message
        );
    });
}

#[test]
fn cook_requires_repo_for_an_explicit_non_git_workspace() {
    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        let error = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--cwd".to_string(),
            workspace.path().display().to_string(),
            "--to-worktree".to_string(),
            workspace.path().display().to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect_err("non-Git workspace cannot infer a repository");
        assert_eq!(
            error.details["args"][0],
            "--repo <repo> is required because the supplied workspace is not a Git checkout with a configured repository remote; provide --repo <configured-component>"
        );
    });
}

#[test]
fn cook_rejects_conflicting_workspace_and_cwd_even_with_an_explicit_repo() {
    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        let cwd = tempfile::tempdir().expect("cwd");
        init_runtime_component_checkout(workspace.path());
        init_runtime_component_checkout(cwd.path());
        add_remote(
            workspace.path(),
            "origin",
            "https://github.com/example/one.git",
        );
        add_remote(cwd.path(), "origin", "https://github.com/example/two.git");
        register_component(
            "one",
            workspace.path(),
            "https://github.com/example/one.git",
        );
        register_component("two", cwd.path(), "https://github.com/example/two.git");

        for repo in ["one", "two"] {
            let error = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--prompt".to_string(),
                "implement the fix".to_string(),
                "--workspace".to_string(),
                workspace.path().display().to_string(),
                "--cwd".to_string(),
                cwd.path().display().to_string(),
                "--to-worktree".to_string(),
                workspace.path().display().to_string(),
                "--repo".to_string(),
                repo.to_string(),
                "--no-finalize".to_string(),
            ]))
            .expect_err("explicit repo cannot select a conflicting checkout");
            assert!(error
                .message
                .contains("cannot select one conflicting checkout"));
            assert!(error.message.contains("git://github.com/example/one"));
            assert!(error.message.contains("git://github.com/example/two"));
        }
    });
}

#[test]
fn cook_rejects_destination_with_a_different_repository_identity() {
    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        let destination = tempfile::tempdir().expect("destination");
        init_runtime_component_checkout(workspace.path());
        init_runtime_component_checkout(destination.path());
        add_remote(
            workspace.path(),
            "origin",
            "https://github.com/example/one.git",
        );
        add_remote(
            destination.path(),
            "origin",
            "https://github.com/example/two.git",
        );
        register_component(
            "one",
            workspace.path(),
            "https://github.com/example/one.git",
        );
        register_component(
            "two",
            destination.path(),
            "https://github.com/example/two.git",
        );
        let args = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--workspace".to_string(),
            workspace.path().display().to_string(),
            "--to-worktree".to_string(),
            destination.path().display().to_string(),
            "--backend".to_string(),
            "fixture".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("resolve source identity");

        let error =
            super::super::run::compile_cook_plan(&args, json!({ "path": destination.path() }))
                .expect_err("destination remote must match Cook repository identity");
        assert!(error
            .message
            .contains("does not match resolved `git://github.com/example/one`"));
        assert!(error.message.contains("git://github.com/example/two"));
    });
}

#[test]
fn cook_infers_an_ssh_scheme_remote_and_redacts_credentials_from_provenance() {
    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        add_remote(
            workspace.path(),
            "origin",
            "ssh://git@github.com/example/secure.git",
        );
        register_component(
            "secure",
            workspace.path(),
            "https://github.com/example/secure.git",
        );

        let args = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--cwd".to_string(),
            workspace.path().display().to_string(),
            "--to-worktree".to_string(),
            workspace.path().display().to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("infer ssh remote");
        assert_eq!(args.dispatch.repo.as_deref(), Some("secure"));
        assert_eq!(
            args.repository_identity.expect("identity")["remote_identity"],
            "git://github.com/example/secure"
        );

        let credentialed = "https://token:secret@github.com/example/secure.git";
        let status = Command::new("git")
            .args(["remote", "set-url", "origin", credentialed])
            .current_dir(workspace.path())
            .status()
            .expect("set credentialed fixture remote");
        assert!(status.success(), "set credentialed fixture remote");
        let credentialed_args =
            super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--prompt".to_string(),
                "implement the fix".to_string(),
                "--cwd".to_string(),
                workspace.path().display().to_string(),
                "--to-worktree".to_string(),
                workspace.path().display().to_string(),
                "--no-finalize".to_string(),
            ]))
            .expect("infer credentialed remote without persisting its credential");
        assert!(!credentialed_args
            .repository_identity
            .expect("identity")
            .to_string()
            .contains("secret"));
        assert_eq!(
            super::super::run::canonical_remote_identity(credentialed).as_deref(),
            Some("git://github.com/example/secure")
        );
        assert!(!super::super::run::canonical_remote_identity(credentialed)
            .expect("canonical identity")
            .contains("secret"));
    });
}

#[test]
fn cook_provisioning_rejects_an_unresolved_destination_without_panicking() {
    with_isolated_home(|_| {
        let args = cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the issue".to_string(),
            "--no-finalize".to_string(),
        ]);

        let error = super::super::run::provision_cook_destination(&args)
            .expect_err("an unresolved destination must fail closed");

        assert_eq!(
            error.details["args"][0],
            "--to-worktree is required before provisioning a Cook destination"
        );
    });
}

#[test]
fn cook_rejects_queue_only_before_creating_a_durable_recipe() {
    with_isolated_home(|_| {
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "missing@worktree",
            "--verify",
            "true",
            "--queue-only",
            "--run-id",
            "cook-queue-only",
        ]);
        let Commands::AgentTask(args) = cli.command else {
            panic!("agent-task cook command");
        };
        let AgentTaskCommand::Cook(cook) = args.command else {
            panic!("cook command");
        };

        let error = super::super::run::run_cook_with_executor(
            *cook,
            ExtensionProviderAgentTaskExecutor::default(),
        )
        .expect_err("queue-only cook must fail before resolving its worktree");

        assert!(error
            .message
            .contains("cannot queue its controller-owned lifecycle"));
        assert!(!homeboy::agents::agent_task_service::recipe_exists("cook-queue-only").unwrap());
    });
}

#[test]
fn cook_rejects_non_cli_no_finalize_authorization_before_provisioning() {
    let matches = Cli::command_with_scoped_lab_args()
        .try_get_matches_from([
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "implement the fix",
            "--to-worktree",
            "missing@worktree",
            "--no-finalize",
        ])
        .expect("parse Cook");
    let (compiled, _) = Cli::compile_registered_arg_matches(&matches).expect("compile Cook");
    let Commands::AgentTask(agent_task) = compiled.value.command else {
        panic!("agent-task Cook command");
    };
    let AgentTaskCommand::Cook(cook) = agent_task.command else {
        panic!("Cook command");
    };
    let mut provenance = compiled.provenance;
    provenance.set("no_finalize", ArgumentSource::Configuration);

    let error = super::super::run::validate_cook_request_with_provenance(&cook, Some(&provenance))
        .expect_err("configuration cannot authorize finalization suppression");

    assert!(error.message.contains("explicitly authorized"));
}

#[test]
fn cook_plan_records_compiled_argument_provenance() {
    let mut plan = AgentTaskPlan::new("cook-provenance", Vec::new());
    let mut provenance = crate::cli_surface::CommandArgumentProvenance::default();
    provenance.set("base", ArgumentSource::Configuration);
    provenance.set("run_id", ArgumentSource::Generated);

    super::super::run::record_cook_argument_provenance(&mut plan, &provenance);

    assert_eq!(
        plan.metadata["command_argument_provenance"]["base"],
        "configuration"
    );
    assert_eq!(
        plan.metadata["command_argument_provenance"]["run_id"],
        "generated"
    );
}

#[test]
fn cook_goal_frames_explicit_task_without_creating_another_provider_cell() {
    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--goal",
            "Preserve the durable provider-cell contract",
            "--task",
            "Implement the targeted repair",
            "--cwd",
            workspace.path().to_str().expect("workspace path"),
            "--to-worktree",
            workspace.path().to_str().expect("workspace path"),
            "--backend",
            "fixture",
            "--no-finalize",
            "--run-id",
            "cook-goal-task-one-cell",
        ]);
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task cook command");
        };
        let AgentTaskCommand::Cook(cook) = agent_task.command else {
            panic!("cook command");
        };

        let _ = run_cook_with_executor_and_dispatcher(
            *cook,
            CapturingExecutor::default(),
            Some(Arc::new(RecipeOnlyDispatcher)),
        );

        let recipe = homeboy::agents::agent_task_service::load_recipe("cook-goal-task-one-cell")
            .expect("durable cook recipe");
        let plan = &recipe.attempts[0].plan;
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.options.execution_budget.max_provider_executions, 1);
        assert_eq!(
            plan.metadata["cook_goal"],
            "Preserve the durable provider-cell contract"
        );
        assert_eq!(plan.tasks[0].instructions, "Implement the targeted repair");
        assert_eq!(
            plan.tasks[0].metadata["cook_goal"],
            "Preserve the durable provider-cell contract"
        );
    });
}

#[test]
fn cook_goal_without_explicit_work_remains_one_provider_cell() {
    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--goal",
            "Implement the targeted repair",
            "--cwd",
            workspace.path().to_str().expect("workspace path"),
            "--to-worktree",
            workspace.path().to_str().expect("workspace path"),
            "--backend",
            "fixture",
            "--no-finalize",
            "--run-id",
            "cook-goal-only-one-cell",
        ]);
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task cook command");
        };
        let AgentTaskCommand::Cook(cook) = agent_task.command else {
            panic!("cook command");
        };

        let _ = run_cook_with_executor_and_dispatcher(
            *cook,
            CapturingExecutor::default(),
            Some(Arc::new(RecipeOnlyDispatcher)),
        );

        let recipe = homeboy::agents::agent_task_service::load_recipe("cook-goal-only-one-cell")
            .expect("durable cook recipe");
        let plan = &recipe.attempts[0].plan;
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].instructions, "Implement the targeted repair");
        assert_eq!(
            plan.tasks[0].metadata["cook_goal"],
            "Implement the targeted repair"
        );
    });
}

#[test]
fn cook_goal_never_becomes_an_extra_prompt_when_work_is_explicit() {
    for (work_flag, work_value) in [
        ("--prompt", "implement the repair"),
        ("--tasks", "@provider-cells.json"),
    ] {
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--goal",
            "Preserve one cell per explicit task",
            work_flag,
            work_value,
            "--to-worktree",
            "homeboy@provider-cells",
            "--backend",
            "fixture",
            "--no-finalize",
        ]);
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task cook command");
        };
        let AgentTaskCommand::Cook(cook) = agent_task.command else {
            panic!("cook command");
        };

        let dispatch = super::super::run::dispatch_args_for_cook(&cook);
        assert_eq!(
            dispatch.prompt.as_deref(),
            (work_flag == "--prompt").then_some(work_value)
        );
        assert_eq!(
            dispatch.core.tasks_json.as_deref(),
            (work_flag == "--tasks").then_some(work_value)
        );
    }
}

#[cfg(unix)]
#[test]
fn invalid_cook_inputs_do_not_mutate_a_configured_provider_destination() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("provider tempdir");
        let ensured = temp.path().join("ensured");
        let provider = temp.path().join("provider");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nif [ \"$1\" = resolve ]; then\n  printf '%s\\n' '{{\"worktrees\":[]}}'\nelse\n  touch '{}'\nfi\n",
                ensured.display()
            ),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");

        let mut config = homeboy::core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy::core::defaults::WorktreeProviderCommands {
                    resolve: Some(vec![
                        provider.display().to_string(),
                        "resolve".to_string(),
                        "{handle}".to_string(),
                    ]),
                    ensure: Some(vec![provider.display().to_string(), "ensure".to_string()]),
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
                        task_url: None,
                    },
                ),
            },
        );
        homeboy::core::defaults::save_config(&config).expect("save provider config");

        for invalid_args in [vec!["--queue-only"], vec![]] {
            let mut args = vec![
                "homeboy",
                "agent-task",
                "cook",
                "--prompt",
                "exercise validation",
                "--to-worktree",
                "missing@destination",
                "--repo",
                "homeboy",
                "--base",
                "main",
                "--head",
                "fix/9908",
                "--task-url",
                "https://github.com/Extra-Chill/homeboy/issues/9908",
            ];
            args.extend(invalid_args);
            let cli = Cli::parse_from(args);
            let Commands::AgentTask(agent_task) = cli.command else {
                panic!("agent-task command")
            };
            let AgentTaskCommand::Cook(cook) = agent_task.command else {
                panic!("cook command")
            };
            run_cook_with_executor(*cook, ExtensionProviderAgentTaskExecutor::default())
                .expect_err("invalid Cook input is rejected before provider ensure");
        }
        assert!(
            !ensured.exists(),
            "invalid Cook inputs must cause zero ensure mutations"
        );
    });
}

#[cfg(unix)]
#[test]
fn cook_resolves_existing_provider_destination_without_creation_metadata() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        let destination_root = tempfile::tempdir().expect("destination root");
        let destination = destination_root.path().join("task");
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "fix/existing",
                destination.to_str().expect("destination path"),
                "HEAD",
            ])
            .current_dir(workspace.path())
            .status()
            .expect("create linked worktree")
            .success());
        let provider_dir = tempfile::tempdir().expect("provider dir");
        let provider = provider_dir.path().join("provider");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"fixture@existing\",\"path\":\"{}\",\"branch\":\"fix/existing\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n",
                destination.display()
            ),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");

        let mut config = homeboy::core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                lookup_output_limit_bytes: 64 * 1024,
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
                        task_url: None,
                    },
                ),
            },
        );
        homeboy::core::defaults::save_config(&config).expect("save provider config");

        let args = cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "reuse destination".to_string(),
            "--to-worktree".to_string(),
            "fixture@existing".to_string(),
            "--no-finalize".to_string(),
        ]);
        let provision = super::super::run::provision_cook_destination(&args)
            .expect("existing provider destination resolves without creation fields");

        assert_eq!(provision["action"], "existing");
        assert_eq!(provision["provider"], "fixture");
        assert_eq!(provision["path"], destination.display().to_string());
    });
}

#[cfg(unix)]
#[test]
fn cook_cwd_is_authoritative_when_provider_lookup_times_out() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let primary = tempfile::tempdir().expect("primary checkout");
        init_runtime_component_checkout(primary.path());
        let worktree_root = tempfile::tempdir().expect("worktree root");
        let cwd = worktree_root.path().join("task");
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "fix/cwd-authority",
                cwd.to_str().expect("worktree path"),
                "HEAD",
            ])
            .current_dir(primary.path())
            .status()
            .expect("create linked worktree")
            .success());
        homeboy::core::worktree::adopt(homeboy::core::worktree::WorktreeAdoptOptions {
            handle: "fixture@cwd-authority".to_string(),
            path: cwd.display().to_string(),
            kind: Some("test".to_string()),
            provenance: None,
        })
        .expect("register linked worktree");

        let provider = tempfile::NamedTempFile::new().expect("provider file");
        std::fs::write(provider.path(), "#!/bin/sh\nsleep 2\n").expect("write provider");
        let mut permissions = std::fs::metadata(provider.path())
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(provider.path(), permissions).expect("make provider executable");
        let mut config = homeboy::core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "timeout".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 1,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy::core::defaults::WorktreeProviderCommands {
                    resolve: Some(vec![provider.path().display().to_string()]),
                    ..Default::default()
                },
                list_result_mapping: None,
            },
        );
        homeboy::core::defaults::save_config(&config).expect("save provider config");

        let args = cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "reuse supplied worktree".to_string(),
            "--cwd".to_string(),
            cwd.display().to_string(),
            "--to-worktree".to_string(),
            "fixture@cwd-authority".to_string(),
            "--no-finalize".to_string(),
        ]);
        let provision = super::super::run::provision_cook_destination(&args)
            .expect("provider timeout must not block an explicit cwd");

        assert_eq!(provision["kind"], "explicit_cwd");
        assert_eq!(
            provision["path"],
            std::fs::canonicalize(&cwd)
                .expect("canonical cwd")
                .display()
                .to_string()
        );
    });
}

#[test]
fn cook_rejects_mismatched_cwd_and_destination() {
    with_isolated_home(|_| {
        let primary = tempfile::tempdir().expect("primary checkout");
        init_runtime_component_checkout(primary.path());
        let worktree_root = tempfile::tempdir().expect("worktree root");
        let cwd = worktree_root.path().join("cwd");
        let destination = worktree_root.path().join("destination");
        for (path, branch) in [(&cwd, "fix/cwd"), (&destination, "fix/destination")] {
            assert!(Command::new("git")
                .args([
                    "worktree",
                    "add",
                    "-b",
                    branch,
                    path.to_str().expect("worktree path"),
                    "HEAD"
                ])
                .current_dir(primary.path())
                .status()
                .expect("create linked worktree")
                .success());
        }
        let args = cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "reject mismatch".to_string(),
            "--cwd".to_string(),
            cwd.display().to_string(),
            "--to-worktree".to_string(),
            destination.display().to_string(),
            "--no-finalize".to_string(),
        ]);

        let error = super::super::run::provision_cook_destination(&args)
            .expect_err("mismatched worktrees must fail");
        assert_eq!(error.details["field"], "to_worktree");
        assert!(error
            .message
            .contains("must resolve to the same linked task worktree"));
    });
}

#[test]
fn cook_rejects_an_inactive_managed_destination_before_provider_execution() {
    #[derive(Clone, Default)]
    struct CountingExecutor(std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl AgentTaskExecutorAdapter for CountingExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            AgentTaskOutcome {
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::Succeeded,
                ..Default::default()
            }
        }
    }

    with_isolated_home(|_| {
        let primary = tempfile::tempdir().expect("primary checkout");
        init_runtime_component_checkout(primary.path());
        register_component(
            "fixture",
            primary.path(),
            "https://github.com/example/fixture.git",
        );
        let created =
            homeboy::core::worktree::create(homeboy::core::worktree::WorktreeCreateOptions {
                component_id: "fixture".to_string(),
                branch: "fix/inactive".to_string(),
                from: None,
                task_url: None,
                run_id: None,
                cleanup_policy: None,
            })
            .expect("create managed worktree");
        let cwd = created.record.worktree_path.clone();
        let handle = created.record.id.clone();
        homeboy::core::worktree::remove(homeboy::core::worktree::WorktreeRemoveOptions {
            id: handle.clone(),
            force: true,
            cleanup_branch: false,
            allow_unmerged_branch: false,
        })
        .expect("remove managed worktree");

        let args = cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "reject inactive destination".to_string(),
            "--repo".to_string(),
            "fixture".to_string(),
            "--backend".to_string(),
            "fixture".to_string(),
            "--cwd".to_string(),
            cwd,
            "--to-worktree".to_string(),
            handle,
            "--no-finalize".to_string(),
        ]);
        let executor = CountingExecutor::default();
        let error = run_cook_with_executor(args, executor.clone())
            .expect_err("inactive managed destination must fail before execution");

        assert_eq!(error.details["field"], "to_worktree");
        assert!(error.message.contains("is no longer active"));
        assert_eq!(executor.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    });
}

#[cfg(unix)]
#[test]
fn cook_does_not_collapse_provider_lookup_failures_into_missing_destination_metadata() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("provider tempdir");
        let ensured = temp.path().join("ensured");
        let provider = temp.path().join("provider");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nif [ \"$1\" = resolve ]; then exit 77; fi\ntouch '{}'\n",
                ensured.display()
            ),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");

        let mut config = homeboy::core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy::core::defaults::WorktreeProviderCommands {
                    resolve: Some(vec![provider.display().to_string(), "resolve".to_string()]),
                    ensure: Some(vec![provider.display().to_string(), "ensure".to_string()]),
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
                        task_url: None,
                    },
                ),
            },
        );
        homeboy::core::defaults::save_config(&config).expect("save provider config");

        let args = cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "resolve destination".to_string(),
            "--to-worktree".to_string(),
            "fixture@missing".to_string(),
            "--no-finalize".to_string(),
        ]);
        let error = super::super::run::provision_cook_destination(&args)
            .expect_err("provider lookup failure is not a missing destination");

        assert!(error
            .message
            .contains("provider `fixture` resolve command failed"));
        assert!(!ensured.exists(), "failed lookup must not run ensure");
    });
}

#[test]
fn from_spec_dispatch_defaults_use_spec_git_checkout() {
    let repo = tempfile::tempdir().expect("repo dir");
    let git_status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .arg("init")
        .status()
        .expect("git init runs");
    assert!(git_status.success());
    let spec_dir = repo.path().join(".github/homeboy/controllers");
    std::fs::create_dir_all(&spec_dir).expect("spec dir");
    let spec_path = spec_dir.join("loop.json");
    std::fs::write(&spec_path, "{}").expect("spec file");
    let mut spec = AgentTaskRepoLoopSpec {
        schema: None,
        loop_id: "repo-loop-cli-defaults".to_string(),
        phase: "init".to_string(),
        config_version: "v1".to_string(),
        metadata: Value::Null,
        entities: Vec::new(),
        agents: Vec::new(),
        tools: Vec::new(),
        abilities: Vec::new(),
        workflows: Vec::new(),
        artifacts: Vec::new(),
        artifact_graph: Vec::new(),
        dependencies: Vec::new(),
        gates: Vec::new(),
        metrics: Vec::new(),
        gate_bundles: Vec::new(),
        policy: None,
        phases: Vec::new(),
        actions: Vec::new(),
        initial_event: None,
    };

    apply_from_spec_dispatch_defaults(&mut spec, &format!("@{}", spec_path.display()));
    let expected_root = std::fs::canonicalize(repo.path()).expect("canonical repo path");

    assert_eq!(
        spec.metadata["dispatch_defaults"]["cwd"],
        expected_root.display().to_string()
    );
    assert_eq!(
        spec.metadata["dispatch_defaults"]["repo"],
        repo.path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string()
    );
}

#[test]
fn from_spec_dispatch_defaults_fall_back_to_current_git_checkout() {
    let repo = tempfile::tempdir().expect("repo dir");
    let git_status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .arg("init")
        .status()
        .expect("git init runs");
    assert!(git_status.success());
    let mut spec = AgentTaskRepoLoopSpec {
        schema: None,
        loop_id: "repo-loop-cli-cwd-defaults".to_string(),
        phase: "init".to_string(),
        config_version: "v1".to_string(),
        metadata: Value::Null,
        entities: Vec::new(),
        agents: Vec::new(),
        tools: Vec::new(),
        abilities: Vec::new(),
        workflows: Vec::new(),
        artifacts: Vec::new(),
        artifact_graph: Vec::new(),
        dependencies: Vec::new(),
        gates: Vec::new(),
        metrics: Vec::new(),
        gate_bundles: Vec::new(),
        policy: None,
        phases: Vec::new(),
        actions: Vec::new(),
        initial_event: None,
    };
    spec.workflows.push(
        homeboy::agents::agent_tasks::controller_service::AgentTaskRepoLoopSpecWorkflow {
            workflow_id: "store-idea".to_string(),
            agent_id: None,
            prompt: Some("cook the next workflow".to_string()),
            tasks: Vec::new(),
            entity_ids: Vec::new(),
            fan_out: None,
            tools: Vec::new(),
            abilities: Vec::new(),
            artifacts: Vec::new(),
            consumes: Vec::new(),
            emits: Vec::new(),
            dependencies: Vec::new(),
            gates: Vec::new(),
            metrics: Vec::new(),
            runtime_execution: Value::Null,
            inputs: Value::Null,
        },
    );

    apply_from_spec_dispatch_defaults_with_cwd(&mut spec, "-", || Some(repo.path().to_path_buf()));
    let expected_root = std::fs::canonicalize(repo.path()).expect("canonical repo path");
    let expected_repo = repo
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    assert_eq!(
        spec.metadata["dispatch_defaults"]["cwd"],
        expected_root.display().to_string()
    );
    assert_eq!(spec.metadata["dispatch_defaults"]["repo"], expected_repo);

    with_isolated_home(|_| {
        let report = agent_task_controller_service::init_from_spec(ControllerFromSpecRequest {
            spec: spec.clone(),
        })
        .expect("from-spec initialized");
        match &report.actions[0].action {
            AgentTaskLoopPolicyAction::SpawnTask { request, .. } => {
                assert_eq!(
                    request["dispatch"]["cwd"].as_str(),
                    Some(expected_root.display().to_string().as_str())
                );
                assert_eq!(
                    request["dispatch"]["repo"].as_str(),
                    Some(expected_repo.as_str())
                );
            }
            other => panic!("expected compiled spawn task, got {other:?}"),
        }
    });
}

#[test]
fn from_spec_dispatch_defaults_replace_stale_workspace_cwd() {
    let repo = tempfile::tempdir().expect("repo dir");
    let git_status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .arg("init")
        .status()
        .expect("git init runs");
    assert!(git_status.success());
    let mut spec = AgentTaskRepoLoopSpec {
        schema: None,
        loop_id: "repo-loop-stale-cwd-defaults".to_string(),
        phase: "init".to_string(),
        config_version: "v1".to_string(),
        metadata: json!({
            "dispatch_defaults": {
                "cwd": "/path/that/does/not/exist",
                "repo": "stale-repo"
            }
        }),
        entities: Vec::new(),
        agents: Vec::new(),
        tools: Vec::new(),
        abilities: Vec::new(),
        workflows: Vec::new(),
        artifacts: Vec::new(),
        artifact_graph: Vec::new(),
        dependencies: Vec::new(),
        gates: Vec::new(),
        metrics: Vec::new(),
        gate_bundles: Vec::new(),
        policy: None,
        phases: Vec::new(),
        actions: Vec::new(),
        initial_event: None,
    };

    homeboy::agents::agent_tasks::controller_service::apply_spec_dispatch_defaults_with_cwd(
        &mut spec,
        "-",
        || Some(repo.path().to_path_buf()),
    );
    let expected_root = std::fs::canonicalize(repo.path()).expect("canonical repo path");

    assert_eq!(
        spec.metadata["dispatch_defaults"]["cwd"],
        expected_root.display().to_string()
    );
    assert_eq!(spec.metadata["dispatch_defaults"]["repo"], "stale-repo");
}

#[test]
fn from_spec_dispatch_defaults_replace_stale_cwd_in_snapshot_workspace() {
    let workspace = tempfile::tempdir().expect("workspace dir");
    let mut spec = AgentTaskRepoLoopSpec {
        schema: None,
        loop_id: "repo-loop-snapshot-cwd-defaults".to_string(),
        phase: "init".to_string(),
        config_version: "v1".to_string(),
        metadata: json!({
            "dispatch_defaults": {
                "cwd": "/path/that/does/not/exist",
                "repo": "wp-site-generator"
            }
        }),
        entities: Vec::new(),
        agents: Vec::new(),
        tools: Vec::new(),
        abilities: Vec::new(),
        workflows: Vec::new(),
        artifacts: Vec::new(),
        artifact_graph: Vec::new(),
        dependencies: Vec::new(),
        gates: Vec::new(),
        metrics: Vec::new(),
        gate_bundles: Vec::new(),
        policy: None,
        phases: Vec::new(),
        actions: Vec::new(),
        initial_event: None,
    };

    homeboy::agents::agent_tasks::controller_service::apply_spec_dispatch_defaults_with_cwd(
        &mut spec,
        "-",
        || Some(workspace.path().to_path_buf()),
    );
    let expected_root = std::fs::canonicalize(workspace.path()).expect("canonical workspace path");

    assert_eq!(
        spec.metadata["dispatch_defaults"]["cwd"],
        expected_root.display().to_string()
    );
    assert_eq!(
        spec.metadata["dispatch_defaults"]["repo"],
        "wp-site-generator"
    );
}

#[test]
fn controller_dispatch_args_preserve_top_level_workspace_context_in_plan() {
    let repo = tempfile::tempdir().expect("repo dir");
    let repo_path = repo.path().display().to_string();
    let request = json!({
        "mode": "dispatch",
        "cwd": repo_path.clone(),
        "repo": "wp-site-generator@canonical-loop-main-20260616",
        "dispatch": {
            "prompt": "cook the next workflow",
            "backend": "sample-runtime"
        }
    });

    let command =
        homeboy::agents::agent_tasks::controller_service::controller_request_dispatch_command(
            &request,
            &homeboy::agents::agent_tasks::controller_service::ControllerDispatchOverrides::default(
            ),
        )
        .expect("dispatch command");
    let dispatch_request =
        homeboy::agents::agent_tasks::dispatch_service::resolve_dispatch_request(command)
            .expect("dispatch request");
    let plan = homeboy::agents::agent_tasks::dispatch_service::build_dispatch_plan_with_provider_requirements(
            &dispatch_request,
            |_backend, _selector| false,
        )
        .expect("dispatch plan");
    let task = plan.tasks.first().expect("plan task");

    assert_eq!(task.workspace.root.as_deref(), Some(repo_path.as_str()));
    assert_eq!(
        task.workspace.slug.as_deref(),
        Some("wp-site-generator@canonical-loop-main-20260616")
    );
    assert_eq!(
        task.executor.config["workspace_root"].as_str(),
        Some(repo_path.as_str())
    );
    assert_eq!(
        task.executor.config["repo"].as_str(),
        Some("wp-site-generator@canonical-loop-main-20260616")
    );
    assert_eq!(
        plan.metadata["workspace_root"].as_str(),
        Some(repo_path.as_str())
    );
}

#[test]
fn cook_dispatch_provider_id_alias_maps_to_selector() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "cook",
        "--to-worktree",
        "homeboy@cook-provider-id",
        "--verify",
        "true",
        "--backend",
        "sample-backend",
        "--dispatch-provider-id",
        "sample-provider",
        "--prompt",
        "cook",
    ])
    .expect("dispatch provider id alias parses");

    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("expected agent-task command");
    };
    let AgentTaskCommand::Cook(args) = agent_task.command else {
        panic!("expected cook command");
    };

    assert_eq!(args.dispatch.selector.as_deref(), Some("sample-provider"));
    assert_eq!(args.dispatch.model, None);
}

#[test]
fn adopt_attempt_selector_parses_as_an_explicit_cook_attempt() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "adopt",
        "cook-with-colliding-first-run-id",
        "--attempt",
        "1",
        "--candidate-ref",
        "deadbeef",
    ])
    .expect("attempt selector parses");

    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("expected agent-task command");
    };
    let AgentTaskCommand::Adopt(args) = agent_task.command else {
        panic!("expected adopt command");
    };
    assert_eq!(args.run_or_cook_id, "cook-with-colliding-first-run-id");
    assert_eq!(args.attempt, Some(1));
}

#[test]
fn adopt_inherited_failure_acceptance_is_explicit_and_documented() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "adopt",
        "cook-11978",
        "--candidate-ref",
        "deadbeef",
        "--accept-inherited-failures",
    ])
    .expect("explicit inherited-failure acceptance parses");
    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("expected agent-task command");
    };
    let AgentTaskCommand::Adopt(args) = agent_task.command else {
        panic!("expected adopt command");
    };
    assert!(args.accept_inherited_failures);

    let Err(error) = Cli::try_parse_from(["homeboy", "agent-task", "adopt", "--help"]) else {
        panic!("help exits after rendering");
    };
    let help = error.to_string();
    assert!(help.contains("--accept-inherited-failures"), "{help}");
    assert!(help.contains("immutable candidate base"), "{help}");
    assert!(
        help.contains("New or changed failures remain blocking"),
        "{help}"
    );
}

#[test]
fn cook_execution_budget_flags_parse_and_reject_legacy_attempts_mix() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "cook",
        "--to-worktree",
        "homeboy@execution-budget",
        "--verify",
        "true",
        "--backend",
        "sample-backend",
        "--prompt",
        "cook",
        "--max-provider-executions",
        "2",
        "--max-same-provider-retries",
        "1",
        "--max-provider-rotations",
        "0",
    ])
    .expect("execution budget flags parse");
    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("expected agent-task command");
    };
    let AgentTaskCommand::Cook(args) = agent_task.command else {
        panic!("expected cook command");
    };
    assert_eq!(args.dispatch.core.attempts, Some(2));
    assert_eq!(args.dispatch.core.same_provider_retries, Some(1));
    // An explicit zero is still explicit: it must remain distinguishable from
    // "not passed", which is what lets a configured rotation fund a default
    // budget without ever overriding an operator (#11082).
    assert_eq!(args.dispatch.core.provider_rotations, Some(0));

    assert!(Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "cook",
        "--to-worktree",
        "homeboy@execution-budget",
        "--verify",
        "true",
        "--backend",
        "sample-backend",
        "--prompt",
        "cook",
        "--attempts",
        "2",
        "--max-provider-executions",
        "2",
    ])
    .is_err());
}

#[test]
fn cook_rejects_zero_max_attempts_at_the_cli_boundary() {
    assert!(Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "cook",
        "--to-worktree",
        "homeboy@execution-budget",
        "--verify",
        "true",
        "--backend",
        "sample-backend",
        "--prompt",
        "cook",
        "--max-attempts",
        "0",
    ])
    .is_err());
}

#[test]
fn active_cursor_continues_discovery_and_cannot_scope_fleet_reconciliation() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "active",
        "--limit",
        "20",
        "--cursor",
        "20",
    ])
    .expect("active continuation parses");
    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("expected agent-task command");
    };
    let AgentTaskCommand::Active(args) = agent_task.command else {
        panic!("expected active command");
    };
    assert_eq!(args.limit, Some(20));
    assert_eq!(args.cursor, Some(20));
    assert!(!args.reconcile);

    assert!(Cli::try_parse_from(["homeboy", "agent-task", "active", "--limit", "0"]).is_err());

    assert!(Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "active",
        "--reconcile",
        "--limit",
        "20",
    ])
    .is_err());
    assert!(Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "active",
        "--reconcile",
        "--cursor",
        "20",
    ])
    .is_err());
    assert!(
        Cli::try_parse_from(["homeboy", "agent-task", "active", "--reconcile", "--full",]).is_err()
    );
}

#[test]
fn agent_task_timeout_ms_flags_parse_for_cook_run_and_run_plan() {
    let cook = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "cook",
        "--to-worktree",
        "homeboy@cook-timeout",
        "--verify",
        "true",
        "--backend",
        "sample-backend",
        "--prompt",
        "cook",
        "--timeout-ms",
        "1234",
    ])
    .expect("cook timeout parses");
    let Commands::AgentTask(agent_task) = cook.command else {
        panic!("expected agent-task command");
    };
    let AgentTaskCommand::Cook(args) = agent_task.command else {
        panic!("expected cook command");
    };
    assert_eq!(args.dispatch.core.timeout_ms, Some(1234));

    let run = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "run",
        "run-123",
        "--timeout-ms",
        "5678",
    ])
    .expect("run timeout parses");
    let Commands::AgentTask(agent_task) = run.command else {
        panic!("expected agent-task command");
    };
    let AgentTaskCommand::Run(args) = agent_task.command else {
        panic!("expected run command");
    };
    assert_eq!(args.timeout_ms, Some(5678));

    let run_plan = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "run-plan",
        "--plan",
        "@plan.json",
        "--timeout-ms",
        "9012",
    ])
    .expect("run-plan timeout parses");
    let Commands::AgentTask(agent_task) = run_plan.command else {
        panic!("expected agent-task command");
    };
    let AgentTaskCommand::RunPlan(args) = agent_task.command else {
        panic!("expected run-plan command");
    };
    assert_eq!(args.timeout_ms, Some(9012));
}

#[test]
fn controller_dispatch_provider_id_alias_maps_to_dispatch_selector() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "controller",
        "from-spec",
        "@controller.json",
        "--dispatch-provider-id",
        "sample-provider",
    ])
    .expect("controller dispatch provider id alias parses");

    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("expected agent-task command");
    };
    let AgentTaskCommand::Controller(controller) = agent_task.command else {
        panic!("expected controller command");
    };
    let super::super::AgentTaskControllerCommand::FromSpec(args) = controller.command else {
        panic!("expected controller from-spec command");
    };

    assert_eq!(
        args.dispatch.dispatch_selector.as_deref(),
        Some("sample-provider")
    );
    assert_eq!(args.dispatch.dispatch_model, None);
}

#[test]
fn controller_events_command_applies_generic_event() {
    with_isolated_home(|_| {
        agent_task_controller_service::init(
            homeboy::agents::agent_tasks::controller_service::ControllerInitRequest {
                loop_id: "controller-events-cli".to_string(),
                phase: "init".to_string(),
                config_version: "v1".to_string(),
            },
        )
        .expect("controller initialized");

        let (value, status) = apply_controller_event(AgentTaskControllerApplyEventArgs {
            loop_id: "controller-events-cli".to_string(),
            event_type: "task.completed".to_string(),
            event_id: Some("event-1".to_string()),
            event_key: Some("task#1".to_string()),
            entity_id: Some("entity-1".to_string()),
            payload: Some(r#"{"status":"ok"}"#.to_string()),
        })
        .expect("event applied");

        assert_eq!(status, 0);
        assert_eq!(
            value["schema"],
            homeboy::agents::agent_tasks::controller_service::APPLY_EVENT_RESULT_SCHEMA
        );
        assert_eq!(
            value["controller"]["history"][0]["event_type"],
            "task.completed"
        );
        assert_eq!(value["controller"]["history"][0]["entity_id"], "entity-1");
        assert_eq!(value["controller"]["history"][0]["payload"]["status"], "ok");
    });
}
