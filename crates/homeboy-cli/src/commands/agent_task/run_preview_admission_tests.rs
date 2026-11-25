use super::*;

/// Shared fixture for the placement tests below: a real Git repository with a
/// linked worktree, and a Cook `--preview` invocation whose declared
/// `--verify` gate is the caller's choice. No Lab runner is registered at
/// all — the readiness snapshot a real dispatch would compute is `absent`,
/// with no available runners.
fn preview_cook_result_for_declared_gate(
    verify_gate: &str,
) -> homeboy::core::Result<(serde_json::Value, i32)> {
    let source = tempfile::NamedTempFile::new().expect("prompt source");
    std::fs::write(source.path(), "Inspect the task workspace.\n").expect("write prompt");
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
        format!("@{}", source.path().display()),
        "--to-worktree".to_string(),
        workspace.to_str().expect("UTF-8 workspace").to_string(),
        "--no-finalize".to_string(),
        "--verify".to_string(),
        verify_gate.to_string(),
    ])
    .expect("parse preview");

    crate::cli_runtime::capture_preflight_result_for_test(
        &cli,
        Some(
            crate::core::parsed_command_preflight::LabReadinessSnapshot {
                state: "absent".to_string(),
                selected_runner_id: None,
                available_runner_ids: Vec::new(),
                reasons: Vec::new(),
                remediation_commands: vec!["homeboy runner connect <runner-id>".to_string()],
                repair_admitted_runner_ids: Vec::new(),
            },
        ),
    );

    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("agent-task command");
    };
    let super::super::super::AgentTaskCommand::Cook(args) = agent_task.command else {
        panic!("Cook command");
    };
    preview_cook(*args, None)
}

/// #14729: with no ready Lab runner, `--placement auto` preview must
/// report the *resolved* placement (local) rather than the request, and
/// must name why — not just that a substitution happened somewhere.
#[test]
fn preview_with_no_lab_runner_reports_local_placement_and_names_the_reason() {
    crate::test_support::with_isolated_home(|_| {
        let (preview, exit_code) =
            preview_cook_result_for_declared_gate("true").expect("compile preview");
        assert_eq!(exit_code, 0);
        assert_eq!(preview["resolved"]["placement"]["requested"], "auto");
        assert_eq!(preview["resolved"]["placement"]["selected"], "local");
        assert_eq!(
            preview["resolved"]["placement"]["fallback_reason"],
            "no Lab runner is configured"
        );

        let summary = crate::commands::agent_task_summary::render_agent_task_summary(
            crate::commands::agent_task_summary::AgentTaskSummaryKind::Cook,
            &preview,
        )
        .expect("cook preview summary renders");
        assert!(
            summary.contains("Placement: local  (requested: auto — no Lab runner is configured)"),
            "summary did not name the resolved placement and reason: {summary}"
        );
    });
}

/// #14731 / #14963: preview must still refuse a `homeboy review test`
/// gate that *pins* an unavailable Lab route — `--placement lab` has no
/// local fallback and hard-fails at execution time instead of deferring,
/// so admitting it would dispatch a provider whose verify gate is
/// already known to fail.
#[test]
fn preview_rejects_admission_for_a_pinned_lab_gate_with_no_ready_lab_runner() {
    crate::test_support::with_isolated_home(|_| {
        let error = preview_cook_result_for_declared_gate(
            "homeboy review test fixture-repository --placement lab",
        )
        .expect_err("a gate pinned to an unavailable Lab route must block preview");
        assert!(
            error
                .message
                .contains("homeboy review test fixture-repository --placement lab"),
            "{}",
            error.message
        );
        assert!(
            error
                .message
                .contains("cannot execute under the resolved local placement"),
            "{}",
            error.message
        );
    });
}

/// #14729: backgrounded/detached dispatches never see preview output, so
/// the non-preview path must report the same resolved-vs-requested
/// substitution independently, and stay silent when nothing diverged.
/// Exercised against directly constructed directives (not the shared
/// process-global capture slot) so this stays deterministic under parallel
/// test execution.
#[test]
fn resolved_execution_placement_line_names_only_a_diverging_fallback() {
    for (requested, reason, expected) in [
        (
            homeboy_lab_contract::Placement::Auto,
            Some("the configured Lab runner is disconnected"),
            "cook: placement: local  (requested: auto — the configured Lab runner is disconnected)",
        ),
        (
            homeboy_lab_contract::Placement::Local,
            None,
            "cook: placement: local",
        ),
    ] {
        let directive = crate::core::parsed_command_preflight::PlacementDirective {
            requested,
            required: homeboy_lab_contract::ExecutionPlacementRequirement::Either,
            selected: homeboy_lab_contract::EffectiveExecutionPlacement::Local,
            runner: None,
            fallback: homeboy_lab_contract::ExecutionPlacementFallback {
                local_allowed: reason.is_some(),
                reason: reason.map(str::to_string),
            },
            override_authorization: homeboy_lab_contract::ExecutionPlacementOverrideAuthorization {
                authorized: reason.is_none(),
                authority: reason
                    .is_none()
                    .then(|| "operator --placement local".to_string()),
            },
        };
        assert_eq!(
            format_resolved_execution_placement_line(&directive),
            expected
        );
    }
}

/// #15039: preview reports the same admitted-component resolution lab staging
/// uses, so a nested registration cannot pass preview and fail staging.
#[test]
fn preview_reports_the_staging_component_for_a_nested_worktree() {
    crate::test_support::with_isolated_home(|home| {
        let root = tempfile::tempdir().expect("repository root");
        let primary = root.path().join("blocks-engine");
        let nested = primary.join("php-transformer");
        let worktree = root.path().join("blocks-engine@fix-issue-2194");
        let remote_url = "https://example.test/org/blocks-engine.git";
        std::fs::create_dir_all(&nested).expect("nested component");
        for args in [
            vec!["init", "--quiet"],
            vec!["config", "user.email", "fixture@example.test"],
            vec!["config", "user.name", "Fixture"],
            vec!["remote", "add", "origin", remote_url],
        ] {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(&primary)
                .status()
                .expect("git setup")
                .success());
        }
        std::fs::write(primary.join("README.md"), "root\n").expect("readme");
        std::fs::write(nested.join("composer.json"), "{}").expect("nested manifest");
        assert!(std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(&primary)
            .status()
            .expect("stage")
            .success());
        assert!(std::process::Command::new("git")
            .args(["commit", "--quiet", "-m", "base"])
            .current_dir(&primary)
            .status()
            .expect("commit")
            .success());
        assert!(std::process::Command::new("git")
            .args(["branch", "-M", "main"])
            .current_dir(&primary)
            .status()
            .expect("rename default branch")
            .success());
        assert!(std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "--quiet",
                "-b",
                "fix/issue-2194",
                worktree.to_str().expect("worktree"),
            ])
            .current_dir(&primary)
            .status()
            .expect("worktree")
            .success());
        let registrations = home.path().join(".config/homeboy/components");
        std::fs::create_dir_all(&registrations).expect("registrations");
        for (id, path) in [("blocks-engine", &primary), ("php-transformer", &nested)] {
            std::fs::write(
                registrations.join(format!("{id}.json")),
                serde_json::json!({
                    "local_path": path,
                    "remote_url": remote_url,
                })
                .to_string(),
            )
            .expect("register component");
        }

        let (preview, exit_code) = preview_cook(
            cook(&[
                "homeboy",
                "agent-task",
                "cook",
                "--base",
                "main",
                "--preview",
                "--backend",
                "fixture",
                "--repo",
                "blocks-engine",
                "--prompt",
                "stage the nested repository",
                "--to-worktree",
                worktree.to_str().expect("worktree"),
                "--no-finalize",
            ]),
            None,
        )
        .expect("preview uses the admitted component");
        assert_eq!(exit_code, 0);
        assert_eq!(
            preview["resolved"]["staging_component"]["component_id"],
            "blocks-engine"
        );
        assert_eq!(preview["resolved"]["staging_component"]["resolved"], true);
        let staged = homeboy::core::deps::dependency_install_plan_for_component(
            &worktree,
            Some("blocks-engine"),
        )
        .expect("staging resolves the same admitted component");
        assert!(staged.is_empty() || staged.iter().all(|step| step.provider_id == "composer"));
    });
}

#[test]
fn local_preview_matches_live_prompt_validation() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::NamedTempFile::new().expect("prompt source");
        std::fs::write(
            source.path(),
            "Read /private/evidence.json before editing.\n",
        )
        .expect("write prompt");
        let args = cook(&[
            "homeboy",
            "agent-task",
            "cook",
            "--preview",
            "--backend",
            "fixture",
            "--prompt",
            &format!("@{}", source.path().display()),
            "--to-worktree",
            env!("CARGO_MANIFEST_DIR"),
            "--no-finalize",
        ]);

        let live = validate_cook_request_with_provenance(&args, None)
            .expect_err("live preflight rejects undeclared path");
        let preview = preview_cook(args, None).expect_err("preview rejects the same path");
        assert_eq!(preview.code, live.code);
        assert_eq!(preview.message, live.message);
        assert_eq!(preview.details, live.details);
    });
}

#[test]
fn local_preview_projects_prompt_evidence_without_execution_admission() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::NamedTempFile::new().expect("evidence source");
        std::fs::write(
            source.path(),
            format!("Read {} before editing.\n", source.path().display()),
        )
        .expect("write prompt containing a declared path");
        let (repository, workspace) = linked_preview_workspace();
        let _capacity = declare_preview_fixture_capacity(repository.path());
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
            "--prompt",
            &format!("@{}", source.path().display()),
            "--to-worktree",
            workspace.to_str().expect("UTF-8 workspace"),
            "--no-finalize",
            "--provider-evidence",
            &format!(
                r#"{{"id":"prompt","source":"{}"}}"#,
                source.path().display()
            ),
        ]);
        let (preview, exit_code) = preview_cook(args.clone(), None).expect("local preview");

        assert_eq!(exit_code, 0);
        assert_eq!(preview["resolved"]["workspace"]["action"], "planned_reuse");
        assert_eq!(
            preview["resolved"]["placement"]["admission"]["revalidate_before_execution"],
            true
        );
        let evidence_path = preview["resolved"]["provider"]["config"]["evidence_inputs"][0]["path"]
            .as_str()
            .expect("projected evidence path");
        assert!(
            !evidence_path.starts_with(workspace.to_str().expect("UTF-8 workspace")),
            "preview must not stage evidence inside the candidate: {evidence_path}"
        );
        assert!(!evidence_path.contains(&source.path().display().to_string()));
        assert!(
            validate_provider_evidence_prompt(
                Some(&format!("Read {evidence_path}-neighbor.")),
                &projected_provider_evidence_paths(
                    preview["resolved"]["provider"]["config"]["evidence_inputs"]
                        .as_array()
                        .unwrap()
                )
            )
            .is_err(),
            "a neighboring undeclared path remains rejected"
        );
        assert!(
            !workspace.join(".homeboy").exists(),
            "preview must not create candidate evidence dirt"
        );
        let phases = preview["progress"].as_array().expect("preview phases");
        assert!(phases.iter().any(|phase| phase["phase"] == "prompt_input"));
        assert!(phases
            .iter()
            .any(|phase| phase["phase"] == "provider_evidence_projection"));
        assert!(phases
            .iter()
            .any(|phase| phase["phase"] == "plan_compilation"));

        let preview_status = std::process::Command::new("git")
            .args([
                "-C",
                workspace.to_str().expect("UTF-8 workspace"),
                "status",
                "--porcelain",
            ])
            .output()
            .expect("read workspace status after preview");
        assert!(preview_status.status.success());
        assert!(
            preview_status.stdout.is_empty(),
            "preview must not dirty the destination: {}",
            String::from_utf8_lossy(&preview_status.stdout)
        );

        let args = {
            let mut input = args;
            match crate::commands::agent_task::run::resolve_cook_repository(&mut input) {
                Ok(repository) => resolve_cook_destination(input, repository.as_ref()),
                Err(error) => Err(error),
            }
        }
        .expect("resolve live destination");
        let live = compile_cook_plan(
            &args,
            serde_json::json!({
                "action": "existing",
                "path": workspace,
            }),
            None,
        )
        .expect("compile live Cook");
        assert_eq!(
            live.tasks[0].executor.config["evidence_inputs"][0]["path"], evidence_path,
            "preview and live Cook must project the same provider evidence path"
        );
        assert!(
            Path::new(evidence_path).is_file(),
            "live Cook materializes its declared evidence projection"
        );
        assert_eq!(
            std::fs::read_dir(Path::new(evidence_path).parent().unwrap())
                .unwrap()
                .count(),
            1,
            "native parent-directory access must not expose neighboring blobs"
        );
    });
}

#[test]
fn declared_evidence_reaches_reading_provider_and_changes_candidate_commit() {
    crate::test_support::with_isolated_home(|_| {
        use homeboy::agents::agent_task_provider::{
            AgentTaskProviderCatalog, ExtensionProviderAgentTaskExecutor,
        };
        use homeboy::agents::agent_task_scheduler::AgentTaskScheduler;
        let (_repository, workspace) = linked_preview_workspace();
        let fixtures = tempfile::tempdir().unwrap();
        let source = fixtures.path().join("value.txt");
        let tree = fixtures.path().join("tree");
        std::fs::create_dir(&tree).unwrap();
        std::fs::write(tree.join("member.txt"), "tree-bytes").unwrap();
        let script = fixtures.path().join("reader.cjs");
        std::fs::write(&script, r#"
const fs = require('fs'), path = require('path');
const instructions = process.argv.at(-1);
const inputs = JSON.parse(instructions.trim().split('\n').pop());
if (inputs.length !== 2 || inputs.some(i => !i.read_only)) throw Error('missing read-only locations');
if (instructions.includes('DO_NOT_FORWARD')) throw Error('private config leaked into instructions');
const values = inputs.map(input => {
  const file = fs.statSync(input.path).isDirectory() ? path.join(input.path, 'member.txt') : input.path;
  return fs.readFileSync(file, 'utf8');
});
fs.writeFileSync('evidence-result.txt', values.join('|'));
process.stdout.write(JSON.stringify({type:'message_end',message:{role:'assistant',stopReason:'stop',content:[{type:'text',text:values.join('|')}]}}));
"#).unwrap();
        let fake = FakePi::with_probes(
            &format!("exec node '{}' \"$@\"", script.display()),
            "printf 'test model\\n'",
        );
        fake.install_catalog_config();
        let provider = fake.provider();
        let executor = std::sync::Arc::new(ExtensionProviderAgentTaskExecutor::from_catalog(
            AgentTaskProviderCatalog {
                providers: vec![provider],
                ..Default::default()
            },
        ));
        let mut immutable_trees = std::collections::BTreeSet::new();
        for bytes in ["first-source", "second-source"] {
            std::fs::write(&source, bytes).unwrap();
            let args = cook(&[
                "homeboy",
                "agent-task",
                "cook",
                "--backend",
                "pi",
                "--prompt",
                &format!("Inspect {} and {}.", source.display(), tree.display()),
                "--to-worktree",
                workspace.to_str().unwrap(),
                "--no-finalize",
                "--provider-evidence",
                &format!(r#"{{"id":"file","source":"{}"}}"#, source.display()),
                "--provider-evidence",
                &format!(r#"{{"id":"tree","source":"{}"}}"#, tree.display()),
            ]);
            let mut plan = compile_cook_plan(
                &args,
                serde_json::json!({"action":"existing","path":workspace}),
                None,
            )
            .unwrap();
            immutable_trees.insert(PathBuf::from(
                plan.tasks[0].executor.config["evidence_inputs"][1]["path"]
                    .as_str()
                    .unwrap(),
            ));
            for task in &mut plan.tasks {
                task.expected_artifacts.clear();
                task.artifact_declarations.clear();
                task.executor.backend = "pi".into();
                task.executor.selector = Some("pi.agent-task-executor".into());
                task.executor.model = Some("test/model".into());
                task.executor.config["command"] = fake.config()["command"].clone();
                task.executor.config["evidence_inputs"][0]["provenance"] =
                    serde_json::json!({"private":"DO_NOT_FORWARD"});
            }
            let aggregate = AgentTaskScheduler::new(executor.clone()).run(plan);
            let outcome = &aggregate.outcomes[0];
            assert_eq!(
                outcome.status,
                AgentTaskOutcomeStatus::Succeeded,
                "{outcome:?}"
            );
            assert_eq!(
                outcome.summary.as_deref(),
                Some(format!("{bytes}|tree-bytes").as_str())
            );
            let candidate = outcome.candidate.as_ref().expect("harvested candidate");
            let diff = std::process::Command::new("git")
                .args(["diff", &candidate.base_sha, &candidate.candidate_sha])
                .current_dir(&workspace)
                .output()
                .unwrap();
            assert!(diff.status.success());
            let patch = String::from_utf8(diff.stdout).unwrap();
            assert!(patch.contains("evidence-result.txt") && patch.contains(bytes));
            assert!(!patch.contains("member.txt") && !patch.contains("value.txt"));
            assert_eq!(std::fs::read_to_string(&source).unwrap(), bytes);
            assert_eq!(
                std::fs::read_to_string(tree.join("member.txt")).unwrap(),
                "tree-bytes"
            );
        }
        #[cfg(unix)]
        for tree in immutable_trees {
            use std::os::unix::fs::PermissionsExt;
            // Test-owner cleanup happens only after both provider runs.
            std::fs::set_permissions(tree, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    });
}

/// Scopes the captured controller preflight to this test thread so a
/// concurrently running preview test cannot observe or overwrite it.
struct ResetCapturedPreflight {
    _scope: homeboy::core::parsed_command_preflight::CapturedResultTestScope,
}

impl ResetCapturedPreflight {
    fn new() -> Self {
        Self {
            _scope: homeboy::core::parsed_command_preflight::isolate_captured_result_for_test(),
        }
    }
}

fn preview_cook_with_captured_runner(runner_id: Option<&str>) -> (Value, i32) {
    let (repository, workspace) = linked_preview_workspace();
    let _capacity =
        homeboy::core::observation::disk_budget::test_seam::ample_disk_capacity_for_test(
            repository.path(),
        );
    let cli = Cli::try_parse_from([
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--preview".to_string(),
        "--backend".to_string(),
        "fixture".to_string(),
        "--repo".to_string(),
        "fixture-repository".to_string(),
        "--prompt".to_string(),
        "Inspect the task workspace.".to_string(),
        "--base".to_string(),
        "main".to_string(),
        "--to-worktree".to_string(),
        workspace.to_str().expect("UTF-8 workspace").to_string(),
        "--no-finalize".to_string(),
        "--verify".to_string(),
        "true".to_string(),
    ])
    .expect("parse preview");
    if let Some(runner_id) = runner_id {
        crate::cli_runtime::capture_admitted_runner_preflight_for_test(&cli, runner_id);
    }
    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("agent-task command");
    };
    let super::super::super::AgentTaskCommand::Cook(args) = agent_task.command else {
        panic!("Cook command");
    };
    preview_cook(*args, None).expect("compile preview")
}

#[test]
fn preview_that_passes_admission_uses_the_same_connected_ready_evidence_execution_requires() {
    crate::test_support::with_isolated_home(|_| {
        let _reset = ResetCapturedPreflight::new();
        let (preview, exit_code) = preview_cook_with_captured_runner(Some("homeboy-lab"));

        assert_eq!(exit_code, 0);
        assert_eq!(
            preview["resolved"]["placement"]["selected_runner"],
            "homeboy-lab"
        );
        assert_eq!(
            preview["resolved"]["placement"]["admission"]["state"],
            "admissible"
        );
        assert!(preview.get("failure").is_none(), "{preview}");

        let captured = homeboy::core::parsed_command_preflight::captured_result()
            .expect("admitted runner preflight remains captured");
        assert_eq!(captured.selected_runner_id.as_deref(), Some("homeboy-lab"));
        let readiness = captured.lab_readiness.as_ref().expect("lab readiness");
        assert_eq!(readiness.state, "connected_ready");
        assert_eq!(readiness.selected_runner_id.as_deref(), Some("homeboy-lab"));
        assert!(readiness
            .available_runner_ids
            .iter()
            .any(|id| id == "homeboy-lab"));

        let summary = crate::commands::agent_task_summary::render_agent_task_summary(
            crate::commands::agent_task_summary::AgentTaskSummaryKind::Cook,
            &preview,
        )
        .expect("cook preview summary renders");
        assert!(
            !summary.contains("Blocked:"),
            "admissible preview must not report a blocker: {summary}"
        );
        assert!(
            !summary.contains("static inputs only; admission not checked"),
            "admissible preview must not claim admission was skipped: {summary}"
        );
    });
}

#[test]
fn preview_provider_secrets_require_a_runner_ref_only_when_the_plan_declares_one() {
    crate::test_support::with_isolated_home(|_| {
        let runner_id = "lab-secret-preview";
        homeboy::runner::runners::create(
            &format!(r#"{{"id":"{runner_id}","kind":"local"}}"#),
            false,
        )
        .expect("register runner without secret refs");
        let placement = serde_json::json!({
            "selected": "lab",
            "selected_runner": runner_id,
        });
        let plan: homeboy::agents::agent_task_schedule::AgentTaskPlan =
            serde_json::from_value(serde_json::json!({
                "schema": "homeboy/agent-task-plan/v1",
                "plan_id": "provider-secret-preview",
                "tasks": [{
                    "schema": "homeboy/agent-task-request/v1",
                    "task_id": "provider-route",
                    "executor": { "backend": "fixture" },
                    "instructions": "Check the runner secret contract."
                }],
            }))
            .expect("parse plan");

        preflight_preview_lab_provider_secret_env(&placement, &plan)
            .expect("a no-secret plan needs no runner credential");

        let mut required_plan = plan;
        required_plan.tasks[0].executor.secret_env =
            vec!["PROVIDER_PREVIEW_SECRET_TEST".to_string()];
        let error = preflight_preview_lab_provider_secret_env(&placement, &required_plan)
            .expect_err("missing runner-owned reference must block preview");
        assert!(error.message.contains("PROVIDER_PREVIEW_SECRET_TEST"));

        let configured_runner = "lab-secret-configured-preview";
        homeboy::runner::runners::create(
                &format!(
                    r#"{{"id":"{configured_runner}","kind":"local","secret_env":{{"PROVIDER_PREVIEW_SECRET_TEST":{{"env":"PROVIDER_PREVIEW_SECRET_TEST"}}}}}}"#
                ),
                false,
            )
            .expect("register runner with secret reference");
        let configured_placement = serde_json::json!({
            "selected": "lab",
            "selected_runner": configured_runner,
        });
        preflight_preview_lab_provider_secret_env(&configured_placement, &required_plan)
            .expect("present runner reference admits the same plan");
    });
}

#[test]
fn local_preview_omits_runner_admission_and_replay_prerequisites() {
    let _reset = ResetCapturedPreflight::new();
    let cli = Cli::try_parse_from([
        "homeboy",
        "--placement",
        "local",
        "agent-task",
        "cook",
        "--preview",
    ])
    .unwrap();
    crate::cli_runtime::capture_preflight_result_for_test(&cli, None);
    let args = cook(&[
        "homeboy",
        "--placement",
        "local",
        "agent-task",
        "cook",
        "--preview",
        "--prompt",
        "implement the issue",
    ]);
    let replay = finalize_cook_preview_replay(
        [
            "homeboy",
            "--placement",
            "local",
            "agent-task",
            "cook",
            "--prompt",
            "implement the issue",
        ]
        .into_iter()
        .map(str::to_string),
        &args,
    );
    let policy = preview_placement_policy_with_admission(&replay.argv);

    assert!(policy.get("admission").is_none());
    assert!(!replay
        .requires
        .iter()
        .any(|requirement| requirement.contains("runner placement admission")));
    assert!(
        !replay
            .requires
            .iter()
            .any(|requirement| { requirement.contains("base admission is indeterminate") }),
        "unmaterialized preview must not require a second preview: {replay:?}"
    );
}

#[test]
fn shared_cook_admission_matrix() {
    use crate::commands::infra::route::{cook_admission_decision, CookAdmissionDecision};
    use homeboy::core::parsed_command_preflight::*;
    crate::test_support::with_isolated_home(|_| {
        let _reset = ResetCapturedPreflight::new();
        for (placement, runner, unavailable, rejected, expected) in [
            ("auto", None, false, false, CookAdmissionDecision::Admitted),
            ("local", None, false, false, CookAdmissionDecision::Admitted),
            (
                "lab-or-local",
                None,
                true,
                false,
                CookAdmissionDecision::Admitted,
            ),
            (
                "lab",
                Some("ready"),
                false,
                false,
                CookAdmissionDecision::Admitted,
            ),
            (
                "auto",
                Some("ready"),
                false,
                false,
                CookAdmissionDecision::Admitted,
            ),
            (
                "auto",
                None,
                false,
                true,
                CookAdmissionDecision::Indeterminate {
                    reason: "resource admission rejected for Cook".into(),
                    next_action: "homeboy self doctor".into(),
                },
            ),
        ] {
            let cli = Cli::try_parse_from([
                "homeboy",
                "--placement",
                placement,
                "agent-task",
                "cook",
                "--preview",
            ])
            .unwrap();
            if let Some(runner) = runner {
                crate::cli_runtime::capture_admitted_runner_preflight_for_test(&cli, runner);
            } else {
                crate::cli_runtime::capture_preflight_result_for_test(
                    &cli,
                    unavailable.then(|| LabReadinessSnapshot {
                        state: "disconnected".into(),
                        selected_runner_id: Some("lab".into()),
                        available_runner_ids: vec![],
                        reasons: vec!["not_connected".into()],
                        remediation_commands: vec![],
                        repair_admitted_runner_ids: vec![],
                    }),
                );
            }
            let mut result = captured_result().unwrap();
            result.resource_admission = if rejected {
                ResourceAdmissionDecision::Rejected {
                    label: "Cook".into(),
                    engages_at: ResourceHeat::Hot,
                    evidence: ResourceAdmissionEvidence::Observed {
                        pressure: ResourceHeat::Hot,
                    },
                }
            } else {
                ResourceAdmissionDecision::Admitted
            };
            assert_eq!(
                cook_admission_decision(&result),
                expected,
                "{placement}, runner={runner:?}, unavailable={unavailable}, rejected={rejected}"
            );
        }
    });
}

#[test]
fn shallow_source_preview_plans_fetch_without_mutation() {
    crate::test_support::with_isolated_home(|_| {
        let root = tempfile::tempdir().unwrap();
        let _capacity = declare_preview_fixture_capacity(root.path());
        let seed = root.path().join("seed");
        std::fs::create_dir(&seed).unwrap();
        let git = |path: &Path, args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_string()
        };
        git(&seed, &["init", "-b", "main"]);
        git(&seed, &["config", "user.email", "fixture@example.test"]);
        git(&seed, &["config", "user.name", "Fixture"]);
        for content in ["base", "tip"] {
            std::fs::write(seed.join("file"), content).unwrap();
            git(&seed, &["add", "."]);
            git(&seed, &["commit", "-m", content]);
        }
        let remote = root.path().join("remote.git");
        git(
            root.path(),
            &[
                "clone",
                "--bare",
                seed.to_str().unwrap(),
                remote.to_str().unwrap(),
            ],
        );
        let source = root.path().join("source");
        git(
            root.path(),
            &[
                "clone",
                "--depth",
                "1",
                "--branch",
                "main",
                &format!("file://{}", remote.display()),
                source.to_str().unwrap(),
            ],
        );
        let mut args = cook(&[
            "homeboy",
            "agent-task",
            "cook",
            "--preview",
            "--backend",
            "fixture",
            "--prompt",
            "implement",
            "--repo",
            source.to_str().unwrap(),
            "--base",
            "main",
            "--head",
            "fix/shallow",
            "--to-worktree",
            "source@fix-shallow",
            "--no-finalize",
        ]);
        let (preview, _) = preview_cook(args.clone(), None).expect("read-only Cook preview");
        assert_eq!(
            preview["resolved"]["workspace"]["action"],
            "planned_unshallow"
        );
        let repository = prepare_cook_request(&mut args, None).unwrap();
        let args = resolve_cook_destination(args, repository.as_ref()).unwrap();
        assert_eq!(
            git(&source, &["rev-parse", "--is-shallow-repository"]),
            "true"
        );
        git(&source, &["remote", "remove", "origin"]);
        let error = provision_cook_destination(&args, repository.as_ref(), true).unwrap_err();
        assert!(error
            .message
            .contains("source checkout is shallow and has no fetchable remote"));
        assert_eq!(
            git(&source, &["rev-parse", "--is-shallow-repository"]),
            "true"
        );
    });
}

#[test]
fn cook_validation_rejects_mismatched_native_handles() {
    crate::test_support::with_isolated_home(|_| {
        for preview in [false, true] {
            for handle in ["other@fix-17598", "homeboy@wrong-head"] {
                let mut args = cook(&[
                    "homeboy",
                    "agent-task",
                    "cook",
                    "--repo",
                    "homeboy",
                    "--head",
                    "fix/17598",
                    "--to-worktree",
                    handle,
                    "--prompt",
                    "fix the issue",
                    "--no-finalize",
                ]);
                args.preview = preview;
                let error = validate_cook_request_with_provenance(&args, None)
                    .expect_err("mismatched handle must fail before admission");
                assert!(
                    error.message.contains("expected `homeboy@fix-17598`"),
                    "{error:?}"
                );
            }
        }
    });
}

#[test]
fn cook_derives_native_handle_from_repository_and_head() {
    crate::test_support::with_isolated_home(|_| {
        let root = tempfile::tempdir().unwrap();
        let checkout = root.path().join("repository");
        std::fs::create_dir(&checkout).unwrap();
        for repository in ["repository".to_string(), checkout.display().to_string()] {
            for (head, expected) in [
                ("fix/17598", "repository@fix-17598"),
                ("Fix/Mixed__Case", "repository@Fix-Mixed__Case"),
            ] {
                let mut args = cook(&[
                    "homeboy",
                    "agent-task",
                    "cook",
                    "--repo",
                    &repository,
                    "--head",
                    head,
                    "--base",
                    "main",
                    "--prompt",
                    "fix the issue",
                    "--no-finalize",
                ]);
                // Component identity must not replace repository identity.
                args.repository_identity = Some(
                    json!({"component_id": "renamed-component", "repository_path": repository}),
                );
                let resolved = resolve_cook_destination(args, None).unwrap();
                assert_eq!(resolved.to_worktree.as_deref(), Some(expected));
            }
        }
    });
}

#[test]
fn unmaterialized_preview_admits_replay_and_cook_materializes_origin_base() {
    let _lookup = issue_lookup_fixture();
    crate::test_support::with_isolated_home(|_| {
        let root = tempfile::tempdir().expect("repository root");
        let _capacity = declare_preview_fixture_capacity(root.path());
        let remote = tempfile::tempdir().expect("bare origin");
        assert!(Command::new("git")
            .args(["init", "--bare", "--quiet"])
            .current_dir(remote.path())
            .status()
            .expect("initialize bare origin")
            .success());
        let checkout = root.path().join("fixture");
        std::fs::create_dir(&checkout).expect("repository checkout");
        for args in [
            ["init", "--quiet", "-b", "main"].as_slice(),
            ["config", "user.email", "fixture@example.test"].as_slice(),
            ["config", "user.name", "Fixture"].as_slice(),
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&checkout)
                .status()
                .expect("configure source")
                .success());
        }
        std::fs::write(checkout.join("README.md"), "fixture\n").expect("write fixture");
        assert!(Command::new("git")
            .args(["add", "README.md"])
            .current_dir(&checkout)
            .status()
            .expect("stage fixture")
            .success());
        assert!(Command::new("git")
            .args(["commit", "--quiet", "-m", "fixture"])
            .current_dir(&checkout)
            .status()
            .expect("commit fixture")
            .success());
        let origin = remote.path().to_str().expect("origin path");
        assert!(Command::new("git")
            .args(["remote", "add", "origin", origin])
            .current_dir(&checkout)
            .status()
            .expect("add origin")
            .success());
        assert!(Command::new("git")
            .args(["push", "--quiet", "-u", "origin", "main"])
            .current_dir(&checkout)
            .status()
            .expect("push origin main")
            .success());
        let origin_sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&checkout)
                .output()
                .expect("resolve origin SHA")
                .stdout,
        )
        .expect("origin SHA is UTF-8")
        .trim()
        .to_string();
        let evidence_source = tempfile::NamedTempFile::new().expect("evidence source");
        std::fs::write(evidence_source.path(), "issue evidence\n").expect("write evidence");
        let evidence_arg = format!(
            r#"{{"id":"issue","source":"{}"}}"#,
            evidence_source.path().display()
        );
        let repository = checkout.display().to_string();
        let handle = "fixture@fix-issue-14714";
        let args = cook(&[
            "homeboy",
            "--placement",
            "local",
            "agent-task",
            "cook",
            "--preview",
            "--backend",
            "fixture",
            "--prompt",
            "implement the issue",
            "--repo",
            &repository,
            "--task-url",
            "https://github.com/example/fixture/issues/14714",
            "--head",
            "fix/issue-14714",
            "--base",
            "main",
            "--to-worktree",
            handle,
            "--no-finalize",
            "--provider-evidence",
            &evidence_arg,
        ]);

        let (preview, exit_code) = preview_cook(args.clone(), None).expect("preview");
        assert_eq!(exit_code, 0);
        assert_eq!(preview["resolved"]["workspace"]["action"], "planned_create");
        assert_eq!(
            preview["resolved"]["base_preparation"]["provenance"],
            "workspace_unmaterialized"
        );
        let requires = preview["replay_requires"]
            .as_array()
            .expect("preview replay requirements");
        assert!(
            !requires.iter().any(|requirement| {
                requirement
                    .as_str()
                    .is_some_and(|value| value.contains("base admission is indeterminate"))
            }),
            "unmaterialized preview must admit replay: {preview}"
        );
        assert!(
            !Path::new(
                preview["resolved"]["workspace"]["path"]
                    .as_str()
                    .expect("planned path")
            )
            .exists(),
            "preview must not materialize the planned worktree"
        );

        let provision = provision_cook_destination(
            &args,
            crate::commands::agent_task::run::resolve_cook_repository(&mut args.clone())
                .unwrap()
                .as_ref(),
            false,
        )
        .expect("execution admits planned create");
        assert_eq!(provision["action"], "lookup_pending");
        let plan =
            compile_cook_plan(&args, provision.clone(), None).expect("compile deferred Cook");
        let projected_path = plan.tasks[0].executor.config["evidence_inputs"][0]["path"]
            .as_str()
            .expect("deferred projected evidence path");
        assert!(Path::new(projected_path).is_file());

        let executor = Arc::new(UnmaterializedPreviewExecutor);
        let (report, _) = run_cook(
            args,
            CookEntryContext {
                executor,
                attempt_dispatcher: None,
                progress: None,
                provenance: None,
            },
        )
        .expect("cook replay");
        assert_eq!(
            report["failure_context"]["provider_executions_consumed"], 1,
            "{report}"
        );
        let destination = PathBuf::from(
            preview["resolved"]["workspace"]["path"]
                .as_str()
                .expect("planned path"),
        );
        assert!(
            destination.is_dir(),
            "cook must materialize the planned worktree: {}",
            destination.display()
        );
        let destination_sha = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&destination)
                .output()
                .expect("resolve destination SHA")
                .stdout,
        )
        .expect("destination SHA is UTF-8")
        .trim()
        .to_string();
        assert_eq!(destination_sha, origin_sha);
    });
}

#[test]
fn cook_preview_and_provision_defer_restoration_of_a_missing_native_task_checkout() {
    let _lookup = issue_lookup_fixture();
    crate::test_support::with_isolated_home(|home| {
        let (_root, source) =
            homeboy::core::test_support::shared_committed_git_repo_fixture("fixture");
        homeboy::core::test_support::write_component_registration(home.path(), "fixture", &source);
        let created =
            homeboy::core::worktree::create(homeboy::core::worktree::WorktreeCreateOptions {
                component_id: "fixture".to_string(),
                branch: "fix/restore".to_string(),
                from: Some("main".to_string()),
                task_url: Some("https://example.test/issues/15565".to_string()),
                run_id: None,
                cleanup_policy: None,
                require_handoff_freshness: false,
                source_path: None,
            })
            .expect("native task checkout");
        let path = PathBuf::from(&created.record.worktree_path);
        std::fs::remove_dir_all(&path).expect("simulate deleted checkout");
        let args = cook(&[
            "homeboy",
            "--placement",
            "local",
            "agent-task",
            "cook",
            "--preview",
            "--backend",
            "fixture",
            "--repo",
            "fixture",
            "--task-url",
            "https://example.test/issues/15565",
            "--head",
            "fix/restore",
            "--base",
            "main",
            "--to-worktree",
            "fixture@fix-restore",
            "--prompt",
            "repair the task",
            "--verify",
            "true",
            "--no-finalize",
        ]);
        let (preview, exit) = preview_cook(args.clone(), None).expect("restoration preview");
        assert_eq!(exit, 0);
        assert_eq!(preview["resolved"]["workspace"]["action"], "planned_create");
        let provision = provision_cook_destination(
            &args,
            crate::commands::agent_task::run::resolve_cook_repository(&mut args.clone())
                .unwrap()
                .as_ref(),
            false,
        )
        .expect("deferred restoration");
        assert_eq!(provision["action"], "lookup_pending");
        assert_eq!(provision["handle"], created.record.id);
        assert!(!path.exists(), "restoration waits for durable admission");
    });
}

#[test]
fn read_only_evidence_projection_uses_the_controller_store() {
    crate::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        let source = tempfile::NamedTempFile::new().expect("evidence");
        std::fs::write(source.path(), "evidence").expect("write evidence");
        let evidence = vec![AgentTaskProviderEvidenceInput {
            id: "issue".to_string(),
            source: source.path().display().to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
        }];
        validate_provider_evidence_inputs(&evidence, Some("Read the evidence."))
            .expect("validate evidence");
        let projected = projected_provider_evidence(&evidence, workspace.path().to_str())
            .expect("project read-only evidence path");
        let path = projected[0]["path"].as_str().expect("projection path");
        assert!(!path.starts_with(workspace.path().to_str().expect("workspace path")));
        assert!(path.contains("provider-evidence"));
        assert!(projected[0]["read_only"].as_bool().expect("read-only flag"));
    });
}

#[test]
fn compile_cook_plan_projects_evidence_before_deferred_workspace_binding() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::NamedTempFile::new().expect("evidence");
        std::fs::write(source.path(), "deferred evidence\n").expect("write evidence");
        let evidence_arg = format!(r#"{{"id":"issue","source":"{}"}}"#, source.path().display());
        let args = cook(&[
            "homeboy",
            "agent-task",
            "cook",
            "--backend",
            "fixture",
            "--prompt",
            "read the evidence",
            "--to-worktree",
            "missing@worktree",
            "--no-finalize",
            "--provider-evidence",
            &evidence_arg,
        ]);
        let plan = compile_cook_plan(
            &args,
            serde_json::json!({ "action": "lookup_pending" }),
            None,
        )
        .expect("deferred evidence is admitted before workspace binding");
        let projected_path = plan.tasks[0].executor.config["evidence_inputs"][0]["path"]
            .as_str()
            .expect("projected evidence path");
        assert!(Path::new(projected_path).is_file());
        assert_eq!(plan.metadata["cook_provision"]["action"], "lookup_pending");
    });
}

#[test]
fn preview_and_execution_plan_absent_provider_worktree_without_task_url() {
    crate::test_support::with_isolated_home(|home| {
        let (_root, repository) =
            homeboy::core::test_support::shared_committed_git_repo_fixture("fixture");
        homeboy::core::test_support::write_component_registration(
            home.path(),
            "fixture",
            &repository,
        );
        let handle = "fixture@feature-no-tracker";
        let args = cook(&[
            "homeboy",
            "agent-task",
            "cook",
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
            "feature/no-tracker",
            "--to-worktree",
            &handle,
            "--no-finalize",
        ]);

        let preview = {
            let mut input = args.clone();
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
        .expect("preview plans the worktree from repo, base and head");
        let execution = provision_cook_destination(
            &args,
            crate::commands::agent_task::run::resolve_cook_repository(&mut args.clone())
                .unwrap()
                .as_ref(),
            false,
        )
        .expect("execution defers creation to durable Cook admission");

        let (_, preview) = preview;
        assert_eq!(preview["action"], "planned_create", "preview: {preview}");
        assert_eq!(preview["handle"], handle);
        assert_eq!(
            execution["action"], "lookup_pending",
            "execution: {execution}"
        );
        assert_eq!(execution["provision_intent"]["head"], "feature/no-tracker");
        assert!(execution["provision_intent"]["task_url"].is_null());
        assert!(
            homeboy::agents::agent_task_lifecycle::list_records_in_store(
                &agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().unwrap()
            )
            .expect("read lifecycle records")
            .is_empty()
        );
    });
}

#[test]
fn preview_rejects_an_existing_unsafe_path_without_changing_it() {
    crate::test_support::with_isolated_home(|_| {
        let path = tempfile::tempdir().expect("unsafe path");
        let sentinel = path.path().join("unchanged");
        std::fs::write(&sentinel, "keep").expect("write sentinel");
        let error = preview_cook(
            cook(&[
                "homeboy",
                "agent-task",
                "cook",
                "--preview",
                "--backend",
                "fixture",
                "--repo",
                "fixture-repository",
                "--prompt",
                "implement the issue",
                "--to-worktree",
                path.path().to_str().expect("UTF-8 path"),
                "--no-finalize",
            ]),
            None,
        )
        .expect_err("non-worktree path must fail static safety checks");
        assert_eq!(
            error.code,
            homeboy::core::ErrorCode::ValidationInvalidArgument
        );
        assert_eq!(
            std::fs::read_to_string(&sentinel).expect("read sentinel"),
            "keep"
        );
    });
}
