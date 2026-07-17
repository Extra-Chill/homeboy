use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::Path;

use homeboy::core::engine::shell;
use homeboy::core::secret_env_plan::SecretEnvPlan;
use homeboy::core::source_snapshot::SourceSnapshot;
use homeboy::core::stream_capture::StreamCaptureMetadata;
use homeboy::core::Error;
use homeboy::runner::runners::{self as runner, RunnerExecOutput, RunnerKind};

use super::super::CmdResult;

#[allow(clippy::too_many_arguments)]
pub(super) fn exec(
    runner_id: &str,
    cwd: Option<String>,
    sync_workspace: Option<String>,
    project_id: Option<String>,
    allow_diagnostic_ssh: bool,
    capture_patch: bool,
    require_paths: Vec<String>,
    script_file: Option<String>,
    env: Vec<String>,
    secret_env: Vec<String>,
    secret_env_plan: Option<String>,
    secret_env_plan_file: Option<String>,
    dry_run: bool,
    run_id: Option<String>,
    artifact_outputs: Vec<String>,
    artifact_dir_outputs: Vec<String>,
    summary_outputs: Vec<String>,
    command: Vec<String>,
) -> CmdResult<RunnerExecOutput> {
    let script = script_file
        .as_deref()
        .map(read_runner_exec_script)
        .transpose()?;
    let prepared_command = prepare_runner_exec_command(script.as_ref(), command)?;
    let raw_env = prepare_runner_exec_env(env, script.as_deref())?;
    let secret_env_plan =
        prepare_runner_exec_secret_env_plan(secret_env, secret_env_plan, secret_env_plan_file)?;
    let secret_env_names = secret_env_plan.secret_env_names();
    validate_runner_exec_public_env(&raw_env, &secret_env_names)?;
    let mut env = secret_env_plan
        .public_env
        .clone()
        .into_iter()
        .collect::<HashMap<_, _>>();
    env.extend(raw_env);
    let required_commands = prepared_command.first().cloned().into_iter().collect();
    let has_declared_outputs = !artifact_outputs.is_empty()
        || !artifact_dir_outputs.is_empty()
        || !summary_outputs.is_empty();

    if !dry_run && has_declared_outputs && run_id.is_none() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "run_id",
            "runner exec --artifact/--artifact-dir/--summary requires --run-id so evidence can be attached to a persisted run",
            None,
            None,
        ));
    }

    if dry_run {
        let (cwd, _) = exec_workspace_context(runner_id, cwd, sync_workspace, true)?;
        return runner_exec_dry_run(
            runner_id,
            cwd,
            allow_diagnostic_ssh,
            require_paths,
            prepared_command,
            script.unwrap_or_default(),
        );
    }

    let validated_run_id = validate_runner_exec_run_id(run_id)?;
    let (cwd, source_snapshot) = exec_workspace_context(runner_id, cwd, sync_workspace, false)?;

    let (mut output, exit_code) = runner::exec(
        runner_id,
        runner::RunnerExecOptions {
            cwd,
            project_id,
            allow_diagnostic_ssh,
            diagnostic_ssh_timeout: None,
            command: prepared_command,
            env,
            secret_env_names,
            secret_env_plan: Some(secret_env_plan),
            env_materialization: None,
            capture_patch,
            raw_exec: true,
            source_snapshot,
            path_materialization_plan: None,
            capability_preflight: Some(runner::RunnerCapabilityPreflight {
                command: "runner.exec".to_string(),
                required_commands,
                ..Default::default()
            }),
            required_extensions: Vec::new(),
            accepted_extension_settings: Vec::new(),
            require_paths,
            lab_runner_workload: None,
            run_id: validated_run_id.clone(),
            detach_after_handoff: false,
            mirror_evidence: true,
            print_handoff: true,
        },
    )?;
    if let Some(run_id) = validated_run_id.as_deref() {
        let artifacts = runner::promote_runner_exec_artifacts(run_id, &output, &artifact_outputs)?;
        let promoted_artifacts = artifacts
            .iter()
            .filter_map(|record| runner::promoted_output(&output, record))
            .collect::<Vec<_>>();
        let artifact_dir_records =
            runner::promote_runner_exec_artifact_dirs(run_id, &output, &artifact_dir_outputs)?;
        let promoted_artifact_dir_records = artifact_dir_records
            .iter()
            .filter_map(|record| runner::promoted_output(&output, record))
            .collect::<Vec<_>>();
        let summaries = runner::promote_runner_exec_summaries(run_id, &output, &summary_outputs)?;
        let structured_summaries = summaries
            .iter()
            .filter_map(|summary| runner::runner_exec_structured_summary(&output, summary))
            .collect::<Vec<_>>();
        let promoted_summaries = summaries
            .iter()
            .filter_map(|record| runner::promoted_output(&output, record))
            .collect::<Vec<_>>();
        output.promoted_outputs.extend(promoted_artifacts);
        output
            .promoted_outputs
            .extend(promoted_artifact_dir_records);
        output.structured_summaries.extend(structured_summaries);
        output.promoted_outputs.extend(promoted_summaries);
    }
    Ok((output, exit_code))
}

pub(super) fn exec_workspace_context(
    runner_id: &str,
    cwd: Option<String>,
    sync_workspace: Option<String>,
    dry_run: bool,
) -> homeboy::core::Result<(Option<String>, Option<SourceSnapshot>)> {
    let Some(local_path) = sync_workspace else {
        return Ok((cwd, None));
    };

    if cwd.is_some() {
        return Err(Error::validation_invalid_argument(
            "cwd",
            "--cwd and --sync-workspace are mutually exclusive; --sync-workspace executes from the materialized runner path",
            None,
            Some(vec![
                "Use --sync-workspace <local-worktree> when the command should run from that worktree snapshot.".to_string(),
                "Use --cwd <runner-path> when the runner-side path already exists.".to_string(),
            ]),
        ));
    }

    if dry_run {
        return Ok((None, None));
    }

    let (synced, _) = runner::sync_workspace(
        runner_id,
        runner::RunnerWorkspaceSyncOptions {
            path: local_path,
            mode: runner::RunnerWorkspaceSyncMode::Snapshot,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
            run_isolation_token: None,
        },
    )?;
    let source_snapshot = SourceSnapshot::collect_local(
        runner_id,
        Path::new(&synced.local_path),
        Some(&synced.remote_path),
        synced.sync_mode.label(),
    );

    Ok((Some(synced.remote_path), Some(source_snapshot)))
}

fn validate_runner_exec_run_id(run_id: Option<String>) -> homeboy::core::Result<Option<String>> {
    let Some(run_id) = run_id else {
        return Ok(None);
    };
    let trimmed = run_id.trim();
    if trimmed.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "run_id",
            "runner exec --run-id must not be empty",
            Some(run_id),
            None,
        ));
    }
    Ok(Some(trimmed.to_string()))
}

/// Maximum number of bytes retained when reading a runner exec script into
/// memory. The script is executed verbatim, so an oversized script is rejected
/// rather than silently truncated; the cap bounds the retained bytes and the
/// truncation metadata records when the source exceeded the limit (#5238).
pub(super) const RUNNER_EXEC_SCRIPT_LIMIT_BYTES: usize = 1024 * 1024;

/// Read a stream into memory with an explicit retained-byte bound, returning the
/// retained bytes plus truncation metadata. Reads one byte past the limit so an
/// overflow is detectable without retaining the entire (potentially unbounded)
/// source.
pub(super) fn read_bounded(
    mut reader: impl Read,
    limit_bytes: usize,
) -> io::Result<(Vec<u8>, StreamCaptureMetadata)> {
    let mut retained = Vec::new();
    let read = reader
        .by_ref()
        .take((limit_bytes as u64).saturating_add(1))
        .read_to_end(&mut retained)?;
    let truncated = read > limit_bytes;
    if truncated {
        retained.truncate(limit_bytes);
    }
    let metadata = StreamCaptureMetadata {
        limit_bytes,
        seen_bytes: read,
        retained_bytes: retained.len(),
        truncated,
    };
    Ok((retained, metadata))
}

pub(super) fn read_runner_exec_script(path: &str) -> homeboy::core::Result<String> {
    let (bytes, capture) = if path == "-" {
        read_bounded(io::stdin().lock(), RUNNER_EXEC_SCRIPT_LIMIT_BYTES).map_err(|err| {
            homeboy::core::Error::internal_io(
                err.to_string(),
                Some("read runner exec script from stdin".to_string()),
            )
        })?
    } else {
        let file = fs::File::open(path).map_err(|err| {
            homeboy::core::Error::internal_io(
                err.to_string(),
                Some(format!("read runner exec script {path}")),
            )
        })?;
        read_bounded(file, RUNNER_EXEC_SCRIPT_LIMIT_BYTES).map_err(|err| {
            homeboy::core::Error::internal_io(
                err.to_string(),
                Some(format!("read runner exec script {path}")),
            )
        })?
    };

    if capture.truncated {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "script_file",
            format!(
                "runner exec script exceeds the {} byte limit (retained {} of {}+ bytes); refusing to execute a truncated script",
                capture.limit_bytes, capture.retained_bytes, capture.seen_bytes
            ),
            Some(path.to_string()),
            None,
        ));
    }

    String::from_utf8(bytes).map_err(|err| {
        homeboy::core::Error::internal_io(
            err.to_string(),
            Some(format!("decode runner exec script {path}")),
        )
    })
}

pub(super) fn prepare_runner_exec_command(
    script: Option<&String>,
    command: Vec<String>,
) -> homeboy::core::Result<Vec<String>> {
    match (script.is_some(), command.is_empty()) {
        (true, false) => Err(homeboy::core::Error::validation_invalid_argument(
            "command",
            "runner exec accepts either --script-file or a command argv, not both",
            None,
            None,
        )),
        (false, true) => Err(homeboy::core::Error::validation_invalid_argument(
            "command",
            "runner exec requires a command after -- or --script-file <path>",
            None,
            None,
        )),
        (true, true) => Ok(vec![
            "bash".to_string(),
            "-c".to_string(),
            format!(
                "printf '%s' {} | bash -s",
                shell::quote_arg(script.expect("script is present"))
            ),
        ]),
        (false, false) => Ok(command),
    }
}

pub(super) fn prepare_runner_exec_env(
    env: Vec<String>,
    _script: Option<&str>,
) -> homeboy::core::Result<HashMap<String, String>> {
    let mut values = HashMap::new();
    for assignment in env {
        let Some((key, value)) = assignment.split_once('=') else {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "env",
                "runner exec --env expects KEY=VALUE",
                Some(assignment),
                None,
            ));
        };
        if key.is_empty() || key.contains('=') || key.chars().any(|c| c.is_whitespace()) {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "env",
                "runner exec --env key must be a non-empty shell environment name",
                Some(key.to_string()),
                None,
            ));
        }
        values.insert(key.to_string(), value.to_string());
    }
    Ok(values)
}

pub(super) fn prepare_runner_exec_secret_env_plan(
    secret_env: Vec<String>,
    secret_env_plan: Option<String>,
    secret_env_plan_file: Option<String>,
) -> homeboy::core::Result<SecretEnvPlan> {
    let mut plan = SecretEnvPlan::from_secret_env_names(secret_env);

    if let Some(path) = secret_env_plan_file {
        let raw = fs::read_to_string(&path).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("read runner exec secret env plan {path}")),
            )
        })?;
        plan.merge_from(parse_runner_exec_secret_env_plan(&raw)?);
    }

    if let Some(raw) = secret_env_plan {
        plan.merge_from(parse_runner_exec_secret_env_plan(&raw)?);
    }

    Ok(plan)
}

fn parse_runner_exec_secret_env_plan(raw: &str) -> homeboy::core::Result<SecretEnvPlan> {
    serde_json::from_str(raw).map_err(|err| {
        Error::validation_invalid_argument(
            "secret_env_plan",
            format!("runner exec secret-env plan must be valid JSON: {err}"),
            None,
            None,
        )
    })
}

pub(super) fn validate_runner_exec_public_env(
    env: &HashMap<String, String>,
    secret_env_names: &[String],
) -> homeboy::core::Result<()> {
    if secret_env_names.is_empty() {
        return Ok(());
    }

    let policy = homeboy::core::redaction::RedactionPolicy::default();
    for key in env.keys() {
        if secret_env_names.iter().any(|name| name == key) && policy.is_sensitive_key(key) {
            return Err(Error::validation_invalid_argument(
                "env",
                format!(
                    "runner exec --env {key}=... would pass a declared secret-like value as public env"
                ),
                Some(key.clone()),
                Some(vec![format!(
                    "Use --secret-env {key} or include {key} in --secret-env-plan so the runner secret-env contract can resolve and redact it."
                )]),
            ));
        }
    }

    Ok(())
}

fn runner_exec_dry_run(
    runner_id: &str,
    cwd: Option<String>,
    allow_diagnostic_ssh: bool,
    require_paths: Vec<String>,
    command: Vec<String>,
    script: String,
) -> CmdResult<RunnerExecOutput> {
    let runner = runner::load(runner_id)?;
    let remote_cwd = cwd
        .or_else(|| runner.workspace_root.clone())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(display_path)
                .unwrap_or_else(|_| ".".to_string())
        });
    let mode = if runner.kind == RunnerKind::Local {
        runner::RunnerExecMode::Local
    } else if allow_diagnostic_ssh {
        runner::RunnerExecMode::DiagnosticSsh
    } else {
        runner::RunnerExecMode::Daemon
    };

    Ok((
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: runner.id,
            dry_run: true,
            mode,
            argv: command,
            remote_cwd,
            exit_code: 0,
            stdout: script,
            stderr: String::new(),
            source_snapshot: None,
            job: None,
            runner_job: None,
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: None,
            mutation_artifacts: None,
            artifacts: Vec::new(),
            promoted_outputs: Vec::new(),
            structured_summaries: Vec::new(),
            metrics: None,
            capture: None,
            execution_record: None,
            runner_result: None,
            handoff: None,
            diagnostics: Some(runner::RunnerExecDiagnostics {
                runner_workspace_root: runner.workspace_root,
                source_snapshot_remote_path: None,
                required_paths: require_paths,
                homeboy_binaries: None,
                hints: vec!["dry run only; no runner command was executed".to_string()],
            }),
        },
        0,
    ))
}

fn display_path(path: std::path::PathBuf) -> String {
    path.display().to_string()
}
