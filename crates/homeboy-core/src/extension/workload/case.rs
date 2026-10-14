//! Replay or minimize one persisted case through the kind's declared command.

use std::time::Duration;

use serde::Serialize;

use crate::engine::run_dir::RunDir;
use crate::error::{Error, Result};
use crate::extension::invoke::{build_scenario_runner, ScenarioRunnerOptions};

use super::resolve::ResolvedWorkloadKind;
use super::safety::DestructiveAuthorization;
use super::stored::StoredWorkloadRun;
use super::{env, safety};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseMode {
    Replay,
    Minimize,
}

impl CaseMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Replay => "replay",
            Self::Minimize => "minimize",
        }
    }

    fn manifest_key(&self) -> &'static str {
        match self {
            Self::Replay => "replay_command",
            Self::Minimize => "minimize_command",
        }
    }
}

pub struct CaseRequest<'a> {
    pub mode: CaseMode,
    pub resolved: &'a ResolvedWorkloadKind,
    pub source: &'a StoredWorkloadRun,
    pub case_id: &'a str,
    pub settings: &'a [(String, String)],
    pub settings_json: &'a [(String, serde_json::Value)],
    pub script_args: &'a [String],
    pub timeout: Option<Duration>,
    pub authorization: Option<&'a DestructiveAuthorization>,
    /// Resolve the command and env without executing it.
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseEnv {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseOutcome {
    pub mode: CaseMode,
    pub kind: String,
    pub source_run_id: String,
    pub case_id: String,
    /// The declared command, with any extra args appended.
    pub case_command: String,
    pub env: Vec<CaseEnv>,
    /// `dry_run`, `passed` or `failed`.
    pub status: String,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout_tail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
    /// Files a minimize command left in `HOMEBOY_WORKLOAD_ARTIFACT_DIR`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub produced_artifacts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_dir: Option<String>,
}

/// The env a case command receives: the generic case contract plus the
/// destructive gate's decision.
pub fn case_env(
    mode: CaseMode,
    source: &StoredWorkloadRun,
    case_id: &str,
    authorization: Option<&DestructiveAuthorization>,
) -> Result<Vec<(String, String)>> {
    let case = source.result.case(case_id).ok_or_else(|| {
        Error::validation_invalid_argument(
            "case_id",
            format!("run `{}` has no case `{case_id}`", source.run.id),
            Some(case_id.to_string()),
            Some(
                source
                    .result
                    .cases
                    .iter()
                    .map(|case| case.case_id.clone())
                    .collect(),
            ),
        )
    })?;
    let mut env = vec![
        (env::KIND.to_string(), source.run.kind.clone()),
        (env::CASE_MODE.to_string(), mode.as_str().to_string()),
        (env::RUN_ID.to_string(), source.run.id.clone()),
        (env::CASE_ID.to_string(), case.case_id.clone()),
        (
            env::SOURCE_RESULTS_FILE.to_string(),
            source.result_file.to_string_lossy().to_string(),
        ),
    ];
    if let Some(workload_id) = &source.result.workload_id {
        env.push((env::ID.to_string(), workload_id.clone()));
    }
    if let Some(seed) = &case.seed {
        env.push((env::SEED.to_string(), seed.clone()));
    }
    if let Some(reference) = &case.artifact_ref {
        env.push((env::CASE_ARTIFACT_REF.to_string(), reference.clone()));
        if let Some(file) = source.artifact_file(reference) {
            env.push((
                env::CASE_ARTIFACT_FILE.to_string(),
                file.to_string_lossy().to_string(),
            ));
        }
    }
    env.extend(safety::env(authorization));
    Ok(env)
}

pub fn execute(request: CaseRequest<'_>) -> Result<CaseOutcome> {
    let resolved = request.resolved;
    let declared = match request.mode {
        CaseMode::Replay => resolved.config.replay_command.as_deref(),
        CaseMode::Minimize => resolved.config.minimize_command.as_deref(),
    };
    let command = declared.map(str::to_string).ok_or_else(|| {
        Error::validation_invalid_argument(
            format!(
                "workloads.{}.{}",
                resolved.kind,
                request.mode.manifest_key()
            ),
            format!(
                "extension `{}` does not declare workloads.{}.{}; {} is unsupported for this kind",
                resolved.extension_id(),
                resolved.kind,
                request.mode.manifest_key(),
                request.mode.as_str()
            ),
            None,
            None,
        )
    })?;
    let command = if request.script_args.is_empty() {
        command
    } else {
        format!(
            "{command} {}",
            crate::engine::shell::quote_args(request.script_args)
        )
    };
    let case_env = case_env(
        request.mode,
        request.source,
        request.case_id,
        request.authorization,
    )?;
    let mut outcome = CaseOutcome {
        mode: request.mode,
        kind: resolved.kind.clone(),
        source_run_id: request.source.run.id.clone(),
        case_id: request.case_id.to_string(),
        case_command: command.clone(),
        env: case_env
            .iter()
            .map(|(name, value)| CaseEnv {
                name: name.clone(),
                value: value.clone(),
            })
            .collect(),
        status: "dry_run".to_string(),
        exit_code: 0,
        stdout_tail: None,
        stderr_tail: None,
        produced_artifacts: Vec::new(),
        artifact_dir: None,
    };
    if request.dry_run {
        return Ok(outcome);
    }

    let run_dir = RunDir::create()?;
    let artifact_dir = run_dir.step_file("workload-artifacts");
    std::fs::create_dir_all(&artifact_dir).map_err(|error| {
        Error::internal_io(error.to_string(), Some(artifact_dir.display().to_string()))
    })?;
    let mut runner = build_scenario_runner(ScenarioRunnerOptions {
        execution_context: &resolved.extension,
        component: &resolved.context.component,
        path_override: Some(resolved.context.source_path.to_string_lossy().to_string()),
        settings: request.settings,
        settings_json: request.settings_json,
        run_dir: &run_dir,
        results_env: None,
        scenario_env: None,
        artifact_env: Some((env::ARTIFACT_DIR, &artifact_dir)),
        list_only_env: None,
        extra_workloads_env: None,
        env_provider_extensions: &[],
        invocation_requirements: Default::default(),
    })?
    .command_override(command)
    .timeout(request.timeout)
    .passthrough(false);
    for (key, value) in &case_env {
        runner = runner.env(key, value);
    }
    let output = runner.run()?;
    outcome.exit_code = if output.success {
        0
    } else if output.exit_code != 0 {
        output.exit_code
    } else {
        1
    };
    outcome.status = if output.success { "passed" } else { "failed" }.to_string();
    outcome.stdout_tail = Some(super::run::tail(&output.stdout));
    outcome.stderr_tail = Some(super::run::tail(&output.stderr));
    outcome.produced_artifacts = std::fs::read_dir(&artifact_dir)
        .map(|entries| {
            let mut names = entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().to_string())
                .collect::<Vec<_>>();
            names.sort();
            names
        })
        .unwrap_or_default();
    if !outcome.produced_artifacts.is_empty() {
        // Keep what the command produced for the operator to collect.
        outcome.artifact_dir = Some(artifact_dir.to_string_lossy().to_string());
        run_dir.finish(false);
    } else {
        run_dir.finish(output.success);
    }
    Ok(outcome)
}
