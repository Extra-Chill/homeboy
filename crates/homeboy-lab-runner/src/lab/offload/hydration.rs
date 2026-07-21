//! Post-materialization dependency hydration for Lab workspaces.
//!
//! After the runner-side workspace is materialized from a filtered snapshot
//! (`vendor/`, `node_modules/`, `target/` are excluded by `default_sync_excludes`),
//! the command starts immediately. Without hydration the offloaded command lands
//! in a workspace with manifests/lockfiles but no installed dependency tree, so
//! workloads that need package dependencies fail before their own checks can run
//! (#7366).
//!
//! Hydration closes that gap by reusing the existing dependency-provider
//! detection in `core::deps` (the machinery behind `homeboy deps install`) to
//! detect the provider on the controller-side source path — whose
//! manifest/lockfile files travel in the snapshot — and running that provider's
//! install command in the materialized
//! runner workspace root, before the executor starts. No new provider knowledge
//! or framework-specific logic is introduced here; detection stays in `deps.rs`.

use serde::Serialize;

use homeboy_core::deps;
use homeboy_core::engine::shell;
use homeboy_core::{Error, Result};

use super::*;

pub(crate) const HYDRATION_SCHEMA: &str = "homeboy/lab-workspace-dependency-hydration/v1";

/// Per-provider result of hydrating a materialized runner workspace. Mirrors the
/// shape operators read from `homeboy runner job logs`: the provider that ran,
/// the install command, how long it took, and its exit status.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct LabWorkspaceHydrationStep {
    pub(crate) provider_id: String,
    pub(crate) command: Vec<String>,
    pub(crate) duration_ms: u64,
    pub(crate) exit_code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) job_id: Option<String>,
    /// Number of job events the hydration exec emitted into the runner job
    /// stream (`homeboy runner job logs`).
    pub(crate) event_count: usize,
}

/// Aggregate hydration outcome for a single materialized workspace.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LabWorkspaceHydrationOutput {
    pub(crate) schema: &'static str,
    /// `hydrated` when at least one provider install ran, `skipped_no_provider`
    /// when the workspace exposed no detected dependency provider.
    pub(crate) status: &'static str,
    pub(crate) workspace: String,
    pub(crate) steps: Vec<LabWorkspaceHydrationStep>,
}

impl LabWorkspaceHydrationOutput {
    pub(crate) fn skipped_no_provider(workspace: &str) -> Self {
        Self {
            schema: HYDRATION_SCHEMA,
            status: "skipped_no_provider",
            workspace: workspace.to_string(),
            steps: Vec::new(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// Detect dependency providers for the controller-side workspace and run each
/// provider's install command in the materialized runner workspace root.
///
/// Detection runs against `local_path` (the controller-side source checkout):
/// the manifest/lockfile files are part of the synced snapshot, so they expose
/// the same providers the materialized runner workspace does. Execution runs via
/// the existing runner `exec` path at `remote_path` (the materialized workspace
/// root), the same primitive source-CLI dependency bootstrap uses, so
/// the install is recorded as a runner job visible in `runner job logs`.
///
/// A non-zero install exit fails the job before the executor starts with an
/// error classified as workspace setup (distinct from a provider/agent failure).
pub(crate) fn hydrate_lab_workspace_dependencies(
    runner_id: &str,
    local_path: &str,
    remote_path: &str,
) -> Result<LabWorkspaceHydrationOutput> {
    hydrate_lab_workspace_dependencies_for_run(runner_id, local_path, remote_path, None)
}

const HYDRATION_RUN_SEGMENT: &str = "-lab-hydration-";

pub(crate) fn hydration_execution_run_id(parent_run_id: &str, index: usize) -> String {
    format!("{parent_run_id}{HYDRATION_RUN_SEGMENT}{index}")
}

pub(crate) fn is_hydration_execution_run_id(parent_run_id: &str, candidate: &str) -> bool {
    candidate
        .strip_prefix(parent_run_id)
        .is_some_and(|suffix| suffix.starts_with(HYDRATION_RUN_SEGMENT))
}

fn hydration_runner_exec_options(
    command: Vec<String>,
    remote_path: &str,
    parent_run_id: Option<&str>,
    index: usize,
) -> RunnerExecOptions {
    let mut options = RunnerExecOptions::raw_command(command)
        .with_cwd(remote_path)
        .without_evidence_mirror();
    options.run_id = parent_run_id.map(|run_id| hydration_execution_run_id(run_id, index));
    // Hydration children are generic runner executions, not agent-task handoffs.
    options.run_id_owns_generic_exec = options.run_id.is_some();
    options
}

fn hydrate_lab_workspace_dependencies_for_run(
    runner_id: &str,
    local_path: &str,
    remote_path: &str,
    parent_run_id: Option<&str>,
) -> Result<LabWorkspaceHydrationOutput> {
    let plan = deps::dependency_install_plan(std::path::Path::new(local_path))?;
    if plan.is_empty() {
        return Ok(LabWorkspaceHydrationOutput::skipped_no_provider(
            remote_path,
        ));
    }

    let mut steps = Vec::new();
    for (index, plan_step) in plan.into_iter().enumerate() {
        let command = runner_hydration_command(&plan_step.invocation)?;
        let started = std::time::Instant::now();
        let options =
            hydration_runner_exec_options(command.clone(), remote_path, parent_run_id, index);
        let (output, exit_code) = exec(
            runner_id,
            // The install command is provider-built shell argv, not a
            // Homeboy-routed command, so dispatch it raw exactly as produced.
            // Hydration is a workspace-setup sub-step of the agent-task offload,
            // so do not mirror it as a standalone controller-side run record.
            options,
        )?;
        let step = LabWorkspaceHydrationStep {
            provider_id: plan_step.provider_id.clone(),
            command,
            duration_ms: started.elapsed().as_millis() as u64,
            exit_code,
            stdout: output.stdout,
            stderr: output.stderr,
            job_id: output.job_id.clone(),
            event_count: output.job_events.map(|events| events.len()).unwrap_or(0),
        };
        if exit_code != 0 {
            return Err(hydration_failure_error(remote_path, &step));
        }
        steps.push(step);
    }

    Ok(LabWorkspaceHydrationOutput {
        schema: HYDRATION_SCHEMA,
        status: "hydrated",
        workspace: remote_path.to_string(),
        steps,
    })
}

/// Convert the controller-created declaration into runner-owned argv. Extension
/// entrypoints are deliberately resolved by the runner shell from its HOME,
/// never from the controller's absolute extension installation path.
fn runner_hydration_command(invocation: &deps::DependencyInstallInvocation) -> Result<Vec<String>> {
    match invocation {
        deps::DependencyInstallInvocation::Argv { argv } => Ok(argv.clone()),
        deps::DependencyInstallInvocation::ExtensionEntrypoint {
            extension_id,
            entrypoint,
            argv,
            entrypoint_index,
        } => {
            if *entrypoint_index > argv.len() {
                return Err(Error::validation_invalid_argument(
                    "lab_workspace_dependency_hydration",
                    "extension dependency invocation has an invalid entrypoint position",
                    None,
                    None,
                ));
            }
            if std::path::Path::new(entrypoint)
                .components()
                .any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir | std::path::Component::RootDir
                    )
                })
            {
                return Err(Error::validation_invalid_argument(
                    "lab_workspace_dependency_hydration",
                    "extension dependency entrypoint must be a relative path inside its extension",
                    Some(entrypoint.clone()),
                    None,
                ));
            }
            let resolved_argv = argv
                .iter()
                .enumerate()
                .flat_map(|(index, value)| {
                    let mut value = vec![shell::quote_arg(value)];
                    if index == *entrypoint_index {
                        value.insert(0, "\"$entrypoint\"".to_string());
                    }
                    value
                })
                .chain((*entrypoint_index == argv.len()).then(|| "\"$entrypoint\"".to_string()))
                .collect::<Vec<_>>()
                .join(" ");
            let command = format!(
                "extension_root=\"${{HOME}}/.config/homeboy/extensions/\"{}; entrypoint=\"$extension_root\"/{}; if test ! -f \"$entrypoint\"; then printf '%s\\n' {} >&2; exit 127; fi; exec {}",
                shell::quote_arg(extension_id),
                shell::quote_arg(entrypoint),
                shell::quote_arg(&format!(
                    "Lab dependency hydration requires runner-local extension `{extension_id}` entrypoint `{entrypoint}`. Install or refresh that extension on the runner."
                )),
                resolved_argv,
            );
            Ok(vec!["sh".to_string(), "-c".to_string(), command])
        }
    }
}

/// Build the pre-executor failure for a hydration step that exited non-zero.
///
/// Classified as workspace setup (`classification = "workspace_setup"`) so it is
/// distinct from a provider/agent execution failure: the workspace was not ready
/// for the agent to run in, which is an actionable setup defect rather than a
/// defect in the agent task itself.
pub(crate) fn hydration_failure_error(
    remote_path: &str,
    step: &LabWorkspaceHydrationStep,
) -> Error {
    let mut error = Error::validation_invalid_argument(
        "lab_workspace_dependency_hydration",
        format!(
            "Lab workspace dependency hydration failed: provider `{}` command `{}` exited {} in `{}`",
            step.provider_id,
            step.command.join(" "),
            step.exit_code,
            remote_path,
        ),
        Some(remote_path.to_string()),
        Some(vec![
            format!("provider: {}", step.provider_id),
            format!("command: {}", step.command.join(" ")),
            format!("exit_code: {}", step.exit_code),
            if step.stderr.trim().is_empty() && step.stdout.trim().is_empty() {
                "The provider install produced no output.".to_string()
            } else {
                let tail: String = step.stderr.trim().chars().take(400).collect();
                format!("stderr: {tail}")
            },
            "Reproduce locally by running the provider install in the source checkout, then retry Lab offload.".to_string(),
            "Pass --skip-deps-hydration only if the workspace is already hydrated or the job does not need installed dependencies.".to_string(),
        ]),
    );
    error.details["classification"] = serde_json::json!("workspace_setup");
    error.details["schema"] = serde_json::json!(HYDRATION_SCHEMA);
    error.details["workspace"] = serde_json::json!(remote_path);
    error.details["provider_id"] = serde_json::json!(step.provider_id);
    error.details["command"] = serde_json::json!(step.command);
    error.details["exit_code"] = serde_json::json!(step.exit_code);
    error.details["duration_ms"] = serde_json::json!(step.duration_ms);
    error.details["job_id"] = serde_json::json!(step.job_id);
    error
}

/// Hydration outcome as recorded by the offload orchestrator. `NotApplied`
/// captures why hydration was skipped (explicit opt-out); `Applied` carries the
/// per-provider results.
#[derive(Debug, Clone, Serialize)]
pub(crate) enum LabWorkspaceHydrationRecord {
    NotApplied { reason: &'static str },
    Applied(LabWorkspaceHydrationOutput),
}

/// Result of the offload hydration step: the updated plan (with a
/// `lab.hydrate_dependencies` step when hydration ran) and the hydration record.
pub(crate) struct LabOffloadDependencyHydration {
    pub(crate) plan: HomeboyPlan,
    pub(crate) record: LabWorkspaceHydrationRecord,
}

/// Hydration plus the runner execution identities durably recorded against an
/// agent-task attempt. Both synchronous offload and controller staging use this
/// boundary so recovery observes the same lifecycle authority.
pub(crate) struct RecordedLabDependencyHydration {
    pub(crate) hydration: LabOffloadDependencyHydration,
    pub(crate) execution_ids: Vec<String>,
}

pub(crate) fn hydrate_for_lab_workspace_exec_with_lifecycle(
    skip_deps_hydration: bool,
    runner_id: &str,
    local_path: &str,
    remote_path: &str,
    plan: HomeboyPlan,
    agent_task_run_id: Option<&str>,
) -> Result<RecordedLabDependencyHydration> {
    let hydration = hydrate_for_lab_workspace_exec_internal(
        skip_deps_hydration,
        runner_id,
        local_path,
        remote_path,
        plan,
        agent_task_run_id,
    )?;
    let execution_ids = match &hydration.record {
        LabWorkspaceHydrationRecord::Applied(output) => output
            .steps
            .iter()
            .filter_map(|step| step.job_id.clone())
            .collect(),
        LabWorkspaceHydrationRecord::NotApplied { .. } => Vec::new(),
    };
    if let Some(run_id) = agent_task_run_id {
        agent_task_lifecycle::record_lab_offload_phase_executions(
            run_id,
            "hydrating",
            execution_ids.clone(),
        )?;
    }
    Ok(RecordedLabDependencyHydration {
        hydration,
        execution_ids,
    })
}

/// Decide whether to hydrate and, when applicable, run hydration for a
/// materialized Lab workspace command.
///
/// Hydration is skipped when the operator passes `--skip-deps-hydration`.
/// Runner-resident commands do not call this path because they do not
/// materialize a source workspace.
pub(crate) fn hydrate_for_lab_workspace_exec(
    skip_deps_hydration: bool,
    runner_id: &str,
    local_path: &str,
    remote_path: &str,
    plan: HomeboyPlan,
) -> Result<LabOffloadDependencyHydration> {
    hydrate_for_lab_workspace_exec_internal(
        skip_deps_hydration,
        runner_id,
        local_path,
        remote_path,
        plan,
        None,
    )
}

fn hydrate_for_lab_workspace_exec_internal(
    skip_deps_hydration: bool,
    runner_id: &str,
    local_path: &str,
    remote_path: &str,
    plan: HomeboyPlan,
    agent_task_run_id: Option<&str>,
) -> Result<LabOffloadDependencyHydration> {
    let record = if skip_deps_hydration {
        LabWorkspaceHydrationRecord::NotApplied { reason: "opt_out" }
    } else {
        LabWorkspaceHydrationRecord::Applied(hydrate_lab_workspace_dependencies_for_run(
            runner_id,
            local_path,
            remote_path,
            agent_task_run_id,
        )?)
    };

    let plan = match &record {
        LabWorkspaceHydrationRecord::Applied(output) if !output.is_empty() => with_step(
            plan,
            PlanStep::ready("lab.hydrate_dependencies", "lab.hydrate_dependencies")
                .inputs(
                    PlanValues::new()
                        .string("workspace", &output.workspace)
                        .string("status", output.status)
                        .json("steps", &output.steps),
                )
                .build(),
        ),
        _ => plan,
    };

    Ok(LabOffloadDependencyHydration { plan, record })
}

/// Render the hydration record as offload metadata (`lab_metadata["dependency_hydration"]`).
pub(crate) fn dependency_hydration_metadata(
    record: &LabWorkspaceHydrationRecord,
) -> serde_json::Value {
    match record {
        LabWorkspaceHydrationRecord::NotApplied { reason } => serde_json::json!({
            "schema": HYDRATION_SCHEMA,
            "status": "not_applied",
            "reason": reason,
        }),
        LabWorkspaceHydrationRecord::Applied(output) => serde_json::to_value(output)
            .unwrap_or_else(|_| {
                serde_json::json!({
                    "schema": HYDRATION_SCHEMA,
                    "status": output.status,
                    "workspace": output.workspace,
                })
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hydration_execution_ids_are_parent_scoped_and_discoverable() {
        let child = hydration_execution_run_id("attempt-1", 2);
        assert_eq!(child, "attempt-1-lab-hydration-2");
        assert!(is_hydration_execution_run_id("attempt-1", &child));
        assert!(!is_hydration_execution_run_id("attempt-2", &child));
        assert!(!is_hydration_execution_run_id("attempt-1", "attempt-1"));
    }

    #[test]
    fn hydration_execution_ids_own_generic_runner_exec_lifecycles() {
        let options = hydration_runner_exec_options(
            vec!["fixture-tool".to_string(), "install".to_string()],
            "/runner/workspace",
            Some("attempt-1"),
            2,
        );

        assert_eq!(options.run_id.as_deref(), Some("attempt-1-lab-hydration-2"));
        assert!(options.run_id_owns_generic_exec);
        assert!(
            !hydration_runner_exec_options(
                vec!["fixture-tool".to_string()],
                "/runner/workspace",
                None,
                0,
            )
            .run_id_owns_generic_exec
        );
    }

    /// Holds a temp bin dir containing a fake executable. The runner exec path
    /// uses a curated `PATH` (`local_runner_command_path`), not the process
    /// `PATH`, so tests must register the runner with `bin_dir` on its own
    /// `env.PATH` (see [`FakeBinGuard::local_runner_spec`]). The process `PATH`
    /// is intentionally NOT mutated so the fake cannot interfere with parallel
    /// tests that spawn subprocesses.
    struct FakeBinGuard {
        _bin: tempfile::TempDir,
    }

    impl FakeBinGuard {
        /// Create `name` as a fake executable in a temp bin dir.
        fn install(name: &str, script: &str) -> Self {
            let bin = tempfile::tempdir().expect("bin tempdir");
            let exe = bin.path().join(name);
            std::fs::write(&exe, script).expect("fake bin");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut mode = std::fs::metadata(&exe).expect("metadata").permissions();
                mode.set_mode(0o755);
                std::fs::set_permissions(&exe, mode).expect("chmod fake bin");
            }
            Self { _bin: bin }
        }

        fn bin_dir(&self) -> String {
            self._bin.path().display().to_string()
        }

        /// Build a local-runner JSON spec whose `env.PATH` exposes the fake bin
        /// (plus the inherited PATH) so the runner exec path resolves the fake
        /// executable instead of any system binary.
        fn local_runner_spec(&self, id: &str) -> String {
            let path_env = format!(
                "{}:{}",
                self.bin_dir(),
                std::env::var("PATH").unwrap_or_default()
            );
            serde_json::json!({
                "id": id,
                "kind": "local",
                "env": {"PATH": path_env}
            })
            .to_string()
        }
    }

    /// `hydration_failure_error` classifies the failure as workspace setup and
    /// records the provider id, command, duration, and exit status so operators
    /// can distinguish a hydration (workspace setup) failure from a later
    /// provider/agent execution failure (#7366).
    #[test]
    fn hydration_failure_classifies_as_workspace_setup() {
        let step = LabWorkspaceHydrationStep {
            provider_id: "declared-provider".to_string(),
            command: vec![
                "fixture-tool".to_string(),
                "install".to_string(),
                "--no-interaction".to_string(),
            ],
            duration_ms: 1234,
            exit_code: 2,
            stdout: String::new(),
            stderr: "missing ext-dom".to_string(),
            job_id: Some("job-1".to_string()),
            event_count: 3,
        };

        let error = hydration_failure_error("/runner/workspaces/primary", &step);

        assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
        assert_eq!(error.details["classification"], "workspace_setup");
        assert_eq!(error.details["provider_id"], "declared-provider");
        assert_eq!(error.details["exit_code"], 2);
        assert_eq!(error.details["duration_ms"], 1234);
        assert_eq!(error.details["job_id"], "job-1");
        assert!(error.message.contains("lab_workspace_dependency_hydration"));
    }

    /// Hydration is a no-op (skipped_no_provider) when no provider detects the
    /// workspace, mirroring `deps::install_for_resolved`'s dependency-less
    /// component policy.
    #[test]
    fn hydration_output_reports_skipped_when_no_provider() {
        let output = LabWorkspaceHydrationOutput::skipped_no_provider("/runner/ws");
        assert_eq!(output.status, "skipped_no_provider");
        assert!(output.is_empty());
        assert_eq!(output.schema, HYDRATION_SCHEMA);
    }

    #[test]
    fn explicit_skip_is_a_completed_no_op_without_runner_access() {
        let hydration = hydrate_for_lab_workspace_exec(
            true,
            "unreachable-runner",
            "/missing/controller-workspace",
            "/missing/runner-workspace",
            HomeboyPlan::for_description(homeboy_core::plan::PlanKind::LabOffload, "hydration"),
        )
        .expect("skip does not detect or execute dependencies");

        assert!(matches!(
            hydration.record,
            LabWorkspaceHydrationRecord::NotApplied { reason: "opt_out" }
        ));
        assert!(hydration.plan.steps.is_empty());
        assert_eq!(
            dependency_hydration_metadata(&hydration.record)["status"],
            "not_applied"
        );
    }

    #[test]
    fn hydration_skips_when_linked_extensions_do_not_provide_dependencies() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/fixture");
            std::fs::create_dir_all(&extension_dir).expect("extension directory");
            std::fs::write(
                extension_dir.join("fixture.json"),
                r#"{"name":"Fixture","version":"1.0.0","test":{"extension_script":"test.sh"}}"#,
            )
            .expect("extension manifest");

            let project = tempfile::tempdir().expect("project tempdir");
            std::fs::write(
                project.path().join("homeboy.json"),
                r#"{"id":"fixture","extensions":{"fixture":{}}}"#,
            )
            .expect("component config");
            let remote = tempfile::tempdir().expect("remote workspace");

            let output = hydrate_lab_workspace_dependencies(
                "unused-runner",
                &project.path().display().to_string(),
                &remote.path().display().to_string(),
            )
            .expect("no applicable dependency provider skips hydration");

            assert_eq!(output.status, "skipped_no_provider");
            assert!(output.steps.is_empty());
        });
    }

    /// The event-emission shape a hydration step records: provider id, command,
    /// duration, exit status, plus the runner job id and event count from the
    /// underlying `exec` job stream.
    #[test]
    fn hydration_step_records_event_emission_shape() {
        let step = LabWorkspaceHydrationStep {
            provider_id: "declared-provider".to_string(),
            command: vec!["fixture-tool".to_string(), "install".to_string()],
            duration_ms: 500,
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            job_id: Some("job-7".to_string()),
            event_count: 4,
        };
        let value = serde_json::to_value(&step).expect("serialize step");
        assert_eq!(value["provider_id"], "declared-provider");
        assert_eq!(
            value["command"],
            serde_json::json!(["fixture-tool", "install"])
        );
        assert_eq!(value["duration_ms"], 500);
        assert_eq!(value["exit_code"], 0);
        assert_eq!(value["job_id"], "job-7");
        assert_eq!(value["event_count"], 4);
    }

    /// End-to-end hydration against a local runner: a declared provider command
    /// runs in the materialized workspace and hydration reports provider id,
    /// command, and exit status.
    #[test]
    fn hydration_runs_detected_provider_install_on_runner() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let path_guard = FakeBinGuard::install(
                "fixture-tool",
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > hydration-args.txt\n",
            );
            crate::create(&path_guard.local_runner_spec("lab-local"), false)
                .expect("create local runner");

            let project = tempfile::tempdir().expect("project tempdir");
            let manifest = r#"{
                "provider": "declared-provider",
                "commands": { "install": { "argv": ["fixture-tool", "install", "--no-interaction", "--no-progress"] } }
            }"#;
            std::fs::write(project.path().join("homeboy-deps.json"), manifest)
                .expect("provider manifest");
            let remote = tempfile::tempdir().expect("remote workspace");
            std::fs::write(remote.path().join("homeboy-deps.json"), manifest)
                .expect("remote provider manifest");

            let output = hydrate_lab_workspace_dependencies(
                "lab-local",
                &project.path().display().to_string(),
                &remote.path().display().to_string(),
            )
            .expect("hydration succeeds");

            assert_eq!(output.status, "hydrated");
            assert_eq!(output.workspace, remote.path().display().to_string());
            assert_eq!(output.steps.len(), 1);
            let step = &output.steps[0];
            assert_eq!(step.provider_id, "declared-provider");
            assert_eq!(step.exit_code, 0);
            assert_eq!(
                step.command,
                vec![
                    "fixture-tool".to_string(),
                    "install".to_string(),
                    "--no-interaction".to_string(),
                    "--no-progress".to_string(),
                ]
            );
            // The fake provider command ran inside the materialized workspace root.
            assert_eq!(
                std::fs::read_to_string(remote.path().join("hydration-args.txt"))
                    .expect("provider command ran"),
                "install\n--no-interaction\n--no-progress\n"
            );
        });
    }

    /// A non-zero provider install exit fails hydration before the executor
    /// starts, classified as workspace setup.
    #[test]
    fn hydration_failure_fails_before_executor() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let path_guard = FakeBinGuard::install(
                "fixture-tool",
                "#!/bin/sh\nprintf 'fixture failure' >&2\nexit 2\n",
            );
            crate::create(&path_guard.local_runner_spec("lab-local"), false)
                .expect("create local runner");

            let project = tempfile::tempdir().expect("project tempdir");
            let manifest = r#"{
                "provider": "declared-provider",
                "commands": { "install": { "argv": ["fixture-tool", "install"] } }
            }"#;
            std::fs::write(project.path().join("homeboy-deps.json"), manifest)
                .expect("provider manifest");
            let remote = tempfile::tempdir().expect("remote workspace");
            std::fs::write(remote.path().join("homeboy-deps.json"), manifest)
                .expect("remote provider manifest");

            let error = hydrate_lab_workspace_dependencies(
                "lab-local",
                &project.path().display().to_string(),
                &remote.path().display().to_string(),
            )
            .expect_err("failing install fails hydration");

            assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
            assert_eq!(error.details["classification"], "workspace_setup");
            assert_eq!(error.details["provider_id"], "declared-provider");
            assert_eq!(error.details["exit_code"], 2);
        });
    }

    /// Hydration runs for materialized workspace commands, including non-agent
    /// workloads such as bench/rig offloads.
    #[test]
    fn hydration_runs_for_materialized_workspace_jobs() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let path_guard = FakeBinGuard::install(
                "fixture-tool",
                "#!/bin/sh\nprintf '%s\\n' hydrated > workspace-hydrated.txt\n",
            );
            crate::create(&path_guard.local_runner_spec("lab-local"), false)
                .expect("create local runner");

            let project = tempfile::tempdir().expect("project tempdir");
            let manifest = r#"{
                "provider": "declared-provider",
                "commands": { "install": { "argv": ["fixture-tool", "install"] } }
            }"#;
            std::fs::write(project.path().join("homeboy-deps.json"), manifest)
                .expect("provider manifest");
            let remote = tempfile::tempdir().expect("remote workspace");
            std::fs::write(remote.path().join("homeboy-deps.json"), manifest)
                .expect("remote provider manifest");

            let result = hydrate_for_lab_workspace_exec(
                false,
                "lab-local",
                &project.path().display().to_string(),
                &remote.path().display().to_string(),
                base_lab_plan(None),
            )
            .expect("workspace command hydrates");

            match result.record {
                LabWorkspaceHydrationRecord::Applied(output) => {
                    assert_eq!(output.status, "hydrated");
                    assert_eq!(output.steps.len(), 1);
                }
                other => panic!("expected Applied, got {other:?}"),
            }
            assert_eq!(
                std::fs::read_to_string(remote.path().join("workspace-hydrated.txt"))
                    .expect("hydration marker"),
                "hydrated\n"
            );
        });
    }

    #[test]
    fn hydration_resolves_extension_entrypoint_from_runner_home() {
        homeboy_core::test_support::with_isolated_home(|controller_home| {
            let runner_home = tempfile::tempdir().expect("runner home");
            let extension_id = "fixture-runtime";
            let entrypoint = "scripts/install.sh";
            let runner_script = runner_home
                .path()
                .join(".config/homeboy/extensions")
                .join(extension_id)
                .join(entrypoint);
            std::fs::create_dir_all(runner_script.parent().expect("script parent"))
                .expect("runner extension directory");
            std::fs::write(
                &runner_script,
                "#!/bin/sh\nprintf '%s\\n' \"$1\" > hydrated-by-runner-extension.txt\n",
            )
            .expect("runner extension script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut mode = std::fs::metadata(&runner_script)
                    .expect("script metadata")
                    .permissions();
                mode.set_mode(0o755);
                std::fs::set_permissions(&runner_script, mode).expect("chmod script");
            }
            crate::create(
                &serde_json::json!({
                    "id": "lab-local",
                    "kind": "local",
                    "env": { "HOME": runner_home.path() },
                })
                .to_string(),
                false,
            )
            .expect("create local runner");
            let workspace = tempfile::tempdir().expect("workspace");
            let invocation = deps::DependencyInstallInvocation::ExtensionEntrypoint {
                extension_id: extension_id.to_string(),
                entrypoint: entrypoint.to_string(),
                argv: vec!["sh".to_string(), "install".to_string()],
                entrypoint_index: 1,
            };

            let command = runner_hydration_command(&invocation).expect("runner command");
            assert!(!command
                .join(" ")
                .contains(&controller_home.path().display().to_string()));
            let (output, exit_code) = exec(
                "lab-local",
                RunnerExecOptions::raw_command(command)
                    .with_cwd(&workspace.path().display().to_string())
                    .without_evidence_mirror(),
            )
            .expect("runner executes extension entrypoint");

            assert_eq!(exit_code, 0, "{}", output.stderr);
            assert_eq!(
                std::fs::read_to_string(workspace.path().join("hydrated-by-runner-extension.txt"))
                    .expect("runner extension marker"),
                "install\n"
            );
        });
    }

    #[test]
    fn hydration_missing_runner_extension_entrypoint_fails_actionably() {
        homeboy_core::test_support::with_isolated_home(|_controller_home| {
            let runner_home = tempfile::tempdir().expect("runner home");
            crate::create(
                &serde_json::json!({
                    "id": "lab-local",
                    "kind": "local",
                    "env": { "HOME": runner_home.path() },
                })
                .to_string(),
                false,
            )
            .expect("create local runner");
            let workspace = tempfile::tempdir().expect("workspace");
            let invocation = deps::DependencyInstallInvocation::ExtensionEntrypoint {
                extension_id: "missing-runtime".to_string(),
                entrypoint: "scripts/install.sh".to_string(),
                argv: vec!["sh".to_string(), "install".to_string()],
                entrypoint_index: 1,
            };
            let command = runner_hydration_command(&invocation).expect("runner command");
            let (output, exit_code) = exec(
                "lab-local",
                RunnerExecOptions::raw_command(command)
                    .with_cwd(&workspace.path().display().to_string())
                    .without_evidence_mirror(),
            )
            .expect("runner executes command");

            assert_eq!(exit_code, 127);
            assert!(output
                .stderr
                .contains("runner-local extension `missing-runtime`"));
            assert!(output
                .stderr
                .contains("Install or refresh that extension on the runner"));
        });
    }

    /// Hydration is skipped when the `--skip-deps-hydration` opt-out is set,
    /// even for a materialized workspace job.
    #[test]
    fn hydration_skipped_when_opt_out_flag_set() {
        let result = hydrate_for_lab_workspace_exec(
            true,
            "unused-runner",
            "/unused/local",
            "/unused/remote",
            base_lab_plan(None),
        )
        .expect("opt-out path does not hydrate");

        match result.record {
            LabWorkspaceHydrationRecord::NotApplied { reason } => {
                assert_eq!(reason, "opt_out");
            }
            other => panic!("expected NotApplied, got {other:?}"),
        }
        let metadata = dependency_hydration_metadata(&result.record);
        assert_eq!(metadata["status"], "not_applied");
        assert_eq!(metadata["reason"], "opt_out");
    }
}
