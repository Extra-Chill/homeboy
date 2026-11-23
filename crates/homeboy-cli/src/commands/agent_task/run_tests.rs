use super::CookContinueArgs;
use super::{
    apply_preview_worktree_capacity, component_lint_gate, cook_continuation_status,
    cook_preview_result, cook_provider_timeout_disclosure, cook_report_with_continuation,
    cook_resolved_policy_disclosure, detached_cook_route_less_warning, durable_cook_identity_lines,
    preflight_continue_cook, preview_gate_contract_disclosure, project_preview_dirty_admission,
    PreviewReplayArgv,
};
use crate::cli_surface::{Cli, Commands};
use crate::commands::agent_task::AgentTaskCommand;
use clap::Parser;

fn parse_cook(argv: &[&str]) -> super::AgentTaskCookArgs {
    let cli = Cli::try_parse_from(argv).expect("parse Cook");
    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("agent-task command");
    };
    let AgentTaskCommand::Cook(cook) = agent_task.command else {
        panic!("Cook command");
    };
    *cook
}

/// Infer a root component's Cook gates the way admission does: defaults
/// before validation, lint after the base resolves.
fn cook_gates(
    argv: &[&str],
    defaults: &[&str],
    declares_lint: bool,
    retained_gate_contract: bool,
) -> Vec<String> {
    let dir = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    std::fs::write(dir.path().join("homeboy.json"), r#"{"id":"data-machine"}"#).unwrap();
    let mut component = serde_json::json!({
        "id": "data-machine",
        "local_path": dir.path(),
        "default_verify_gates": defaults,
    });
    if declares_lint {
        component["scripts"] = serde_json::json!({"lint": ["echo lint"]});
    }
    let component = serde_json::from_value(component).unwrap();
    let mut args = parse_cook(&[&["homeboy", "agent-task", "cook"], argv].concat());
    super::apply_component_default_verify_gates(&mut args, Some(&component)).unwrap();
    args.base = Some("release".to_string());
    super::apply_component_lint_gate(&mut args, retained_gate_contract, Some(&component)).unwrap();
    args.gates.verify.clone()
}

#[test]
fn component_default_gates_fill_only_an_unconfigured_cook() {
    assert_eq!(
        cook_gates(&[], &["cargo test"], false, false),
        ["cargo test"]
    );
    assert_eq!(
        cook_gates(&["--verify", "npm test"], &["cargo test"], false, false),
        ["npm test"]
    );
}

#[test]
fn component_lint_gate_is_added_beside_explicit_test_gate() {
    // #15888: data-machine#3608 declared only `homeboy review test` and
    // published a PR that failed the component's required CI lint.
    // #17226: the gate checks only what changed since the Cook's base.
    let lint = "HOMEBOY_LINT_BASE=__HOMEBOY_COOK_BASE_SHA__ homeboy --placement local review lint data-machine --path . --changed-since __HOMEBOY_COOK_BASE_SHA__";
    assert_eq!(
        cook_gates(
            &["--verify", "homeboy review test data-machine"],
            &[],
            true,
            false
        ),
        ["homeboy review test data-machine", lint]
    );
    assert_eq!(
        cook_gates(&[], &["cargo test"], true, false),
        ["cargo test", lint]
    );
}

#[test]
fn component_lint_gate_respects_opt_out_replay_and_existing_lint() {
    let test_gate = ["--verify", "homeboy review test data-machine"];
    for (argv, defaults, declares_lint, retained, expected) in [
        (&test_gate[..], &[][..], false, false, &test_gate[1..]),
        (
            &[
                "--no-component-lint-gate",
                "--verify",
                "homeboy review test data-machine",
            ][..],
            &[][..],
            true,
            false,
            &test_gate[1..],
        ),
        (&test_gate[..], &[][..], true, true, &test_gate[1..]),
        (
            &[
                "--verify",
                "homeboy review lint data-machine --changed-since origin/main",
            ][..],
            &[][..],
            true,
            false,
            &["homeboy review lint data-machine --changed-since origin/main"][..],
        ),
        (
            &[][..],
            &["homeboy lint data-machine"][..],
            true,
            false,
            &["homeboy lint data-machine"][..],
        ),
    ] {
        assert_eq!(
            cook_gates(argv, defaults, declares_lint, retained),
            expected
        );
    }
}

#[test]
fn repository_level_cook_skips_component_lint_gate_and_keeps_verify() {
    let dir = tempfile::tempdir().expect("temp repo");
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .expect("git init");
    assert!(status.success());
    for id in ["a", "b"] {
        let nested = dir.path().join("plugins").join(id);
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("homeboy.json"), format!(r#"{{"id":"{id}"}}"#)).unwrap();
    }
    let component = serde_json::from_value(serde_json::json!({
        "id": "repository",
        "local_path": dir.path().to_string_lossy(),
        "scripts": {"lint": ["echo lint"]}
    }))
    .expect("component");
    let mut args = parse_cook(&["homeboy", "agent-task", "cook", "--verify", "echo explicit"]);
    args.base = Some("main".to_string());
    super::apply_component_lint_gate(&mut args, false, Some(&component)).expect("repository gates");
    assert_eq!(args.gates.verify, ["echo explicit"]);

    // The same registration still gets its inferred lint gate when the
    // checkout has a root component manifest.
    std::fs::write(dir.path().join("homeboy.json"), r#"{"id":"repository"}"#).unwrap();
    super::apply_component_lint_gate(&mut args, false, Some(&component)).expect("component gates");
    assert_eq!(args.gates.verify.len(), 2);
    assert!(args.gates.verify[1].contains("review lint repository"));
}

#[test]
fn nested_component_gates_use_candidate_relative_paths() {
    let dir = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap()
        .success());
    let nested = dir.path().join("plugins/users plugin");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("homeboy.json"), r#"{"id":"users"}"#).unwrap();
    let component = serde_json::from_value(serde_json::json!({
        "id": "users",
        "local_path": nested,
        "scripts": {"lint": ["echo lint"]},
        "default_verify_gates": ["homeboy review test users --path ."]
    }))
    .unwrap();
    let mut args = parse_cook(&["homeboy", "agent-task", "cook"]);
    super::apply_component_default_verify_gates(&mut args, Some(&component)).unwrap();
    args.base = Some("main".to_string());
    super::apply_component_lint_gate(&mut args, false, Some(&component)).unwrap();
    assert_eq!(
        args.gates.verify,
        [
            "(cd 'plugins/users plugin' && homeboy review test users --path .)",
            "HOMEBOY_LINT_BASE=__HOMEBOY_COOK_BASE_SHA__ homeboy --placement local review lint users --path 'plugins/users plugin' --changed-since __HOMEBOY_COOK_BASE_SHA__"
        ]
    );
    assert!(!args
        .gates
        .verify
        .join(" ")
        .contains(dir.path().to_str().unwrap()));

    let mut explicit = parse_cook(&[
        "homeboy",
        "agent-task",
        "cook",
        "--verify",
        "echo repository gate",
    ]);
    super::apply_component_default_verify_gates(&mut explicit, Some(&component)).unwrap();
    explicit.base = Some("main".to_string());
    super::apply_component_lint_gate(&mut explicit, false, Some(&component)).unwrap();
    assert_eq!(
        explicit.gates.verify,
        [
            "echo repository gate",
            "HOMEBOY_LINT_BASE=__HOMEBOY_COOK_BASE_SHA__ homeboy --placement local review lint users --path 'plugins/users plugin' --changed-since __HOMEBOY_COOK_BASE_SHA__"
        ]
    );
}

#[test]
fn component_lint_gate_selects_changed_monorepo_sibling_at_gate_time() {
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success());
    };
    git(&["init", "-q"]);
    let components: Vec<homeboy::core::component::Component> =
        ["data-machine", "data-machine-events"]
            .into_iter()
            .map(|id| {
                let path = dir.path().join("plugins").join(id);
                std::fs::create_dir_all(&path).unwrap();
                std::fs::write(path.join("homeboy.json"), "{}").unwrap();
                serde_json::from_value(serde_json::json!({
                    "id": id, "local_path": path,
                    "scripts": {"lint": ["echo lint"]}
                }))
                .unwrap()
            })
            .collect();
    git(&["add", "."]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.com",
        "commit",
        "-qm",
        "base",
    ]);
    git(&["branch", "-m", "trunk"]);
    let base = String::from_utf8(
        std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let siblings = super::lint_components_in_checkout(&components[0], components.clone());
    assert_eq!(siblings.len(), 2);
    let gate = super::changed_component_lint_gate(&siblings, &base).unwrap();
    assert!(!gate.contains(dir.path().to_str().unwrap()));
    // A shell function stands in for homeboy and records exactly which
    // component/path was linted. Run with -e like a strict gate runner.
    let run = || {
        std::process::Command::new("sh")
            .args([
                "-ec",
                &format!("homeboy() {{ printf '%s\\n' \"$*\"; }}; {gate}"),
            ])
            .current_dir(dir.path())
            .output()
            .unwrap()
    };
    assert!(
        run().stdout.is_empty(),
        "unchanged siblings must not be linted"
    );
    std::fs::write(
        dir.path().join("plugins/data-machine-events/homeboy.json"),
        "{ }\n",
    )
    .unwrap();
    let output = run();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(),
        format!("--placement local review lint data-machine-events --path plugins/data-machine-events --changed-since {base}\n"));
    let failed = std::process::Command::new("sh")
        .args(["-ec", &format!("homeboy() {{ return 23; }}; {gate}")])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert_eq!(failed.code(), Some(23), "lint failures must propagate");
    // Once both components change both gates must execute.
    std::fs::write(
        dir.path().join("plugins/data-machine/homeboy.json"),
        "{ }\n",
    )
    .unwrap();
    assert_eq!(String::from_utf8(run().stdout).unwrap().lines().count(), 2);
    assert!(!gate.contains("origin/"));
}

#[test]
fn component_lint_gate_quotes_component_id() {
    assert_eq!(
        component_lint_gate("odd id", ".", "odd base"),
        "HOMEBOY_LINT_BASE='odd base' homeboy --placement local review lint 'odd id' --path . --changed-since 'odd base'"
    );
}

#[test]
fn controller_lint_gate_records_provenance_and_dedupes() {
    let mut args = parse_cook(&[
        "homeboy",
        "agent-task",
        "cook",
        "--verify",
        "homeboy review test data-machine",
    ]);
    assert!(!args.gates.has_retained_contract());
    args.gates
        .snapshot_file_inputs()
        .expect("snapshot inline gates");
    args.gates
        .push_controller_verify(component_lint_gate("data-machine", ".", "main"));
    args.gates
        .push_controller_verify(component_lint_gate("data-machine", ".", "main"));
    assert_eq!(
        args.gates.verify,
        [
            "homeboy review test data-machine",
            "HOMEBOY_LINT_BASE=main homeboy --placement local review lint data-machine --path . --changed-since main"
        ]
    );
    assert_eq!(args.gates.input_sources.len(), 2);
    assert_eq!(args.gates.input_sources[1].source_kind, "inline");
    // A Homeboy-generated replay retains provenance and is not widened.
    assert!(args.gates.has_retained_contract());
}

#[test]
fn no_component_lint_gate_flag_parses() {
    let args = parse_cook(&[
        "homeboy",
        "agent-task",
        "cook",
        "--verify",
        "true",
        "--no-component-lint-gate",
    ]);
    assert!(args.gates.no_component_lint_gate);
}

#[test]
fn preview_worktree_capacity_blocks_local_shortfall_and_reports_safe_action() {
    let mut resolved = serde_json::json!({ "placement": { "selected": "local" } });
    let capacity = serde_json::json!({
        "state": "shortfall",
        "available_bytes": 90,
        "reserved_bytes": 100,
        "shortfall_bytes": 10
    });
    apply_preview_worktree_capacity(&mut resolved, Some(&capacity), None);
    assert_eq!(resolved["placement"]["admission"]["state"], "blocked");
    assert_eq!(
        resolved["placement"]["admission"]["capacity"]["shortfall_bytes"],
        10
    );
    // The preview never inventories merged worktrees itself (#16396); it
    // names the on-demand inspection that measures the reclaimable space.
    assert_eq!(
        resolved["placement"]["admission"]["next_action"],
        "homeboy worktree cleanup --dry-run"
    );
}

#[test]
fn preview_worktree_capacity_admits_at_reserve_and_leaves_other_placement_unblocked() {
    let available = serde_json::json!({ "placement": { "selected": "local" } });
    let mut resolved = available;
    let capacity = serde_json::json!({ "state": "available", "shortfall_bytes": 0 });
    apply_preview_worktree_capacity(&mut resolved, Some(&capacity), None);
    assert!(resolved["placement"].get("admission").is_none());

    for workspace in [
        serde_json::json!({"action": "planned_reuse", "kind": "preview_local"}),
        serde_json::json!({"action": "materialization_required", "kind": "runner"}),
        serde_json::json!({"action": "planned_create", "kind": "runner"}),
    ] {
        let mut lab = serde_json::json!({ "placement": { "selected": "lab", "requested": "lab" }, "workspace": workspace });
        apply_preview_worktree_capacity(
            &mut lab,
            Some(&serde_json::json!({ "state": "shortfall", "shortfall_bytes": 10 })),
            None,
        );
        assert!(lab["placement"].get("admission").is_none(), "{lab}");
    }
}

/// #16149: a Lab-placed Cook only allocates a plain source checkout on
/// the controller — gates, dependency installs and builds run on the
/// admitted runner — so its admission is sized to that checkout, not the
/// flat reconstructable-artifact reserve.
#[test]
fn lab_cook_planned_checkout_is_sized_to_the_checkout_not_the_flat_reserve() {
    let (_root, repository) =
        homeboy::core::test_support::shared_committed_git_repo_fixture("fixture");
    let identity = serde_json::json!({ "workspace_path": repository });
    let mut resolved = serde_json::json!({
        "workspace": { "action": "planned_create", "kind": "native" },
        "placement": { "requested": "lab", "selected": "lab" },
    });
    // The flat reserve reports a 10 GiB shortfall, but 10 GiB of headroom
    // easily holds a megabytes-scale checkout: the Lab Cook is admitted.
    const GIB: u64 = 1024 * 1024 * 1024;
    let capacity = serde_json::json!({
        "state": "shortfall",
        "available_bytes": 10 * GIB,
        "reserved_bytes": 20 * GIB,
        "shortfall_bytes": 10 * GIB,
    });
    apply_preview_worktree_capacity(&mut resolved, Some(&capacity), Some(&identity));
    assert!(
        resolved["placement"].get("admission").is_none(),
        "a checkout-sized allocation must not be blocked by the flat reserve: {resolved}"
    );
    let allocation = &resolved["workspace"]["capacity"]["lab_checkout_allocation"];
    assert!(
        allocation["estimated_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes < GIB),
        "disclosed checkout estimate: {allocation}"
    );
}

/// A controller filesystem that cannot even hold the plain checkout still
/// blocks a Lab-placed Cook, and the next action inspects the worktrees
/// holding reconstructable dependency dirs instead of a category that
/// cannot free the space.
#[test]
fn lab_cook_blocks_when_the_checkout_itself_does_not_fit_and_reports_worktree_artifacts() {
    let (_root, repository) =
        homeboy::core::test_support::shared_committed_git_repo_fixture("fixture");
    let identity = serde_json::json!({ "workspace_path": repository });
    let mut resolved = serde_json::json!({
        "workspace": { "action": "planned_create", "kind": "native" },
        "placement": { "requested": "lab", "selected": "lab" },
    });
    let capacity = serde_json::json!({
        "state": "shortfall",
        "available_bytes": 1024,
        "reserved_bytes": 20_u64 * 1024 * 1024 * 1024,
        "shortfall_bytes": 20_u64 * 1024 * 1024 * 1024 - 1024,
    });
    apply_preview_worktree_capacity(&mut resolved, Some(&capacity), Some(&identity));
    let admission = &resolved["placement"]["admission"];
    assert_eq!(admission["state"], "blocked", "{resolved}");
    let blocker = admission["remaining_blocker"]
        .as_str()
        .expect("checkout-sized blocker");
    assert!(
        blocker.contains("plain source checkout"),
        "blocker is sized to the checkout, not the reserve: {blocker}"
    );
    let next_action = admission["next_action"].as_str().expect("next action");
    assert!(
        next_action.contains("--all-worktrees")
            && next_action.contains(repository.to_str().expect("repository path")),
        "next action inspects repository worktree artifacts: {next_action}"
    );
    assert!(
        admission["capacity"]["lab_checkout_allocation"]["estimated_bytes"]
            .as_u64()
            .is_some(),
        "blocked admission discloses the checkout estimate: {admission}"
    );
}

/// A Lab-placed Cook whose planned checkout cannot be sized (no readable
/// source tree) keeps the reserve-based admission: fail closed rather
/// than guessing.
#[test]
fn planned_controller_worktree_shortfall_blocks_lab_preview() {
    crate::test_support::with_isolated_home(|home| {
        let config_path = home.path().join(".config/homeboy/homeboy.json");
        std::fs::create_dir_all(config_path.parent().expect("config parent"))
            .expect("config directory");
        std::fs::write(
            &config_path,
            r#"{"retention":{"reconstructable_artifact_reserve_bytes":18446744073709551615}}"#,
        )
        .expect("constrained reserve config");
        homeboy::core::defaults::reset_config_cache_for_test();
        let root = tempfile::tempdir().expect("controller filesystem");
        let destination = root.path().join("planned-worktree");
        let capacity =
            homeboy::core::cleanup::reconstructable_artifact_capacity_preview(&destination);
        assert_eq!(capacity["state"], "shortfall");
        let mut resolved = serde_json::json!({
            "workspace": {"action": "planned_create", "kind": "native", "path": destination},
            "placement": {"requested": "lab", "selected": "lab", "admission": super::render_cook_admission(crate::commands::infra::route::CookAdmissionDecision::Admitted)}
        });
        apply_preview_worktree_capacity(&mut resolved, Some(&capacity), None);
        let preview = super::cook_preview_result(
            resolved,
            Vec::new(),
            super::PreviewReplayArgv {
                argv: Vec::new(),
                requires: Vec::new(),
            },
            None,
            None,
        );
        assert_eq!(
            preview["resolved"]["placement"]["admission"]["state"],
            "blocked"
        );
        assert_eq!(preview["resolved"]["workspace"]["capacity"], capacity);
        assert!(preview["failure"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("below its configured reserve")));
        assert_eq!(
            preview["failure"]["next_action"].as_str(),
            Some("homeboy worktree cleanup --dry-run")
        );
        assert!(
            !destination.exists(),
            "capacity preview must not allocate a checkout"
        );
    });
}

#[test]
fn full_preview_checks_capacity_for_missing_worktree_destination() {
    crate::test_support::with_isolated_home(|home| {
        let config_path = home.path().join(".config/homeboy/homeboy.json");
        std::fs::create_dir_all(config_path.parent().expect("config parent"))
            .expect("config directory");
        std::fs::write(
            &config_path,
            r#"{"retention":{"reconstructable_artifact_reserve_bytes":18446744073709551615}}"#,
        )
        .expect("constrained reserve config");
        homeboy::core::defaults::reset_config_cache_for_test();

        let (_root, repository) =
            homeboy::core::test_support::shared_committed_git_repo_fixture("fixture");
        homeboy::core::test_support::write_component_registration(
            home.path(),
            "fixture",
            &repository,
        );
        let destination = "fixture@feature-preview-capacity";
        let head = "feature/preview-capacity";
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "cook",
            "--preview",
            "--backend",
            "fixture",
            "--prompt",
            "implement the issue",
            "--repo",
            "fixture",
            "--workspace",
            repository.to_str().expect("repository path"),
            "--base",
            "main",
            "--head",
            head,
            "--task-url",
            "https://example.test/issues/15150",
            "--to-worktree",
            destination,
            "--placement",
            "local",
            "--no-finalize",
        ])
        .expect("parse full preview command");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task")
        };
        let AgentTaskCommand::Cook(args) = agent_task.command else {
            panic!("Cook")
        };
        let (preview, exit_code) =
            super::preview_cook(*args, None).expect("preview completes with typed capacity denial");
        assert_eq!(exit_code, 0);
        assert_eq!(
            preview["resolved"]["workspace"]["capacity"]["state"], "shortfall",
            "workspace projection: {}",
            preview["resolved"]["workspace"]
        );
        let target = repository
            .parent()
            .expect("worktree parent")
            .join(destination);
        assert!(!target.exists(), "preview must not create destination");
        assert_eq!(
            preview["resolved"]["placement"]["admission"]["state"],
            "blocked"
        );
        assert!(preview["failure"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("below its configured reserve")));
    });
}

#[test]
fn preview_result_render_discloses_public_exact_filter_and_withholds_private_gate() {
    let validation = crate::commands::agent_task::gate_contract::GateContractValidation {
        schema: "homeboy/gate-contract-validation/v1",
        status: "valid",
        gates: vec![
            crate::commands::agent_task::gate_contract::GateContractValidationEntry {
                command: "cargo test public::test_id -- --exact".to_string(),
                kind: "cargo",
                status: "selection_deferred",
                mode: Some("focused".to_string()),
                filter_interpretation: Some("exact".to_string()),
                filter: Some("public::test_id".to_string()),
                selected_count: None,
                selected_ids: None,
                validation: Some("test_inventory_unavailable_without_build".to_string()),
            },
            crate::commands::agent_task::gate_contract::GateContractValidationEntry {
                command: "cargo test private-command-secret -- --exact".to_string(),
                kind: "cargo",
                status: "selection_deferred",
                mode: Some("focused".to_string()),
                filter_interpretation: Some("exact".to_string()),
                filter: Some("private-command-secret".to_string()),
                selected_count: None,
                selected_ids: None,
                validation: Some("test_inventory_unavailable_without_build".to_string()),
            },
        ],
    };
    let mut resolved = serde_json::json!({
        "placement": { "requested": "local" },
        "provider": { "backend": "fixture", "model": "test-model" },
        "workspace": { "path": "/tmp/worktree" },
        "gates": { "public": 1, "private": 1 },
    });
    resolved["gate_contract_validation"] = preview_gate_contract_disclosure(
        &validation,
        &["cargo test public::test_id -- --exact".to_string()],
    );
    let preview = cook_preview_result(
        resolved,
        Vec::new(),
        PreviewReplayArgv {
            argv: vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
            ],
            requires: Vec::new(),
        },
        None,
        None,
    );
    let summary = crate::commands::agent_task_summary::render_agent_task_summary(
        crate::commands::agent_task_summary::AgentTaskSummaryKind::Cook,
        &preview,
    )
    .expect("rendered preview summary");
    assert!(summary.contains("filter `public::test_id` (exact)"));
    assert!(summary.contains("test inventory unavailable without building test binaries"));
    assert!(summary.contains("Private gate selections deferred: 1 (details withheld)"));
    assert!(!summary.contains("private-command-secret"));
}

#[test]
fn cook_admission_subjects_follow_component_release_policy() {
    use super::PrTitlePolicy::{ConventionalCommit, Plain};
    for (policy, title, message, accepted) in [
        (ConventionalCommit, None, None, true),
        (ConventionalCommit, Some("feat(x): add thing"), None, true),
        (
            ConventionalCommit,
            Some("fix!: change contract"),
            None,
            true,
        ),
        (ConventionalCommit, Some("Add thing"), None, false),
        (ConventionalCommit, Some(""), None, false),
        (
            ConventionalCommit,
            None,
            Some("fix(cook): repair thing\n\nDetails"),
            true,
        ),
        (
            ConventionalCommit,
            None,
            Some("Add thing\n\nfix: body is not the subject"),
            false,
        ),
        (ConventionalCommit, None, Some("\nfix: later line"), false),
        (ConventionalCommit, None, Some(""), false),
        (
            ConventionalCommit,
            Some("feat: add thing"),
            Some("Invalid subject"),
            false,
        ),
        (
            Plain,
            Some("Record what PHP was doing when a site start times out"),
            None,
            true,
        ),
        (Plain, None, Some("Add thing\n\nDetails"), true),
        (Plain, Some(""), None, false),
        (Plain, None, Some("\nlater line"), false),
    ] {
        let result = super::validate_cook_subjects(title, message, policy);
        assert_eq!(
            result.is_ok(),
            accepted,
            "{policy:?} {title:?} / {message:?}"
        );
        if let Err(error) = result {
            let error = error.to_string();
            if policy == ConventionalCommit {
                assert!(error.contains("type(scope)?!?: subject"), "{error}");
                assert!(error.contains("minor"), "{error}");
                assert!(error.contains("major"), "{error}");
            } else {
                assert!(error.contains("non-empty"), "{error}");
            }
        }
    }
}

fn title_cook_args(extra: &[&str]) -> super::AgentTaskCookArgs {
    let mut argv = vec![
        "homeboy",
        "agent-task",
        "cook",
        "--prompt",
        "fixture",
        "--cwd",
        ".",
        "--no-finalize",
    ];
    argv.extend_from_slice(extra);
    let Commands::AgentTask(command) = Cli::parse_from(argv).command else {
        panic!("agent-task command");
    };
    let AgentTaskCommand::Cook(args) = command.command else {
        panic!("cook command");
    };
    *args
}

#[test]
fn explicit_local_retry_selects_controller_owned_dispatch_for_lab_recipe() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "retry",
        "cook-previously-lab",
        "--placement",
        "local",
        "--run",
    ])
    .expect("the exact operator retry form parses");
    assert_eq!(
        cli.placement,
        crate::cli_surface::Placement::Local,
        "a post-subcommand --placement maps to the same global CLI field",
    );

    let normalized = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "retry".to_string(),
        "cook-previously-lab".to_string(),
        "--placement".to_string(),
        "local".to_string(),
        "--run".to_string(),
    ];
    let mut preflight = homeboy::core::parsed_command_preflight::ParsedCommandPreflightResult::new(
        normalized.clone(),
        crate::commands::utils::resource_policy::parsed_command_preflight_input(&cli, &normalized),
        None,
        None,
        homeboy::core::parsed_command_preflight::DeferredWorkloadDecision::NotApplicable,
        homeboy::core::parsed_command_preflight::FallbackDirective::None,
        crate::cli_runtime::placement_directive(&cli, None, false),
        None,
    );
    preflight.resource_admission =
        homeboy::core::parsed_command_preflight::ResourceAdmissionDecision::Rejected {
            label: "agent-task retry".to_string(),
            engages_at: homeboy::core::parsed_command_preflight::ResourceHeat::Hot,
            evidence:
                homeboy::core::parsed_command_preflight::ResourceAdmissionEvidence::Observed {
                    pressure: homeboy::core::parsed_command_preflight::ResourceHeat::Hot,
                },
        };
    let _preflight = homeboy::core::parsed_command_preflight::isolate_captured_result_for_test();
    homeboy::core::parsed_command_preflight::capture_result(preflight);
    let captured = homeboy::core::parsed_command_preflight::captured_result()
        .expect("completed CLI preflight captured");
    assert_eq!(captured.placement.requested, cli.placement);
    let refusal = super::explicit_local_retry_admission_refusal(Some(&captured))
        .expect("hot local admission must refuse before reservation");
    assert_eq!(
        refusal.code,
        homeboy::core::ErrorCode::ValidationInvalidArgument
    );
    assert!(refusal
        .message
        .contains("resource admission rejected agent-task retry"));
    homeboy::core::parsed_command_preflight::reset_captured_result_for_test();
}

#[test]
fn only_explicit_cook_title_is_immutable_recipe_title() {
    let explicit = title_cook_args(&["--title", "Explicit title", "--goal", "Goal title"]);
    assert_eq!(explicit.title.as_deref(), Some("Explicit title"));
    let goal = title_cook_args(&["--goal", "  Restore\n cold\t reconstruction  "]);
    assert!(goal.title.is_none());
    let fallback = title_cook_args(&["--repo", "fixture-repository"]);
    assert!(fallback.title.is_none());
}

#[test]
fn preview_projects_each_dirty_admission_state() {
    let workspace = tempfile::tempdir().expect("workspace");
    let git = |args: &[&str]| {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(workspace.path())
            .status()
            .expect("run git")
            .success());
    };
    git(&["init", "--initial-branch=main"]);
    git(&["config", "user.email", "agent@example.test"]);
    git(&["config", "user.name", "Agent"]);
    std::fs::write(workspace.path().join("tracked.txt"), "base\n").expect("write base");
    git(&["add", "tracked.txt"]);
    git(&["commit", "-m", "base"]);

    std::fs::write(workspace.path().join("tracked.txt"), "modified\n")
        .expect("modify tracked file");
    std::fs::write(workspace.path().join("staged.txt"), "staged\n").expect("write staged file");
    git(&["add", "staged.txt"]);
    std::fs::write(workspace.path().join("untracked.txt"), "untracked\n")
        .expect("write untracked file");

    let mut provision = serde_json::json!({ "path": workspace.path() });
    project_preview_dirty_admission(&mut provision);

    assert_eq!(
        provision["admission"]["status"],
        "would_refuse_dirty_candidate"
    );
    assert_eq!(provision["admission"]["changes"]["tracked"], 1);
    assert_eq!(provision["admission"]["changes"]["staged"], 1);
    assert_eq!(provision["admission"]["changes"]["untracked"], 1);
}

#[test]
fn durable_cook_identity_block_leads_with_the_run_id_and_follow_up_commands() {
    let lines = durable_cook_identity_lines(Some("cook-10419"), "cook-10419-attempt-1");

    // The very first thing an interrupted caller sees must be the handle.
    assert!(
        lines[0].contains("cook-10419-attempt-1"),
        "identity block must lead with the durable run id: {lines:?}"
    );
    assert!(lines[0].contains("cook-10419"));
    let joined = lines.join("\n");
    assert!(joined.contains("homeboy --placement local agent-task status cook-10419-attempt-1"));
    assert!(joined.contains("homeboy --placement local runs logs cook-10419-attempt-1"));
    assert!(joined.contains("homeboy --placement local agent-task cancel cook-10419-attempt-1"));
}

#[test]
fn durable_cook_identity_block_omits_a_cook_alias_equal_to_the_run_id() {
    let lines = durable_cook_identity_lines(Some("run-9163"), "run-9163");

    assert!(!lines[0].contains("(cook"), "no redundant alias: {lines:?}");
    assert!(lines[0].contains("run-9163"));
}

#[test]
fn resolved_cook_policy_disclosure_reports_the_execution_budget() {
    let mut plan = homeboy::agents::agent_task_scheduler::AgentTaskPlan::new("plan", vec![]);
    plan.options.execution_budget =
        homeboy::agents::agent_task_scheduler::AgentTaskExecutionBudget::new(4, 1, 2);

    assert_eq!(
        cook_resolved_policy_disclosure(2, &plan),
        "cook: retry policy: 1 initial execution, 1 same-provider remediation retry(ies), 2 rotation(s), 4 provider execution(s) maximum"
    );
}

/// A budget nobody states is a budget an operator can only discover by
/// losing a run to it (#12568), so the preamble reports the resolved value —
/// the inherited default included.
#[test]
fn provider_timeout_disclosure_reports_the_resolved_budget() {
    let mut plan = homeboy::agents::agent_task_scheduler::AgentTaskPlan::new("plan", vec![]);

    assert_eq!(
        cook_provider_timeout_disclosure(&plan),
        format!(
            "cook: provider timeout: {}s per provider execution (override with --timeout-ms)",
            homeboy::agents::agent_task_timeout::DEFAULT_PROVIDER_TIMEOUT_MS / 1_000
        ),
        "the inherited default must be named, not left implicit"
    );

    plan.options.timeout_ms = Some(2_700_000);
    assert_eq!(
        cook_provider_timeout_disclosure(&plan),
        "cook: provider timeout: 2700s per provider execution (override with --timeout-ms)"
    );
}

#[test]
fn detached_route_less_warning_names_only_safe_resolver_diagnostics() {
    let mut resolution =
        homeboy::core::notification_route::NotificationRouteResolution::new("route_less");
    resolution.resolver_transport = Some("generic.completed".to_string());
    resolution.missing_context = vec!["CALLER_THREAD_ID".to_string()];

    let warning = detached_cook_route_less_warning(&resolution).expect("warning");
    assert!(warning.contains("generic.completed"));
    assert!(warning.contains("CALLER_THREAD_ID"));
    assert!(warning.contains("active Discord thread is not inferred"));
    assert!(warning.contains("--notification-transport <transport> --notification-route <route>"));
    assert!(warning.contains("HOMEBOY_NOTIFICATION_TRANSPORT"));
    assert!(!warning.contains("opaque-destination"));

    resolution.classification = "resolver".to_string();
    assert!(detached_cook_route_less_warning(&resolution).is_none());

    resolution.classification = "route_less".to_string();
    resolution.resolver_transport = None;
    resolution.missing_context.clear();
    let warning = detached_cook_route_less_warning(&resolution).expect("warning");
    assert!(warning.contains("no resolver supplied a route"));
    assert!(!warning.contains("no installed resolver transport matched"));
}

#[test]
fn in_flight_cook_report_keeps_provider_state_and_managed_continuation_separate() {
    let report = cook_report_with_continuation(serde_json::json!({
        "cook_id": "cook-1",
        "latest_run_id": "cook-1-attempt-1",
        "status": "in_flight",
        "attempts": [{ "run_state": "Running" }],
    }));

    assert_eq!(report["provider"]["state"], "Running");
    assert_eq!(
        report["remaining_phases"],
        serde_json::json!(["harvest", "review", "gates", "promotion", "finalization"])
    );
    assert_eq!(
        report["continuation_command"],
        "homeboy agent-task recover cook-1-attempt-1"
    );
}

#[test]
fn cook_continue_reports_runner_capacity_without_offering_a_duplicate_dispatch() {
    crate::test_support::with_isolated_home(|_| {
        let plan = homeboy::agents::agent_task_schedule::AgentTaskPlan::new("plan", vec![]);
        homeboy::agents::agent_task_lifecycle::submit_plan(&plan, Some("cook-queue-attempt-1"))
            .expect("persist queued attempt");
        homeboy::agents::agent_task_lifecycle::rewrite_record_for_test(
            "cook-queue-attempt-1",
            |record| {
                record.metadata = serde_json::json!({
                    "runner_id": "fixture-lab",
                    "runner_job_status": "queued",
                    "runner_queue": { "state": "waiting_for_capacity" },
                    "execution_placement_decision": {
                        "requested": "auto",
                        "runner": { "source": "policy" },
                        "override_authorization": { "authorized": false }
                    },
                });
            },
        )
        .expect("record accepted queue ownership");
        let record = homeboy::agents::agent_task_lifecycle::status("cook-queue-attempt-1")
            .expect("read queued attempt");

        let report = cook_continuation_status("cook-queue", &record);

        assert_eq!(report["status"], "accepted_unscheduled");
        assert_eq!(report["guidance"]["action"], "await_runner_capacity");
        assert_eq!(
            report["guidance"]["command"],
            "homeboy runner status fixture-lab"
        );
        assert!(report.get("continuation_command").is_none());
        assert_eq!(report["placement_authority"], "policy_pinned");
    });
}

#[test]
fn cook_continue_reports_a_queued_record_without_runner_as_unscheduled() {
    crate::test_support::with_isolated_home(|_| {
        let plan = homeboy::agents::agent_task_schedule::AgentTaskPlan::new("plan", vec![]);
        homeboy::agents::agent_task_lifecycle::submit_plan(
            &plan,
            Some("cook-unassigned-attempt-1"),
        )
        .expect("persist queued attempt");
        let record = homeboy::agents::agent_task_lifecycle::status("cook-unassigned-attempt-1")
            .expect("read queued attempt without a runner boundary");

        let report = cook_continuation_status("cook-unassigned", &record);

        assert_eq!(report["status"], "accepted_unscheduled");
        assert_eq!(report["guidance"]["action"], "schedule_queued_run");
        assert_eq!(
            report["guidance"]["command"],
            "homeboy agent-task run cook-unassigned-attempt-1"
        );
        assert!(report.get("continuation_command").is_none());
    });
}

#[test]
fn cook_continue_reports_active_runner_work_with_a_watch_command() {
    crate::test_support::with_isolated_home(|_| {
        let plan = homeboy::agents::agent_task_schedule::AgentTaskPlan::new("plan", vec![]);
        homeboy::agents::agent_task_lifecycle::submit_plan(&plan, Some("cook-active-attempt-1"))
            .expect("persist active attempt");
        homeboy::agents::agent_task_lifecycle::mark_running("cook-active-attempt-1")
            .expect("runner starts the provider attempt");
        homeboy::agents::agent_task_lifecycle::rewrite_record_for_test(
            "cook-active-attempt-1",
            |record| {
                record.metadata = serde_json::json!({
                    "runner_id": "fixture-lab",
                    "runner_job_status": "running",
                    "provider_run_ids": ["fixture-provider-run-1"],
                    "phase": "provider_execution",
                });
            },
        )
        .expect("record active runner and provider evidence");
        let record = homeboy::agents::agent_task_lifecycle::status("cook-active-attempt-1")
            .expect("read active runner-backed provider attempt");

        let report = cook_continuation_status("cook-active", &record);

        assert_eq!(report["status"], "observation_in_progress");
        assert_eq!(report["guidance"]["action"], "watch_provider");
        assert_eq!(
            report["guidance"]["command"],
            "homeboy runs logs cook-active-attempt-1"
        );
        assert!(report.get("continuation_command").is_none());
    });
}

#[test]
fn continuation_preflight_reports_a_read_only_rejection_without_creating_a_run() {
    crate::test_support::with_isolated_home(|_| {
        let before = homeboy::agents::agent_task_lifecycle::list_records_in_store(
            &homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()
                .unwrap(),
        )
        .expect("read initial lifecycle records");
        let (report, exit_code) = preflight_continue_cook(CookContinueArgs {
            cook_or_attempt_id: "missing-cook".to_string(),
            ..Default::default()
        })
        .expect("preflight returns a machine-readable rejection");
        let after = homeboy::agents::agent_task_lifecycle::list_records_in_store(
            &homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()
                .unwrap(),
        )
        .expect("read lifecycle records after preflight");

        assert_eq!(exit_code, 1);
        assert_eq!(report["admitted"], false);
        assert!(report.get("run_id").is_some());
        assert!(report.get("failure_context").is_some());
        assert!(!report.to_string().contains("<run-id>"));
        assert_eq!(report["phases"][0]["phase"], "recipe");
        assert_eq!(report["side_effects"]["provider_dispatch"], false);
        assert_eq!(report["side_effects"]["git_mutation"], false);
        assert_eq!(report["side_effects"]["github_mutation"], false);
        assert_eq!(report["side_effects"]["finalization"], false);
        assert_eq!(
            before.len(),
            after.len(),
            "preflight must not materialize a run"
        );
    });
}

/// A pressure wait is durable Cook progress whose start survives every poll.
#[test]
fn local_dispatch_wait_is_durable_cook_progress() {
    crate::test_support::with_isolated_home(|_| {
        let store =
            homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()
                .unwrap();
        let plan = homeboy::agents::agent_task_schedule::AgentTaskPlan::new("plan", vec![]);
        homeboy::agents::agent_task_lifecycle::submit_plan(&plan, Some("cook-hot")).unwrap();
        let progress = |reason: &str| {
            super::record_local_dispatch_wait(&store, "cook-hot", reason);
            store.read_record("cook-hot").unwrap().metadata["cook_progress"].clone()
        };

        let first = progress("machine is hot (load average 40.4 across 32 CPU(s))");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let later = progress("machine is hot (load average 33.1 across 32 CPU(s))");

        assert_eq!(later["phase"], "queued_local_pressure");
        assert_eq!(later["wait_owner"], "cook_queue");
        assert_eq!(
            later["detail"],
            "machine is hot (load average 33.1 across 32 CPU(s))"
        );
        assert_eq!(later["phase_started_at"], first["phase_started_at"]);
        assert!(later["elapsed_ms"].as_u64().unwrap() >= 20);
        super::record_local_dispatch_wait(&store, "foreground-cook", "machine is hot");
        assert!(store.read_record("foreground-cook").is_err());
    });
}

mod local_dispatch_admission {
    use super::super::admit_local_cook_dispatch_with_clock;
    use homeboy::core::local_dispatch_admission::{
        acquire_local_dispatch_lease, active_local_dispatch_count,
    };
    use homeboy::core::resource_policy_context::{
        ResourcePolicyContext, ResourcePolicyHostSnapshot, ResourcePolicyRunnerSelection,
    };
    use std::time::Duration;

    fn context(severity: &str, load_one: Option<f64>, cpu_count: usize) -> ResourcePolicyContext {
        ResourcePolicyContext {
            command: "agent-task cook".to_string(),
            severity: severity.to_string(),
            local_override: false,
            warned: severity != "ok",
            message: None,
            runner_selection: ResourcePolicyRunnerSelection {
                runner_id: None,
                available_runner_ids: Vec::new(),
                readiness_state: "absent".to_string(),
                readiness_reasons: Vec::new(),
                remediation_commands: Vec::new(),
                reason: "local_no_default_runner".to_string(),
            },
            host: ResourcePolicyHostSnapshot {
                load_severity: severity.to_string(),
                load_one,
                load_five: load_one,
                load_fifteen: load_one,
                cpu_count,
                memory_severity: None,
                memory_used_percent: None,
                memory_available_mb: None,
                memory_total_mb: None,
                relevant_process_count: 0,
                process_severity: "ok".to_string(),
                active_rig_lease_count: 0,
                rig_lease_severity: "ok".to_string(),
                rig_lease_concurrency_limit: None,
            },
            // This fixture models local dispatch under load, where no lab
            // reconnect is attempted; #14730 records its outcome here.
            auto_placement_reconnect: None,
        }
    }

    /// Warm pressure admits even beyond the old CPU/4 ceiling.
    #[test]
    fn dispatch_beyond_the_old_ceiling_admits_without_waiting() {
        let temp = tempfile::tempdir().expect("temp dir");
        let data_root = temp.path();
        let holders: Vec<_> = (0..4)
            .map(|index| {
                acquire_local_dispatch_lease(data_root, &format!("holder-{index}"))
                    .expect("acquire holder lease")
            })
            .collect();
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 4);

        let _holders = holders;
        let result = admit_local_cook_dispatch_with_clock(
            "queued-run",
            data_root,
            || Ok(("warm".to_string(), Some(10.0), 18)),
            Duration::from_secs(1),
            |_| panic!("warm pressure must not wait"),
            |_| panic!("warm pressure must not queue"),
        );

        assert!(result.is_ok(), "{:?}", result.err());
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 5);
    }

    /// Hot pressure queues without taking a lease, then admits when cool.
    #[test]
    fn hot_pressure_queues_then_admits_when_pressure_drops() {
        let temp = tempfile::tempdir().expect("temp dir");
        let data_root = temp.path();
        let slept = std::cell::Cell::new(false);

        let result = admit_local_cook_dispatch_with_clock(
            "queued-run",
            data_root,
            || {
                Ok((
                    if slept.get() { "ok" } else { "hot" }.to_string(),
                    Some(30.4),
                    18,
                ))
            },
            Duration::from_secs(1),
            |_| {
                assert_eq!(active_local_dispatch_count(data_root).unwrap(), 0);
                slept.set(true);
            },
            |reason| {
                assert!(reason.contains("hot"));
                assert!(reason.contains("30.4"));
                assert!(reason.contains("18 CPU"));
            },
        );

        assert!(result.is_ok(), "{result:?}");
        assert!(slept.get());
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 1);
    }

    /// Regression: waiting past 1800s must not fail or take a lease.
    #[test]
    fn hot_queue_past_old_timeout_keeps_waiting_until_pressure_drops() {
        let temp = tempfile::tempdir().expect("temp dir");
        let data_root = temp.path();
        let elapsed = std::cell::Cell::new(Duration::ZERO);
        let mut queue_calls = 0;

        let result = admit_local_cook_dispatch_with_clock(
            "never-admitted-run",
            data_root,
            || {
                Ok((
                    if elapsed.get().as_secs() > 1830 {
                        "warm"
                    } else {
                        "hot"
                    }
                    .to_string(),
                    Some(30.4),
                    32,
                ))
            },
            Duration::from_secs(15),
            |interval| {
                assert_eq!(active_local_dispatch_count(data_root).unwrap(), 0);
                elapsed.set(elapsed.get() + interval);
            },
            |_| queue_calls += 1,
        );

        assert!(result.is_ok(), "{result:?}");
        assert!(elapsed.get().as_secs() > 1800);
        assert!(queue_calls > 120, "must still queue past the old timeout");
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 1);
    }

    /// A refreshed measurement, not the stale preflight snapshot, governs admission.
    #[test]
    fn refreshed_pressure_admits_after_hot_preflight() {
        let temp = tempfile::tempdir().expect("temp dir");
        let data_root = temp.path();
        let mut hot_context = context("hot", Some(30.4), 18);
        hot_context.local_override = true;

        let result = admit_local_cook_dispatch_with_clock(
            "override-run",
            data_root,
            || {
                assert_eq!(hot_context.severity, "hot");
                Ok(("ok".to_string(), Some(1.0), 18))
            },
            Duration::from_secs(1),
            |_interval| panic!("capacity is free; must not queue"),
            |_| panic!("capacity is free; must not queue"),
        );
        assert!(result.is_ok(), "{:?}", result.err());
        assert_eq!(active_local_dispatch_count(data_root).unwrap(), 1);
    }
}
