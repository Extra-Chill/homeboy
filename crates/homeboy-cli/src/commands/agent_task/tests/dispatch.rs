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

fn register_component_with_aliases(
    id: &str,
    aliases: &[&str],
    path: &std::path::Path,
    remote_url: &str,
) {
    homeboy::core::component::write_standalone_component_config(
        &homeboy::core::component::Component {
            id: id.to_string(),
            aliases: aliases.iter().map(|alias| (*alias).to_string()).collect(),
            local_path: path.display().to_string(),
            remote_url: Some(remote_url.to_string()),
            ..Default::default()
        },
    )
    .expect("register component");
}

#[test]
fn explicit_repo_identity_resolution_does_not_hydrate_unrelated_components() {
    with_isolated_home(|_| {
        let checkout = tempfile::tempdir().expect("checkout");
        init_runtime_component_checkout(checkout.path());
        add_remote(
            checkout.path(),
            "origin",
            "https://github.com/example/fixture.git",
        );
        register_component(
            "fixture",
            checkout.path(),
            "https://github.com/example/fixture.git",
        );

        let unrelated = tempfile::tempdir().expect("unrelated checkout");
        register_component(
            "unrelated",
            unrelated.path(),
            "https://github.com/example/fixture.git",
        );

        let identities = super::super::run::cook_repository_identities_for_workspace(
            "--cwd",
            checkout.path().to_str().expect("UTF-8 checkout path"),
            Some("fixture"),
        )
        .expect("resolve explicit repository identity");

        assert_eq!(identities.len(), 1);
    });
}

#[test]
fn explicit_unregistered_repo_is_proven_by_the_workspace_remote() {
    with_isolated_home(|_| {
        let checkout = tempfile::tempdir().expect("checkout");
        init_runtime_component_checkout(checkout.path());
        add_remote(
            checkout.path(),
            "origin",
            "https://github.com/example/fixture.git",
        );

        let resolved = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--repo".to_string(),
            "fixture".to_string(),
            "--cwd".to_string(),
            checkout.path().display().to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("resolve explicit repository identity from its remote");

        assert_eq!(resolved.dispatch.repo.as_deref(), Some("fixture"));
        assert_eq!(resolved.component, None);
        let identity = resolved.repository_identity.expect("repository identity");
        assert_eq!(
            identity["remote_identity"],
            "git://github.com/example/fixture"
        );
        assert_eq!(identity["component_registered"], false);

        let stale_component =
            super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--prompt".to_string(),
                "implement the fix".to_string(),
                "--repo".to_string(),
                "fixture".to_string(),
                "--component".to_string(),
                "removed-component".to_string(),
                "--cwd".to_string(),
                checkout.path().display().to_string(),
                "--no-finalize".to_string(),
            ]))
            .expect_err("an explicit stale component remains invalid");
        assert!(
            stale_component
                .details
                .to_string()
                .contains("does not match the supplied workspace repository"),
            "stale-component diagnostic: {}",
            stale_component.details
        );
    });
}

#[cfg(unix)]
#[test]
fn cwd_only_unregistered_repo_fails_without_hydrating_unrelated_components() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let checkout = tempfile::tempdir().expect("checkout");
        init_runtime_component_checkout(checkout.path());
        add_remote(
            checkout.path(),
            "origin",
            "https://github.com/example/unregistered.git",
        );

        let stale = tempfile::tempdir().expect("stale checkout");
        std::fs::write(stale.path().join("homeboy.json"), r#"{"id":"stale"}"#)
            .expect("write portable config");
        register_component(
            "stale",
            stale.path(),
            "https://github.com/example/stale.git",
        );

        let bin = tempfile::tempdir().expect("fake git bin");
        let git = bin.path().join("git");
        std::fs::write(
            &git,
            "#!/bin/sh\nif [ \"$PWD\" = \"$HOMEBOY_STALE_CHECKOUT\" ]; then sleep 30; exit 0; fi\nPATH=/usr/bin:/bin git \"$@\"\n",
        )
        .expect("write fake git");
        let mut permissions = std::fs::metadata(&git).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&git, permissions).expect("make fake git executable");
        let _stale_checkout = homeboy::core::test_support::EnvVarGuard::set(
            "HOMEBOY_STALE_CHECKOUT",
            std::fs::canonicalize(stale.path())
                .expect("canonical stale checkout")
                .display()
                .to_string(),
        );
        let _path = homeboy::core::test_support::EnvVarGuard::set(
            "PATH",
            format!(
                "{}:{}",
                bin.path().display(),
                std::env::var_os("PATH")
                    .as_deref()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
        );

        let started = std::time::Instant::now();
        let error = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--cwd".to_string(),
            checkout.path().display().to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect_err("unregistered cwd requires an explicit repository identity");

        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert!(
            error
                .details
                .to_string()
                .contains("--repo <repo> is required"),
            "missing-argument recovery: {}",
            error.details
        );
    });
}

fn write_component_without_collision_validation(component: homeboy::core::component::Component) {
    let directory = homeboy::core::paths::components().expect("component config directory");
    std::fs::create_dir_all(&directory).expect("create component config directory");
    std::fs::write(
        directory.join(format!("{}.json", component.id)),
        serde_json::to_vec(&component).expect("serialize component config"),
    )
    .expect("write component config");
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
        assert_eq!(
            derived
                .repository_identity
                .as_ref()
                .expect("repo expectation")["repository_name"],
            "homeboy"
        );

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

#[cfg(unix)]
#[test]
fn cook_reuses_a_task_candidate_without_overriding_explicit_head() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        assert!(Command::new("git")
            .args(["checkout", "-b", "fix/216-persist-task"])
            .current_dir(workspace.path())
            .status()
            .expect("create fixture branch")
            .success());
        let provider = tempfile::NamedTempFile::new()
            .expect("provider file")
            .into_temp_path();
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"project@task-216\",\"path\":\"{}\",\"branch\":\"fix/216-persist-task\",\"task_url\":\"HTTPS://EXAMPLE.TEST/owner/Project/issues/216/?provider=1#result\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n",
                workspace.path().display()
            ),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("provider metadata")
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
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy::core::defaults::WorktreeProviderCommands {
                    resolve_task: Some(vec![
                        provider.display().to_string(),
                        "{task_url}".to_string(),
                    ]),
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
                        task_url: Some("$.task_url".to_string()),
                    },
                ),
            },
        );
        homeboy::core::defaults::save_config(&config).expect("save provider config");

        let resolved = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "reuse task workspace".to_string(),
            "--repo".to_string(),
            "project".to_string(),
            "--task-url".to_string(),
            " HTTPS://example.test/owner/Project/issues/216/?source=cook#details ".to_string(),
            "--head".to_string(),
            "fix/216-persist-task".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("reuse the matching task workspace");
        assert_eq!(resolved.to_worktree.as_deref(), Some("project@task-216"));
        assert_eq!(resolved.head.as_deref(), Some("fix/216-persist-task"));
    });
}

#[cfg(unix)]
#[test]
fn self_repair_bootstrap_uses_explicit_checkout_and_preserves_normal_cook_contract() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let primary = tempfile::tempdir().expect("provider repository");
        init_runtime_component_checkout(primary.path());
        add_remote(
            primary.path(),
            "origin",
            "https://github.com/example/workspace-service.git",
        );
        register_component(
            "workspace-service-component",
            primary.path(),
            "https://github.com/example/workspace-service.git",
        );
        let root = tempfile::tempdir().expect("worktree root");
        let checkout = root.path().join("workspace-service-self-repair");
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "fix/provider-self-repair",
                checkout.to_str().expect("checkout path"),
                "HEAD",
            ])
            .current_dir(primary.path())
            .status()
            .expect("create self-repair checkout")
            .success());
        let invoked = root.path().join("provider-invoked");
        let provider = root.path().join("failed-provider");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nprintf 'invoked\\n' >> '{}'\nexit 9\n",
                invoked.display()
            ),
        )
        .expect("write failed provider");
        let mut permissions = std::fs::metadata(&provider).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");

        let mut config = homeboy::core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "workspace-service".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy::core::defaults::WorktreeProviderCommands {
                    resolve_task: Some(vec![provider.display().to_string()]),
                    ensure: Some(vec![provider.display().to_string()]),
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
                        task_url: Some("$.task_url".to_string()),
                    },
                ),
            },
        );
        config.settings.insert(
            homeboy::core::worktree_providers::WORKTREE_PROVIDER_SELF_REPAIR_SETTINGS_KEY
                .to_string(),
            json!({
                "workspace-service": {
                    "repository": "workspace-service-component"
                }
            }),
        );
        homeboy::core::defaults::save_config(&config).expect("save provider ownership");

        let nonfinalizing_failure =
            super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--prompt".to_string(),
                "repair without publication".to_string(),
                "--repo".to_string(),
                "workspace-service-component".to_string(),
                "--task-url".to_string(),
                "https://github.com/example/workspace-service/issues/13410".to_string(),
                "--backend".to_string(),
                "fixture".to_string(),
                "--no-finalize".to_string(),
            ]))
            .expect_err("non-finalizing provider failure is not a self-repair route");
        assert!(nonfinalizing_failure
            .details
            .get("worktree_provider_self_repair")
            .is_none());

        let failure = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "repair the workspace service".to_string(),
            "--repo".to_string(),
            "workspace-service-component".to_string(),
            "--task-url".to_string(),
            "https://github.com/example/workspace-service/issues/13410".to_string(),
            "--backend".to_string(),
            "fixture".to_string(),
            "--verify".to_string(),
            "cargo test --workspace".to_string(),
            "--private-verify".to_string(),
            "PRIVATE_GATE_SECRET=do-not-persist".to_string(),
        ]))
        .expect_err("normal provider-owned lookup fails before bootstrap");
        assert_eq!(
            failure.details["worktree_provider_self_repair"]["failed_operation"],
            "resolve_task"
        );
        let route = &failure.details["worktree_provider_self_repair"];
        assert_eq!(
            route["schema"],
            "homeboy/worktree-provider-self-repair-route/v1"
        );
        assert_eq!(route["provider_id"], "workspace-service");
        assert_eq!(
            route["provider_lifecycle_reconciliation"]["status"],
            "required_after_repair_ships"
        );
        let replay = route["replay_argv"].as_array().expect("typed replay argv");
        assert!(replay.iter().any(|value| value == "--cwd"));
        assert!(replay
            .iter()
            .any(|value| value == "<clean-existing-linked-worktree>"));
        assert!(replay
            .iter()
            .any(|value| value == "--worktree-provider-self-repair"));
        assert!(replay.iter().any(|value| value == "cargo test --workspace"));
        assert!(replay
            .iter()
            .any(|value| value == "<redacted:--private-verify>"));
        assert!(!route.to_string().contains("PRIVATE_GATE_SECRET"));
        assert!(route["replay_requires"]
            .as_array()
            .expect("replay requirements")
            .iter()
            .any(|requirement| requirement
                .as_str()
                .is_some_and(|requirement| requirement.contains("private gate"))));

        let args = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "repair the workspace service".to_string(),
            "--repo".to_string(),
            "workspace-service-component".to_string(),
            "--task-url".to_string(),
            "https://github.com/example/workspace-service/issues/13410".to_string(),
            "--backend".to_string(),
            "fixture".to_string(),
            "--cwd".to_string(),
            checkout.display().to_string(),
            "--worktree-provider-self-repair".to_string(),
            "workspace-service".to_string(),
            "--verify".to_string(),
            "cargo test --workspace".to_string(),
        ]))
        .expect("admit explicit provider self-repair route");
        let provision = super::super::run::provision_cook_destination(&args)
            .expect("provision from explicit checkout without provider");
        let plan = super::super::run::compile_cook_plan(&args, provision.clone())
            .expect("compile normal reviewed Cook plan");

        assert_eq!(
            std::fs::read_to_string(&invoked)
                .expect("provider invocation record")
                .lines()
                .count(),
            2,
            "bootstrap must not invoke the failed provider again"
        );
        assert!(!args.no_finalize, "self-repair retains normal finalization");
        assert_eq!(args.head.as_deref(), Some("fix/provider-self-repair"));
        assert_eq!(
            provision["self_repair_bootstrap"]["workspace_authority"],
            "explicit_clean_existing_checkout"
        );
        assert_eq!(
            provision["self_repair_bootstrap"]["review_and_finalization"],
            "normal"
        );
        assert_eq!(
            provision["self_repair_bootstrap"]["provider_lifecycle_reconciliation"]["status"],
            "pending"
        );
        assert_eq!(
            plan.metadata["cook_provision"]["self_repair_bootstrap"]["task_url"],
            "https://github.com/example/workspace-service/issues/13410"
        );
        assert_eq!(
            plan.tasks[0].workspace.root.as_deref(),
            std::fs::canonicalize(&checkout)
                .expect("canonical checkout")
                .to_str()
        );
        assert_eq!(args.gates.verify, vec!["cargo test --workspace"]);
    });
}

#[cfg(unix)]
#[test]
fn cook_explicit_repo_skips_unrelated_portable_git_enrichment() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let target = tempfile::tempdir().expect("target checkout");
        register_component(
            "target",
            target.path(),
            "https://github.com/example/target.git",
        );
        let stale = tempfile::tempdir().expect("stale checkout");
        std::fs::write(stale.path().join("homeboy.json"), r#"{"id":"stale"}"#)
            .expect("write portable config");
        register_component(
            "stale",
            stale.path(),
            "https://github.com/example/stale.git",
        );

        let bin = tempfile::tempdir().expect("fake git bin");
        let git = bin.path().join("git");
        std::fs::write(
            &git,
            "#!/bin/sh\nif [ \"$PWD\" = \"$HOMEBOY_STALE_CHECKOUT\" ]; then sleep 30; exit 0; fi\nPATH=/usr/bin:/bin git \"$@\"\n",
        )
        .expect("write fake git");
        let mut permissions = std::fs::metadata(&git).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&git, permissions).expect("make fake git executable");
        let _stale_checkout = homeboy::core::test_support::EnvVarGuard::set(
            "HOMEBOY_STALE_CHECKOUT",
            std::fs::canonicalize(stale.path())
                .expect("canonical stale checkout")
                .display()
                .to_string(),
        );
        let _path = homeboy::core::test_support::EnvVarGuard::set(
            "PATH",
            format!(
                "{}:{}",
                bin.path().display(),
                std::env::var_os("PATH")
                    .as_deref()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
        );

        let started = std::time::Instant::now();
        let args = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "targeted lookup".to_string(),
            "--repo".to_string(),
            "target".to_string(),
            "--to-worktree".to_string(),
            target.path().display().to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("explicit repo must not inspect unrelated registrations");

        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "unrelated portable checkout was probed"
        );
        assert_eq!(
            args.repository_identity.expect("identity")["remote_identity"],
            "git://github.com/example/target"
        );
    });
}

#[test]
fn cook_infers_repo_from_an_explicit_linked_worktree_cwd_and_persists_its_provenance() {
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
            "--cwd".to_string(),
            destination.display().to_string(),
            "--task-url".to_string(),
            "https://github.com/example/inferred/issues/13947".to_string(),
            "--backend".to_string(),
            "fixture".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("infer repository from linked worktree cwd");
        assert_eq!(args.dispatch.repo.as_deref(), Some("inferred"));
        assert_eq!(
            args.repository_identity.as_ref().expect("identity")["remote_identity"],
            "git://github.com/example/inferred"
        );
        assert_eq!(
            args.repository_identity.as_ref().expect("identity")["provenance"],
            "--cwd:git-remote:origin"
        );

        let plan = super::super::run::compile_cook_plan(&args, json!({ "path": destination }))
            .expect("compile Cook plan");
        assert_eq!(
            plan.metadata["cook_repository_identity"],
            args.repository_identity.expect("identity")
        );
        assert_eq!(
            plan.metadata["cook_base_resolution"]["source"],
            "compatibility_fallback"
        );
    });
}

#[test]
fn repo_only_cook_persists_configured_repository_identity_for_deferred_lookup() {
    with_isolated_home(|_| {
        let component_root = tempfile::tempdir().expect("component root");
        register_component(
            "expected",
            component_root.path(),
            "https://token:secret@github.com/example/expected.git",
        );

        let args = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--repo".to_string(),
            "expected".to_string(),
            "--to-worktree".to_string(),
            "fixture@expected".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("bind configured repository identity");

        let identity = args.repository_identity.expect("repository identity");
        assert_eq!(
            identity["remote_identity"],
            "git://github.com/example/expected"
        );
        assert_eq!(identity["provenance"], "--repo:configured-component");
        assert!(!identity.to_string().contains("secret"));
    });
}

#[test]
fn repo_only_cook_without_registered_component_persists_requested_repository_expectation() {
    with_isolated_home(|_| {
        let args = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--repo".to_string(),
            "unidentified".to_string(),
            "--to-worktree".to_string(),
            "fixture@unidentified".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("repo-only Cook remains compatible without component registration");

        let identity = args.repository_identity.expect("repository expectation");
        assert_eq!(identity["repository_name"], "unidentified");
        assert_eq!(identity["provenance"], "--repo:requested-repository");
    });
}

#[test]
fn cook_preserves_repository_and_component_identity_for_every_destination_form() {
    with_isolated_home(|_| {
        let checkout = tempfile::tempdir().expect("repository checkout");
        init_runtime_component_checkout(checkout.path());
        std::fs::create_dir(checkout.path().join("php-transformer"))
            .expect("nested component directory");
        std::fs::write(
            checkout.path().join("php-transformer/plugin.php"),
            "<?php\n",
        )
        .expect("nested component marker");
        assert!(Command::new("git")
            .args(["add", "php-transformer/plugin.php"])
            .current_dir(checkout.path())
            .status()
            .expect("stage nested component")
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "add nested component"])
            .current_dir(checkout.path())
            .status()
            .expect("commit nested component")
            .success());
        let remote = "https://github.com/example/blocks-engine.git";
        add_remote(checkout.path(), "origin", remote);
        register_component_with_aliases(
            "php-transformer",
            &["blocks-engine"],
            &checkout.path().join("php-transformer"),
            remote,
        );
        let destination_root = tempfile::tempdir().expect("destination root");
        let destination = destination_root.path().join("blocks-engine@fix-12844");
        assert!(Command::new("git")
            .args(["worktree", "add", "-b", "fix-12844"])
            .arg(&destination)
            .current_dir(checkout.path())
            .status()
            .expect("create linked destination")
            .success());

        let issue = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--repo".to_string(),
            "blocks-engine".to_string(),
            "--task-url".to_string(),
            "https://github.com/example/blocks-engine/issues/12844".to_string(),
            "--base".to_string(),
            "trunk".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("normalize issue repository alias");
        let cwd = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--repo".to_string(),
            "blocks-engine".to_string(),
            "--cwd".to_string(),
            checkout.path().display().to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("normalize cwd repository alias");
        let worktree = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--repo".to_string(),
            "blocks-engine".to_string(),
            "--to-worktree".to_string(),
            destination.display().to_string(),
            "--backend".to_string(),
            "fixture".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("normalize worktree repository alias");

        for args in [&issue, &cwd, &worktree] {
            assert_eq!(args.dispatch.repo.as_deref(), Some("blocks-engine"));
            assert_eq!(args.component.as_deref(), Some("php-transformer"));
            let identity = args
                .repository_identity
                .as_ref()
                .expect("identity evidence");
            assert_eq!(identity["repository_name"], "blocks-engine");
            assert_eq!(identity["component_id"], "php-transformer");
        }
        assert_eq!(
            issue.to_worktree.as_deref(),
            Some("blocks-engine@fix-issue-12844-blocks-engine")
        );
        assert_eq!(issue.base.as_deref(), Some("trunk"));
        let replay = super::super::run::cook_replay_argv(&issue);
        assert!(replay
            .windows(2)
            .any(|args| args == ["--repo", "blocks-engine"]));
        assert!(replay
            .windows(2)
            .any(|args| args == ["--component", "php-transformer"]));

        let plan = super::super::run::compile_cook_plan(
            &worktree,
            json!({ "action": "existing", "path": destination }),
        )
        .expect("compile normalized durable plan");
        assert_eq!(
            plan.metadata["cook_repository_identity"]["repository_name"],
            "blocks-engine"
        );
        assert_eq!(
            plan.metadata["cook_repository_identity"]["component_id"],
            "php-transformer"
        );
        assert_eq!(plan.group_key.as_deref(), Some("blocks-engine"));
        assert_eq!(plan.tasks[0].group_key.as_deref(), Some("blocks-engine"));
        assert_eq!(
            plan.tasks[0].workspace.slug.as_deref(),
            Some("blocks-engine")
        );
        assert_eq!(plan.metadata["component"], "php-transformer");
        assert_eq!(plan.tasks[0].metadata["component"], "php-transformer");
        assert_eq!(plan.tasks[0].executor.config["repo"], "blocks-engine");
        assert_eq!(
            plan.tasks[0].executor.config["component_id"],
            "php-transformer"
        );
        assert_eq!(
            plan.tasks[0].executor.config["component_cwd"],
            "php-transformer"
        );
        assert_eq!(
            plan.metadata["gate_workspace"]["component_cwd"],
            "php-transformer"
        );

        let mut config = homeboy::core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "dmc".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy::core::defaults::WorktreeProviderCommands {
                    ensure: Some(vec![
                        "dmc-worktree-provider".to_string(),
                        "{repo}".to_string(),
                    ]),
                    ..Default::default()
                },
                list_result_mapping: None,
            },
        );
        homeboy::core::defaults::save_config(&config).expect("save DMC provider config");
        let mut provider_args = issue.clone();
        provider_args.base = Some("main".to_string());
        let provision = super::super::run::provision_cook_destination(&provider_args)
            .expect("defer DMC provisioning until Cook admission");
        assert_eq!(provision["provision_intent"]["repo"], "blocks-engine");
        assert_eq!(
            provider_args.dispatch.repo.as_deref(),
            Some("blocks-engine")
        );

        let component_id = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--repo".to_string(),
            "php-transformer".to_string(),
            "--to-worktree".to_string(),
            "php-transformer@component-id".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("normalize configured component id");
        let identity = component_id
            .repository_identity
            .expect("component-id identity evidence");
        assert_eq!(identity["repository_name"], "blocks-engine");
        assert_eq!(identity["component_id"], "php-transformer");
    });
}

#[test]
fn cook_replay_preserves_exact_component_when_repository_has_multiple_components() {
    with_isolated_home(|_| {
        let root = tempfile::tempdir().expect("repository root");
        homeboy::core::test_support::run_git_fixture_command(root.path(), &["init", "-q"]);
        let first = root.path().join("component-a");
        let second = root.path().join("component-b");
        std::fs::create_dir_all(&first).expect("first component");
        std::fs::create_dir_all(&second).expect("second component");
        let remote = "https://github.com/example/shared-repository.git";
        register_component("component-a", &first, remote);
        register_component("component-b", &second, remote);

        for component in ["component-a", "component-b"] {
            let resolved = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--prompt".to_string(),
                "implement the fix".to_string(),
                "--repo".to_string(),
                component.to_string(),
                "--task-url".to_string(),
                "https://github.com/example/shared-repository/issues/12844".to_string(),
                "--base".to_string(),
                "main".to_string(),
                "--no-finalize".to_string(),
            ]))
            .expect("resolve exact component");
            assert_eq!(resolved.dispatch.repo.as_deref(), Some("shared-repository"));
            assert_eq!(resolved.component.as_deref(), Some(component));

            let replay = super::super::run::cook_replay_argv(&resolved);
            assert!(replay
                .windows(2)
                .any(|args| args == ["--repo", "shared-repository"]));
            assert!(replay
                .windows(2)
                .any(|args| args == ["--component", component]));
            let replayed = super::super::run::resolve_cook_destination(cook_args_from_cli(replay))
                .expect("replay exact component");
            assert_eq!(replayed.dispatch.repo, resolved.dispatch.repo);
            assert_eq!(replayed.component, resolved.component);
        }

        let error = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--repo".to_string(),
            "shared-repository".to_string(),
            "--task-url".to_string(),
            "https://github.com/example/shared-repository/issues/12844".to_string(),
            "--base".to_string(),
            "main".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect_err("canonical repository remains ambiguous without a component");
        assert!(error
            .message
            .contains("multiple configured component identities"));
    });
}

#[test]
fn cook_resolves_omitted_base_from_workspace_upstream_for_standard_and_custom_branches() {
    with_isolated_home(|_| {
        for branch in ["main", "master", "trunk", "release/2026"] {
            let source = tempfile::tempdir().expect("source checkout");
            let remote = tempfile::tempdir().expect("bare remote");
            for args in [
                vec!["init", "-b", branch],
                vec!["config", "user.email", "test@example.com"],
                vec!["config", "user.name", "Test"],
            ] {
                assert!(Command::new("git")
                    .args(args)
                    .current_dir(source.path())
                    .status()
                    .expect("configure source")
                    .success());
            }
            std::fs::write(source.path().join("README"), branch).expect("write source");
            assert!(Command::new("git")
                .args(["add", "README"])
                .current_dir(source.path())
                .status()
                .expect("stage source")
                .success());
            assert!(Command::new("git")
                .args(["commit", "-m", "initial"])
                .current_dir(source.path())
                .status()
                .expect("commit source")
                .success());
            assert!(Command::new("git")
                .args([
                    "init",
                    "--bare",
                    "--initial-branch",
                    branch,
                    remote.path().to_str().unwrap()
                ])
                .status()
                .expect("create remote")
                .success());
            add_remote(source.path(), "origin", remote.path().to_str().unwrap());
            assert!(Command::new("git")
                .args(["push", "-u", "origin", branch])
                .current_dir(source.path())
                .status()
                .expect("push upstream")
                .success());
            let component_remote = format!("https://github.com/example/{branch}.git");
            assert!(Command::new("git")
                .args(["remote", "set-url", "origin", &component_remote])
                .current_dir(source.path())
                .status()
                .expect("normalize fixture remote")
                .success());
            register_component("fixture", source.path(), &component_remote);

            let args = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--prompt".to_string(),
                "resolve base".to_string(),
                "--workspace".to_string(),
                source.path().display().to_string(),
                "--to-worktree".to_string(),
                source.path().display().to_string(),
                "--repo".to_string(),
                "fixture".to_string(),
                "--backend".to_string(),
                "fixture".to_string(),
                "--no-finalize".to_string(),
            ]))
            .expect("resolve workspace upstream");
            assert_eq!(args.base.as_deref(), Some(branch));
            assert_eq!(
                args.base_resolution.as_ref().expect("base evidence")["source"],
                "workspace_upstream"
            );
        }
    });
}

#[test]
fn cook_rejects_ambiguous_repository_identity_across_destination_forms() {
    for (kind, first, second) in [
        (
            "duplicate alias",
            (
                "first",
                vec!["shared"],
                "https://github.com/example/first.git",
            ),
            (
                "second",
                vec!["shared"],
                "https://github.com/example/second.git",
            ),
        ),
        (
            "normalized alias",
            (
                "first",
                vec!["shared.git"],
                "https://github.com/example/first.git",
            ),
            (
                "second",
                vec!["shared"],
                "https://github.com/example/second.git",
            ),
        ),
        (
            "duplicate remote",
            ("first", Vec::new(), "https://github.com/example/shared.git"),
            (
                "second",
                Vec::new(),
                "ssh://git@github.com/example/shared.git",
            ),
        ),
    ] {
        with_isolated_home(|_| {
            let checkout = tempfile::tempdir().expect("repository checkout");
            init_runtime_component_checkout(checkout.path());
            add_remote(checkout.path(), "origin", first.2);
            for (id, aliases, remote) in [first, second] {
                write_component_without_collision_validation(homeboy::core::component::Component {
                    id: id.to_string(),
                    aliases: aliases.into_iter().map(str::to_string).collect(),
                    local_path: checkout.path().display().to_string(),
                    remote_url: Some(remote.to_string()),
                    ..Default::default()
                });
            }

            for form in ["issue", "cwd", "to-worktree"] {
                let mut command = vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                    "--prompt".to_string(),
                    "implement the fix".to_string(),
                    "--repo".to_string(),
                    "shared".to_string(),
                ];
                match form {
                    "issue" => command.extend([
                        "--task-url".to_string(),
                        "https://github.com/example/shared/issues/12844".to_string(),
                    ]),
                    "cwd" => {
                        command.extend(["--cwd".to_string(), checkout.path().display().to_string()])
                    }
                    "to-worktree" => command.extend([
                        "--to-worktree".to_string(),
                        "first@identity-collision".to_string(),
                    ]),
                    _ => unreachable!(),
                }
                command.push("--no-finalize".to_string());
                let error =
                    super::super::run::resolve_cook_destination(cook_args_from_cli(command))
                        .expect_err(
                            "ambiguous configured identity must reject before provisioning",
                        );
                assert!(error
                    .message
                    .contains("matches multiple configured component identities"));
                assert!(error.message.contains("first"));
                assert!(error.message.contains("second"));
                assert_eq!(error.details["field"], "repo", "{kind} via {form}");
                assert_eq!(
                    error.details["tried"],
                    json!([
                        "homeboy agent-task cook --repo first ...",
                        "homeboy agent-task cook --repo second ..."
                    ]),
                    "{kind} via {form}"
                );
            }
        });
    }
}

#[test]
fn cook_exact_component_slug_disambiguates_shared_monorepo_remote() {
    with_isolated_home(|_| {
        let checkout = tempfile::tempdir().expect("repository checkout");
        init_runtime_component_checkout(checkout.path());
        let remote = "https://github.com/example/wp-build.git";
        add_remote(checkout.path(), "origin", remote);
        for id in [
            "site-forge",
            "site-generation-agents",
            "wp-build",
            "wp-build-repo",
            "wp-build-theme",
        ] {
            write_component_without_collision_validation(homeboy::core::component::Component {
                id: id.to_string(),
                local_path: checkout.path().display().to_string(),
                remote_url: Some(remote.to_string()),
                ..Default::default()
            });
        }

        let args = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "implement the fix".to_string(),
            "--repo".to_string(),
            "wp-build".to_string(),
            "--cwd".to_string(),
            checkout.path().display().to_string(),
            "--to-worktree".to_string(),
            checkout.path().display().to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("exact component slug must disambiguate its shared monorepo remote");

        assert_eq!(args.dispatch.repo.as_deref(), Some("wp-build"));
        assert_eq!(
            args.repository_identity.expect("repository identity")["component_id"],
            "wp-build"
        );
    });
}

#[test]
fn cook_resolves_omitted_base_from_repository_metadata_or_compatibility_fallback() {
    with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source checkout");
        init_runtime_component_checkout(source.path());
        let remote = tempfile::tempdir().expect("bare remote");
        assert!(Command::new("git")
            .args([
                "init",
                "--bare",
                "--initial-branch",
                "trunk",
                remote.path().to_str().unwrap()
            ])
            .status()
            .expect("create remote")
            .success());
        add_remote(source.path(), "origin", remote.path().to_str().unwrap());
        assert!(Command::new("git")
            .args(["push", "origin", "HEAD:trunk"])
            .current_dir(source.path())
            .status()
            .expect("push trunk")
            .success());
        assert!(Command::new("git")
            .args(["remote", "set-head", "origin", "trunk"])
            .current_dir(source.path())
            .status()
            .expect("set remote head")
            .success());
        assert!(Command::new("git")
            .args(["checkout", "-b", "feature", "origin/trunk"])
            .current_dir(source.path())
            .status()
            .expect("leave the old default branch")
            .success());
        assert!(Command::new("git")
            .args(["branch", "-D", "main"])
            .current_dir(source.path())
            .status()
            .expect("remove unavailable main")
            .success());
        let component_remote = "https://github.com/example/fixture.git";
        assert!(Command::new("git")
            .args(["remote", "set-url", "origin", component_remote])
            .current_dir(source.path())
            .status()
            .expect("normalize fixture remote")
            .success());
        register_component("fixture", source.path(), component_remote);

        let metadata = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "resolve metadata".to_string(),
            "--repo".to_string(),
            "fixture".to_string(),
            "--to-worktree".to_string(),
            "fixture@missing".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("resolve repository metadata");
        assert_eq!(metadata.base.as_deref(), Some("trunk"));
        assert_eq!(
            metadata.base_resolution.as_ref().expect("base evidence")["source"],
            "repository_metadata"
        );

        let mut mismatch = metadata;
        mismatch.base = Some("main".to_string());
        mismatch.base_resolution = Some(json!({ "base": "main", "source": "explicit" }));
        let error = super::super::run::provision_cook_destination(&mismatch)
            .expect_err("invalid base must fail before provisioning");
        assert_eq!(error.details["field"], "base");
        assert!(error.details["tried"].as_array().is_some_and(|replays| {
            replays.iter().any(|replay| {
                replay
                    .as_str()
                    .is_some_and(|replay| replay.contains("--base trunk"))
            })
        }));
        assert_eq!(
            error.details["correction_argv"],
            json!([
                "homeboy",
                "agent-task",
                "cook",
                "--prompt",
                "resolve metadata",
                "--repo",
                "fixture",
                "--component",
                "fixture",
                "--to-worktree",
                "fixture@missing",
                "--base",
                "trunk",
                "--no-finalize",
            ])
        );

        let fallback = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "fallback".to_string(),
            "--repo".to_string(),
            "unavailable".to_string(),
            "--to-worktree".to_string(),
            "unavailable@missing".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("fall back without metadata");
        assert_eq!(fallback.base.as_deref(), Some("main"));
        assert_eq!(
            fallback.base_resolution.expect("base evidence")["source"],
            "compatibility_fallback"
        );
    });
}

#[test]
fn corrected_cook_base_replay_keeps_adversarial_values_as_typed_argv() {
    let worktree = "fixture@work tree;$(touch pwned) 'quoted'";
    let branch = "trunk;$(touch pwned) 'quoted'";
    let args = cook_args_from_cli(vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--prompt".to_string(),
        "resolve safely".to_string(),
        "--repo".to_string(),
        "fixture".to_string(),
        "--to-worktree".to_string(),
        worktree.to_string(),
        "--base".to_string(),
        "main".to_string(),
        "--no-finalize".to_string(),
    ]);

    let argv = super::super::run::corrected_cook_base_replay_argv(&args, branch);
    assert_eq!(
        argv,
        vec![
            "homeboy",
            "agent-task",
            "cook",
            "--prompt",
            "resolve safely",
            "--repo",
            "fixture",
            "--to-worktree",
            worktree,
            "--base",
            branch,
            "--no-finalize",
        ]
    );
    assert_eq!(
        homeboy::core::engine::shell::quote_args(&argv),
        "homeboy agent-task cook --prompt 'resolve safely' --repo fixture --to-worktree 'fixture@work tree;$(touch pwned) '\\''quoted'\\''' --base 'trunk;$(touch pwned) '\\''quoted'\\''' --no-finalize"
    );
}

#[test]
fn cook_rejects_ambiguous_or_mismatching_workspace_repository_identity() {
    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        let target_root = tempfile::tempdir().expect("target root");
        let target = target_root.path().join("task");
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "fix/goal-task",
                target.to_str().expect("target path"),
                "HEAD"
            ])
            .current_dir(workspace.path())
            .status()
            .expect("create linked worktree")
            .success());
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
            "--repo <repo> is required because the supplied workspace is not a Git checkout; provide --repo <repository-or-configured-component>"
        );
    });
}

#[test]
fn cook_requires_repo_for_an_explicit_git_workspace_without_a_configured_mapping() {
    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        add_remote(
            workspace.path(),
            "origin",
            "https://github.com/example/unconfigured.git",
        );

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
        .expect_err("unmapped Git workspace cannot infer a repository");

        assert_eq!(
            error.details["args"][0],
            "--repo <repo> is required because the supplied Git checkout has no configured repository remote mapping; provide --repo <repository-or-configured-component>"
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
        assert!(!error.message.contains("git://github.com/example/two"));
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
            Arc::new(ExtensionProviderAgentTaskExecutor::default()),
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
fn cook_goal_frames_explicit_prompt_without_creating_another_provider_cell() {
    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        let target_root = tempfile::tempdir().expect("target root");
        let target = target_root.path().join("task");
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "fix/goal-task",
                target.to_str().expect("target path"),
                "HEAD"
            ])
            .current_dir(workspace.path())
            .status()
            .expect("create linked worktree")
            .success());
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--goal",
            "Preserve the durable provider-cell contract",
            "--prompt",
            "Implement the targeted repair",
            "--repo",
            "fixture",
            "--cwd",
            target.to_str().expect("target path"),
            "--to-worktree",
            target.to_str().expect("target path"),
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

        run_cook_with_executor_and_dispatcher(
            *cook,
            Arc::new(CapturingExecutor::default()),
            Some(Arc::new(RecipeOnlyDispatcher)),
        )
        .expect("persist Cook recipe before provider dispatch");

        let recipe = homeboy::agents::agent_task_service::load_recipe("cook-goal-task-one-cell")
            .expect("durable cook recipe");
        let plan = &recipe.attempts[0].plan;
        assert_eq!(plan.tasks.len(), 1);
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
        let target_root = tempfile::tempdir().expect("target root");
        let target = target_root.path().join("task");
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "fix/goal-only",
                target.to_str().expect("target path"),
                "HEAD"
            ])
            .current_dir(workspace.path())
            .status()
            .expect("create linked worktree")
            .success());
        let cli = Cli::parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--goal",
            "Implement the targeted repair",
            "--repo",
            "fixture",
            "--cwd",
            target.to_str().expect("target path"),
            "--to-worktree",
            target.to_str().expect("target path"),
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

        run_cook_with_executor_and_dispatcher(
            *cook,
            Arc::new(CapturingExecutor::default()),
            Some(Arc::new(RecipeOnlyDispatcher)),
        )
        .expect("persist Cook recipe before provider dispatch");

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
                mutation_timeout_ms: 30_000,
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
            run_cook_with_executor(
                *cook,
                Arc::new(ExtensionProviderAgentTaskExecutor::default()),
            )
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
fn cook_defers_existing_provider_destination_lookup_until_durable_admission() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        add_remote(
            workspace.path(),
            "origin",
            "https://github.com/example/existing.git",
        );
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
                mutation_timeout_ms: 30_000,
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

        let args = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "reuse destination".to_string(),
            "--repo".to_string(),
            "existing".to_string(),
            "--to-worktree".to_string(),
            "fixture@existing".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("repo-only Cook retains its requested repository expectation");
        let provision = super::super::run::provision_cook_destination(&args)
            .expect("provider destination lookup is deferred until durable Cook admission");

        assert_eq!(provision["action"], "lookup_pending");
        assert_eq!(provision["handle"], "fixture@existing");
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
        // `into_temp_path` closes the write handle. A `NamedTempFile` keeps one
        // open for its whole lifetime, and Linux refuses to `execve` a file that
        // any process still holds open for writing -- ETXTBSY, surfaced as
        // "Text file busy (os error 26)". The file stays alive and still
        // self-deletes; only the descriptor goes away.
        let provider = tempfile::NamedTempFile::new()
            .expect("provider file")
            .into_temp_path();
        std::fs::write(&provider, "#!/bin/sh\nsleep 2\n").expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");
        let mut config = homeboy::core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "timeout".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 1,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy::core::defaults::WorktreeProviderCommands {
                    list: Some(vec![provider.display().to_string()]),
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
                        task_url: Some("$.task_url".to_string()),
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
            "reuse supplied worktree".to_string(),
            "--cwd".to_string(),
            cwd.display().to_string(),
            "--repo".to_string(),
            "fixture".to_string(),
            "--task-url".to_string(),
            "https://github.com/example/fixture/issues/12088".to_string(),
            "--no-finalize".to_string(),
        ]);
        let started = std::time::Instant::now();
        let args = super::super::run::resolve_cook_destination(args)
            .expect("explicit cwd must bypass issue handle derivation");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "provider lookup ran while deriving an explicit cwd destination"
        );
        assert_eq!(
            args.to_worktree.as_deref(),
            Some(
                std::fs::canonicalize(&cwd)
                    .expect("canonical cwd")
                    .to_str()
                    .expect("UTF-8 fixture path")
            )
        );
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

#[cfg(unix)]
#[test]
fn cook_cwd_matching_external_handle_never_invokes_a_sleeping_resolver() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let primary = tempfile::tempdir().expect("primary checkout");
        init_runtime_component_checkout(primary.path());
        let worktree_root = tempfile::tempdir().expect("worktree root");
        let cwd = worktree_root
            .path()
            .join("homeboy@fix-12088-cwd-authority-final");
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "fix/cwd-provider-authority",
                cwd.to_str().expect("worktree path"),
                "HEAD",
            ])
            .current_dir(primary.path())
            .status()
            .expect("create linked worktree")
            .success());
        let provider_dir = tempfile::tempdir().expect("provider directory");
        let provider = provider_dir.path().join("sleeping-resolver");
        let invoked = provider_dir.path().join("resolver-invoked");
        std::fs::write(
            &provider,
            format!("#!/bin/sh\ntouch '{}'\nsleep 10\n", invoked.display()),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");
        let mut config = homeboy::core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "dmc".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
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
            "reuse DMC worktree".to_string(),
            "--cwd".to_string(),
            cwd.display().to_string(),
            "--to-worktree".to_string(),
            "homeboy@fix-12088-cwd-authority-final".to_string(),
            "--no-finalize".to_string(),
        ]);
        let started = std::time::Instant::now();
        let provision = super::super::run::provision_cook_destination(&args)
            .expect("matching provider handle must retain the authoritative CWD");

        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "a sleeping resolver blocked authoritative CWD validation"
        );
        assert!(
            !invoked.exists(),
            "authoritative CWD validation invoked the external resolver"
        );
        assert_eq!(provision["kind"], "explicit_cwd");
        assert_eq!(
            provision["path"],
            std::fs::canonicalize(&cwd).unwrap().display().to_string()
        );
        assert_eq!(
            provision["logical_provider_provenance"]["handle"],
            "homeboy@fix-12088-cwd-authority-final"
        );
        assert!(provision.get("workspace_identity").is_none());
    });
}

#[cfg(unix)]
#[test]
fn cook_cwd_adopts_clean_unpushed_provider_candidate_before_provisioning() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let primary = tempfile::tempdir().expect("primary checkout");
        init_runtime_component_checkout(primary.path());
        let worktree_root = tempfile::tempdir().expect("worktree root");
        let cwd = worktree_root.path().join("provider-owned-checkout");
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "fix/13421",
                cwd.to_str().expect("worktree path"),
                "HEAD",
            ])
            .current_dir(primary.path())
            .status()
            .expect("create linked worktree")
            .success());
        let provider_dir = tempfile::tempdir().expect("provider directory");
        let provider = provider_dir.path().join("provider");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\n",
                serde_json::json!({ "worktrees": [{
                    "handle": "homeboy@fix-13421",
                    "path": cwd,
                    "branch": "fix/13421",
                    "safety": { "dirty": false, "unpushed": true, "primary": false }
                }] })
            ),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).unwrap();
        let mut config = homeboy::core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "dmc".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy::core::defaults::WorktreeProviderCommands {
                    resolve_path: Some(vec![provider.display().to_string(), "{path}".to_string()]),
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
            "retain the candidate".to_string(),
            "--cwd".to_string(),
            cwd.display().to_string(),
            "--repo".to_string(),
            "homeboy".to_string(),
            "--no-finalize".to_string(),
        ]);

        let args = super::super::run::resolve_cook_destination(args)
            .expect("map provider path to canonical handle");
        assert_eq!(args.to_worktree.as_deref(), Some("homeboy@fix-13421"));
        let provision = super::super::run::provision_cook_destination(&args)
            .expect("provision the same canonical identity");
        assert_eq!(provision["handle"], "homeboy@fix-13421");
        assert_eq!(
            provision["workspace_identity"]["handle"],
            "homeboy@fix-13421"
        );
        assert_eq!(provision["workspace_safety"]["fresh"], true);
        assert_eq!(provision["workspace_safety"]["unpushed"], true);
    });
}

#[cfg(unix)]
#[test]
fn cook_cwd_rejects_a_mismatched_external_provider_handle_before_dispatch() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let primary = tempfile::tempdir().expect("primary checkout");
        init_runtime_component_checkout(primary.path());
        let worktree_root = tempfile::tempdir().expect("worktree root");
        let cwd = worktree_root.path().join("cwd");
        let foreign = worktree_root.path().join("foreign");
        for (path, branch) in [(&cwd, "fix/cwd"), (&foreign, "fix/foreign")] {
            assert!(Command::new("git")
                .args([
                    "worktree",
                    "add",
                    "-b",
                    branch,
                    path.to_str().unwrap(),
                    "HEAD"
                ])
                .current_dir(primary.path())
                .status()
                .expect("create linked worktree")
                .success());
        }
        let provider_dir = tempfile::tempdir().expect("provider directory");
        let provider = provider_dir.path().join("dmc-provider");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"homeboy@foreign\",\"path\":\"{}\",\"branch\":\"fix/foreign\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n",
                foreign.display()
            ),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).expect("make provider executable");
        let mut config = homeboy::core::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "dmc".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
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
            "reject foreign provider worktree".to_string(),
            "--cwd".to_string(),
            cwd.display().to_string(),
            "--to-worktree".to_string(),
            "homeboy@foreign".to_string(),
            "--no-finalize".to_string(),
        ]);

        let error = super::super::run::provision_cook_destination(&args)
            .expect_err("foreign provider handle must fail before Cook dispatch");
        assert_eq!(error.details["field"], "to_worktree");
        assert!(error
            .message
            .contains("must name the same linked task worktree"));
    });
}

#[test]
fn cook_cwd_external_handle_relationship_is_exact_and_case_preserving() {
    with_isolated_home(|_| {
        let root = tempfile::tempdir().expect("worktree root");
        let cwd = root.path().join("DMC@fix-12088-cwd-authority-final");
        std::fs::create_dir(&cwd).expect("create authoritative checkout");
        let canonical_cwd = std::fs::canonicalize(&cwd).expect("canonical authoritative checkout");
        let basename = canonical_cwd
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 authoritative basename");

        super::super::run::validate_logical_worktree_handle_path_relationship(
            &canonical_cwd,
            basename,
        )
        .expect("the complete canonical basename is accepted");

        for collision in [
            "DMC@fix/12088-cwd-authority-final",
            "DMC@fix_12088_cwd_authority_final",
            "DMC@fix-12088_cwd-authority-final",
            "dmc@fix-12088-cwd-authority-final",
            "other@fix-12088-cwd-authority-final",
        ] {
            let error = super::super::run::validate_logical_worktree_handle_path_relationship(
                &canonical_cwd,
                collision,
            )
            .expect_err("lossy punctuation, case, and branch-only collisions must reject");
            assert_eq!(error.details["field"], "to_worktree");
        }
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
        let executor = Arc::new(CountingExecutor::default());
        let error = run_cook_with_executor(args, executor.clone())
            .expect_err("inactive managed destination must fail before execution");

        assert_eq!(error.details["field"], "to_worktree");
        assert!(error.message.contains("is no longer active"));
        assert_eq!(executor.0.load(std::sync::atomic::Ordering::SeqCst), 0);
    });
}

#[cfg(unix)]
#[test]
fn cook_defers_foreign_provider_destination_validation_until_durable_admission() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        add_remote(
            workspace.path(),
            "origin",
            "https://token:provider-secret@github.com/example/foreign.git",
        );
        let destination_root = tempfile::tempdir().expect("destination root");
        let destination = destination_root.path().join("task");
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "fix/foreign",
                destination.to_str().expect("destination path"),
                "HEAD",
            ])
            .current_dir(workspace.path())
            .status()
            .expect("create linked worktree")
            .success());
        // `into_temp_path` closes the write handle. A `NamedTempFile` keeps one
        // open for its whole lifetime, and Linux refuses to `execve` a file that
        // any process still holds open for writing -- ETXTBSY, surfaced as
        // "Text file busy (os error 26)". The file stays alive and still
        // self-deletes; only the descriptor goes away.
        let provider = tempfile::NamedTempFile::new()
            .expect("provider file")
            .into_temp_path();
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"fixture@foreign\",\"path\":\"{}\",\"branch\":\"fix/foreign\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n",
                destination.display()
            ),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("provider metadata")
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
                mutation_timeout_ms: 30_000,
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
        let args = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "reject foreign destination".to_string(),
            "--repo".to_string(),
            "expected".to_string(),
            "--to-worktree".to_string(),
            "fixture@foreign".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("persist generic repo expectation");
        let provision = super::super::run::provision_cook_destination(&args)
            .expect("provider destination validation is deferred until Cook is durable");
        assert_eq!(provision["action"], "lookup_pending");
        assert_eq!(provision["handle"], "fixture@foreign");
    });
}

#[cfg(unix)]
#[test]
fn cook_defers_provider_lookup_failures_until_durable_admission() {
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
                mutation_timeout_ms: 30_000,
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
        let provision = super::super::run::provision_cook_destination(&args)
            .expect("provider lookup failure is deferred until Cook is durable");
        assert_eq!(provision["action"], "lookup_pending");
        assert!(!ensured.exists(), "failed lookup must not run ensure");
    });
}

#[cfg(unix)]
#[test]
fn cook_defers_an_explicit_missing_destination_until_durable_admission() {
    use std::os::unix::fs::PermissionsExt;

    with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("provider tempdir");
        let ensured = temp.path().join("ensured");
        let provider = temp.path().join("provider");
        std::fs::write(
            &provider,
            format!("#!/bin/sh\ntouch '{}'\n", ensured.display()),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("provider metadata")
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
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy::core::defaults::WorktreeProviderCommands {
                    ensure: Some(vec![provider.display().to_string()]),
                    ..Default::default()
                },
                list_result_mapping: None,
            },
        );
        homeboy::core::defaults::save_config(&config).expect("save provider config");

        let args = super::super::run::resolve_cook_destination(cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "create the issue worktree".to_string(),
            "--repo".to_string(),
            "homeboy".to_string(),
            "--task-url".to_string(),
            "https://github.com/Extra-Chill/homeboy/issues/12601".to_string(),
            "--to-worktree".to_string(),
            "homeboy@fix-issue-12601-homeboy".to_string(),
            "--head".to_string(),
            "fix/issue-12601-homeboy".to_string(),
            "--backend".to_string(),
            "fixture".to_string(),
            "--no-finalize".to_string(),
        ]))
        .expect("retain explicit missing destination without provisioning it");
        let provision = super::super::run::provision_cook_destination(&args)
            .expect("ensure-only provider is deferred until durable Cook admission");

        assert_eq!(provision["action"], "lookup_pending");
        assert_eq!(provision["handle"], "homeboy@fix-issue-12601-homeboy");
        assert_eq!(
            provision["provision_intent"],
            serde_json::json!({
                "repo": "homeboy",
                "base": "main",
                "head": "fix/issue-12601-homeboy",
                "task_url": "https://github.com/Extra-Chill/homeboy/issues/12601",
            })
        );
        assert_eq!(
            provision["lifecycle_intent"],
            serde_json::json!({
                "purpose": "agent_task_cook",
                "cleanup_policy": "remove_on_success",
            })
        );
        let plan = super::super::run::compile_cook_plan(&args, provision.clone())
            .expect("compile explicit Cook with its deferred provision intent");
        assert_eq!(plan.metadata["cook_provision"], provision);
        assert_eq!(
            plan.tasks[0].metadata["worktree_provision"],
            plan.metadata["cook_provision"]
        );
        assert!(
            !ensured.exists(),
            "provider ensure must wait for durable Cook admission"
        );
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
    std::fs::write(workspace.path().join(".git"), "gitdir: .snapshot-git\n")
        .expect("snapshot boundary blocks parent repository discovery");
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
fn list_latest_accepts_list_filters_and_rejects_pagination_selectors() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "list",
        "--latest",
        "--repo",
        "homeboy",
        "--worktree",
        "homeboy@fix-12242",
        "--task-url",
        "https://github.com/Extra-Chill/homeboy/issues/12242",
        "--submitted-after",
        "2026-01-01T00:00:00Z",
        "--state",
        "failed",
        "--run-placement",
        "runner",
        "--parent-id",
        "batch-12242",
    ])
    .expect("filtered latest list parses");
    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("expected agent-task command");
    };
    let AgentTaskCommand::List(args) = agent_task.command else {
        panic!("expected list command");
    };
    assert!(args.latest);
    assert_eq!(args.repo.as_deref(), Some("homeboy"));
    assert_eq!(args.worktree.as_deref(), Some("homeboy@fix-12242"));
    assert_eq!(
        args.task_url.as_deref(),
        Some("https://github.com/Extra-Chill/homeboy/issues/12242")
    );
    assert_eq!(
        args.submitted_after.as_deref(),
        Some("2026-01-01T00:00:00Z")
    );
    assert_eq!(args.state.as_deref(), Some("failed"));
    assert_eq!(args.run_placement.as_deref(), Some("runner"));
    assert_eq!(args.parent_id.as_deref(), Some("batch-12242"));

    assert!(
        Cli::try_parse_from(["homeboy", "agent-task", "list", "--latest", "--cursor", "1",])
            .is_err()
    );
    assert!(Cli::try_parse_from(["homeboy", "agent-task", "list", "--latest", "--full",]).is_err());
    assert!(
        Cli::try_parse_from(["homeboy", "agent-task", "list", "--latest", "--limit", "1",])
            .is_err()
    );
}

#[test]
fn list_latest_selects_the_newest_complete_filtered_match_or_an_empty_result() {
    with_isolated_home(|_| {
        let mut matching = test_plan();
        matching.group_key = Some("homeboy".to_string());
        matching.tasks[0].group_key = Some("homeboy".to_string());
        matching.tasks[0].workspace.root = Some("/work/homeboy".to_string());
        matching.tasks[0].workspace.task_url =
            Some("https://github.com/Extra-Chill/homeboy/issues/12242".to_string());
        matching.tasks[0].parent_plan_id = Some("batch-12242".to_string());
        agent_task_lifecycle::submit_plan(&matching, Some("run-filtered-match-old"))
            .expect("persist older matching run");
        agent_task_lifecycle::submit_plan(&matching, Some("run-filtered-match-new"))
            .expect("persist newer matching run");

        for index in 0..1001 {
            agent_task_lifecycle::submit_plan(
                &test_plan(),
                Some(&format!("run-unmatched-{index:04}")),
            )
            .expect("persist newer non-match");
        }

        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "list",
            "--latest",
            "--repo",
            "homeboy",
            "--worktree",
            "/work/homeboy",
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/12242",
            "--state",
            "queued",
            "--run-placement",
            "local",
            "--parent-id",
            "batch-12242",
        ])
        .expect("filtered latest list parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let (result, exit_code) = super::super::run(agent_task).expect("filtered latest runs");
        assert_eq!(exit_code, 0);
        assert_eq!(result["filter"], "latest");
        assert_eq!(result["count"], 1);
        assert_eq!(result["runs"][0]["run_id"], "run-filtered-match-new");

        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "list",
            "--latest",
            "--repo",
            "missing-repo",
        ])
        .expect("no-match latest list parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("expected agent-task command");
        };
        let (result, exit_code) = super::super::run(agent_task).expect("no-match latest runs");
        assert_eq!(exit_code, 0);
        assert_eq!(result["filter"], "latest");
        assert_eq!(result["count"], 0);
        assert_eq!(result["total"], 0);
        assert_eq!(result["runs"], json!([]));
    });
}

#[test]
fn agent_task_timeout_ms_flags_parse_for_cook_run_and_run_plan() {
    let cook_continue = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "cook-continue",
        "cook-123",
        "--timeout-ms",
        "2400000",
    ])
    .expect("cook-continue timeout parses");
    let Commands::AgentTask(agent_task) = cook_continue.command else {
        panic!("expected agent-task command");
    };
    let AgentTaskCommand::CookContinue(args) = agent_task.command else {
        panic!("expected cook-continue command");
    };
    assert_eq!(args.timeout_ms, Some(2_400_000));

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
