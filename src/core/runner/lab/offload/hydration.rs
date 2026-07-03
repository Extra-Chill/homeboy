//! Post-materialization dependency hydration for agent-task Lab workspaces.
//!
//! After the runner-side workspace is materialized from a filtered snapshot
//! (`vendor/`, `node_modules/`, `target/` are excluded by `default_sync_excludes`),
//! the executor starts immediately. Without hydration the offloaded agent/task
//! lands in a workspace with manifests/lockfiles but no installed dependency
//! tree, so a coding agent that fetches dependency source from outside the
//! workspace trips the tool-permission policy and the run fails (#7366).
//!
//! Hydration closes that gap by reusing the existing dependency-provider
//! detection in `core::deps` (the machinery behind `homeboy deps install`) to
//! detect the provider on the controller-side source path — whose
//! manifest/lockfile files travel in the snapshot — and running that provider's
//! install command (e.g. `composer install`, `npm ci`) in the materialized
//! runner workspace root, before the executor starts. No new provider knowledge
//! or framework-specific logic is introduced here; detection stays in `deps.rs`.

use std::collections::HashMap;

use serde::Serialize;

use crate::core::deps;
use crate::core::{Error, Result};

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
/// root), the same primitive `bootstrap_source_cli_node_dependencies` uses, so
/// the install is recorded as a runner job visible in `runner job logs`.
///
/// A non-zero install exit fails the job before the executor starts with an
/// error classified as workspace setup (distinct from a provider/agent failure).
pub(crate) fn hydrate_lab_workspace_dependencies(
    runner_id: &str,
    local_path: &str,
    remote_path: &str,
) -> Result<LabWorkspaceHydrationOutput> {
    let plan = deps::dependency_install_plan(std::path::Path::new(local_path))?;
    if plan.is_empty() {
        return Ok(LabWorkspaceHydrationOutput::skipped_no_provider(
            remote_path,
        ));
    }

    let mut steps = Vec::new();
    for plan_step in plan {
        let started = std::time::Instant::now();
        let (output, exit_code) = exec(
            runner_id,
            RunnerExecOptions {
                cwd: Some(remote_path.to_string()),
                project_id: None,
                allow_diagnostic_ssh: false,
                command: plan_step.command.clone(),
                env: HashMap::new(),
                secret_env_names: Vec::new(),
                secret_env_plan: None,
                capture_patch: false,
                // The install command is a provider-built shell argv (e.g.
                // `composer install`), not a Homeboy-routed command, so dispatch
                // it raw to the runner exactly as the provider produced it.
                raw_exec: true,
                source_snapshot: None,
                capability_preflight: None,
                required_extensions: Vec::new(),
                require_paths: Vec::new(),
                runner_workload: None,
                run_id: None,
                detach_after_handoff: false,
                // Hydration is a workspace-setup sub-step of the agent-task
                // offload, not a standalone run. The provider install runs as a
                // runner daemon job (queryable via `runner job logs <job_id>`),
                // but it is not mirrored as its own controller-side run record so
                // `runs list` stays focused on the agent-task run itself.
                mirror_evidence: false,
                print_handoff: false,
            },
        )?;
        let step = LabWorkspaceHydrationStep {
            provider_id: plan_step.provider_id.clone(),
            command: plan_step.command.clone(),
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
/// captures why hydration was skipped (non-agent-task job or explicit opt-out);
/// `Applied` carries the per-provider results.
#[derive(Debug, Clone)]
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

/// Decide whether to hydrate and, when applicable, run hydration for an
/// agent-task exec job.
///
/// Hydration runs only for job kinds that execute agent/task work in the
/// workspace (`is_agent_task_exec`), and is skipped when the operator passes
/// `--skip-deps-hydration`. Both skip cases return a `NotApplied` record without
/// touching the runner, so patch-only/read paths never hydrate.
pub(crate) fn hydrate_for_agent_task_exec(
    is_agent_task_exec: bool,
    skip_deps_hydration: bool,
    runner_id: &str,
    local_path: &str,
    remote_path: &str,
    plan: HomeboyPlan,
) -> Result<LabOffloadDependencyHydration> {
    let record = if !is_agent_task_exec {
        LabWorkspaceHydrationRecord::NotApplied {
            reason: "not_agent_task_exec",
        }
    } else if skip_deps_hydration {
        LabWorkspaceHydrationRecord::NotApplied { reason: "opt_out" }
    } else {
        LabWorkspaceHydrationRecord::Applied(hydrate_lab_workspace_dependencies(
            runner_id,
            local_path,
            remote_path,
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
            provider_id: "composer".to_string(),
            command: vec![
                "composer".to_string(),
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
        assert_eq!(error.details["provider_id"], "composer");
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

    /// The event-emission shape a hydration step records: provider id, command,
    /// duration, exit status, plus the runner job id and event count from the
    /// underlying `exec` job stream.
    #[test]
    fn hydration_step_records_event_emission_shape() {
        let step = LabWorkspaceHydrationStep {
            provider_id: "npm".to_string(),
            command: vec!["npm".to_string(), "ci".to_string()],
            duration_ms: 500,
            exit_code: 0,
            stdout: String::new(),
            stderr: String::new(),
            job_id: Some("job-7".to_string()),
            event_count: 4,
        };
        let value = serde_json::to_value(&step).expect("serialize step");
        assert_eq!(value["provider_id"], "npm");
        assert_eq!(value["command"], serde_json::json!(["npm", "ci"]));
        assert_eq!(value["duration_ms"], 500);
        assert_eq!(value["exit_code"], 0);
        assert_eq!(value["job_id"], "job-7");
        assert_eq!(value["event_count"], 4);
    }

    /// End-to-end hydration against a local runner: a detected composer
    /// provider triggers `composer install` in the materialized workspace, the
    /// fake composer records the argv it received, and hydration reports a
    /// successful step with the provider id, command, and exit status.
    #[test]
    fn hydration_runs_detected_provider_install_on_runner() {
        crate::test_support::with_isolated_home(|_| {
            let path_guard = FakeBinGuard::install(
                "composer",
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > composer-hydration-args.txt\n",
            );
            crate::core::runner::create(&path_guard.local_runner_spec("lab-local"), false)
                .expect("create local runner");

            let project = tempfile::tempdir().expect("project tempdir");
            std::fs::write(project.path().join("composer.json"), "{}").expect("composer json");
            let remote = tempfile::tempdir().expect("remote workspace");
            // The snapshot sync carries the manifest into the materialized
            // workspace; mirror that so the install runs against a real manifest.
            std::fs::write(remote.path().join("composer.json"), "{}")
                .expect("remote composer json");

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
            assert_eq!(step.provider_id, "composer");
            assert_eq!(step.exit_code, 0);
            assert_eq!(
                step.command,
                vec![
                    "composer".to_string(),
                    "install".to_string(),
                    "--no-interaction".to_string(),
                    "--no-progress".to_string(),
                ]
            );
            // The fake composer ran inside the materialized workspace root.
            assert_eq!(
                std::fs::read_to_string(remote.path().join("composer-hydration-args.txt"))
                    .expect("composer ran"),
                "install\n--no-interaction\n--no-progress\n"
            );
        });
    }

    /// A non-zero provider install exit fails hydration before the executor
    /// starts, classified as workspace setup.
    #[test]
    fn hydration_failure_fails_before_executor() {
        crate::test_support::with_isolated_home(|_| {
            let path_guard = FakeBinGuard::install(
                "composer",
                "#!/bin/sh\nprintf 'composer missing php ext' >&2\nexit 2\n",
            );
            crate::core::runner::create(&path_guard.local_runner_spec("lab-local"), false)
                .expect("create local runner");

            let project = tempfile::tempdir().expect("project tempdir");
            std::fs::write(project.path().join("composer.json"), "{}").expect("composer json");
            let remote = tempfile::tempdir().expect("remote workspace");
            std::fs::write(remote.path().join("composer.json"), "{}")
                .expect("remote composer json");

            let error = hydrate_lab_workspace_dependencies(
                "lab-local",
                &project.path().display().to_string(),
                &remote.path().display().to_string(),
            )
            .expect_err("failing install fails hydration");

            assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
            assert_eq!(error.details["classification"], "workspace_setup");
            assert_eq!(error.details["provider_id"], "composer");
            assert_eq!(error.details["exit_code"], 2);
        });
    }

    /// Hydration is skipped (`NotApplied`) for job kinds that do not execute
    /// agent/task work in the workspace, without touching the runner.
    #[test]
    fn hydration_skipped_for_non_agent_task_jobs() {
        let result = hydrate_for_agent_task_exec(
            false,
            false,
            "unused-runner",
            "/unused/local",
            "/unused/remote",
            base_lab_plan(None),
        )
        .expect("non-agent-task path does not hydrate");

        match result.record {
            LabWorkspaceHydrationRecord::NotApplied { reason } => {
                assert_eq!(reason, "not_agent_task_exec");
            }
            other => panic!("expected NotApplied, got {other:?}"),
        }
        let metadata = dependency_hydration_metadata(&result.record);
        assert_eq!(metadata["status"], "not_applied");
        assert_eq!(metadata["reason"], "not_agent_task_exec");
    }

    /// Hydration is skipped when the `--skip-deps-hydration` opt-out is set,
    /// even for an agent-task exec job.
    #[test]
    fn hydration_skipped_when_opt_out_flag_set() {
        let result = hydrate_for_agent_task_exec(
            true,
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
