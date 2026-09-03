use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::time::Duration;

use homeboy::core::engine::shell;
use homeboy::core::secret_env_plan::SecretEnvPlan;
use homeboy::core::stream_capture::StreamCaptureMetadata;
use homeboy::core::Error;
use homeboy::runner::runners::{self as runner, RunnerExecOutput};
use homeboy_engine_primitives::content_hash;
use homeboy_runner_contract::RunnerKind;

use super::super::CmdResult;

#[derive(Debug, Clone)]
pub(super) struct RunnerExecInput {
    pub runner_id: String,
    pub command: Vec<String>,
    pub cwd: Option<String>,
    pub sync_workspace: Option<String>,
    pub workspace_ref: Option<String>,
    pub hydrate_deps: bool,
    pub workspace_sync_timeout: Duration,
    pub project_id: Option<String>,
    pub allow_diagnostic_ssh: bool,
    pub capture_patch: bool,
    pub require_paths: Vec<String>,
    pub script_file: Option<String>,
    pub env: Vec<String>,
    pub secret_env: Vec<String>,
    pub secret_env_plan: Option<String>,
    pub secret_env_plan_file: Option<String>,
    pub dry_run: bool,
    pub run_id: Option<String>,
    pub artifact_outputs: Vec<String>,
    pub artifact_dir_outputs: Vec<String>,
    pub summary_outputs: Vec<String>,
    pub read_only_artifact: bool,
    pub raw: bool,
    pub extension_env_providers: Vec<String>,
}

impl RunnerExecInput {
    pub(super) fn new(runner_id: impl Into<String>, command: Vec<String>) -> Self {
        Self {
            runner_id: runner_id.into(),
            command,
            cwd: None,
            sync_workspace: None,
            workspace_ref: None,
            hydrate_deps: false,
            workspace_sync_timeout: Duration::from_secs(240),
            project_id: None,
            allow_diagnostic_ssh: false,
            capture_patch: false,
            require_paths: Vec::new(),
            script_file: None,
            env: Vec::new(),
            secret_env: Vec::new(),
            secret_env_plan: None,
            secret_env_plan_file: None,
            dry_run: false,
            run_id: None,
            artifact_outputs: Vec::new(),
            artifact_dir_outputs: Vec::new(),
            summary_outputs: Vec::new(),
            read_only_artifact: false,
            raw: false,
            extension_env_providers: Vec::new(),
        }
    }
}

pub(super) fn execute(input: RunnerExecInput) -> CmdResult<RunnerExecOutput> {
    let RunnerExecInput {
        runner_id,
        command,
        cwd,
        sync_workspace,
        workspace_ref,
        hydrate_deps,
        workspace_sync_timeout,
        project_id,
        allow_diagnostic_ssh,
        capture_patch,
        require_paths,
        script_file,
        env,
        secret_env,
        secret_env_plan,
        secret_env_plan_file,
        dry_run,
        run_id,
        artifact_outputs,
        artifact_dir_outputs,
        summary_outputs,
        read_only_artifact,
        raw,
        extension_env_providers,
    } = input;
    validate_runner_exec_invocation_shape(script_file.as_deref(), &command)?;
    let script = script_file
        .as_deref()
        .map(read_runner_exec_script)
        .transpose()?;
    let command = prepare_runner_exec_command(script.as_ref(), command)?;
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
    let extension_env_providers = normalize_extension_env_providers(extension_env_providers);
    if read_only_artifact
        && (!artifact_outputs.is_empty()
            || !artifact_dir_outputs.is_empty()
            || !summary_outputs.is_empty()
            || capture_patch)
    {
        return Err(Error::validation_invalid_argument("read_only_artifact", "runner exec --read-only-artifact is a non-destructive retrieval; it cannot be combined with --capture-patch or --artifact/--artifact-dir/--summary output declarations", None, None));
    }
    if dry_run {
        let (cwd, _, _) =
            exec_workspace_context(&runner_id, cwd, sync_workspace, workspace_ref, false)?;
        return runner_exec_dry_run(
            &runner_id,
            cwd,
            allow_diagnostic_ssh,
            require_paths,
            command,
            script.unwrap_or_default(),
        );
    }
    let required_commands = command.first().cloned().into_iter().collect();
    let options = runner::RunnerExecOptions {
        execution_context:
            homeboy::core::runner_job_execution_context::RunnerJobExecutionContext::local("homeboy"),
        cwd,
        project_id,
        allow_diagnostic_ssh,
        diagnostic_ssh_timeout: None,
        command,
        env,
        secret_env_names,
        secret_env_plan: Some(secret_env_plan),
        env_materialization: None,
        capture_patch,
        raw_exec: true,
        source_snapshot: None,
        path_materialization_plan: None,
        capability_preflight: Some(runner::RunnerCapabilityPreflight {
            command: "runner.exec".to_string(),
            required_commands,
            ..Default::default()
        }),
        required_extensions: extension_env_providers.clone(),
        extension_env_providers,
        accepted_extension_settings: Vec::new(),
        require_paths,
        lab_runner_workload: None,
        run_id,
        run_id_owns_generic_exec: true,
        detach_after_handoff: false,
        mirror_evidence: !read_only_artifact,
        print_handoff: should_print_handoff(raw, read_only_artifact),
        read_only_artifact_access: read_only_artifact,
    };
    let mut request = runner::RunnerExecRequest::new(runner_id, options);
    request.sync_workspace = sync_workspace;
    request.workspace_ref = workspace_ref;
    request.hydrate_deps = hydrate_deps;
    request.workspace_sync_timeout = workspace_sync_timeout;
    request.artifact_outputs = artifact_outputs;
    request.artifact_dir_outputs = artifact_dir_outputs;
    request.summary_outputs = summary_outputs;
    runner::exec_request(request)
}

fn normalize_extension_env_providers(extension_ids: Vec<String>) -> Vec<String> {
    let mut providers = Vec::new();
    for extension_id in extension_ids {
        let extension_id = extension_id.trim();
        if !extension_id.is_empty() && !providers.iter().any(|value| value == extension_id) {
            providers.push(extension_id.to_string());
        }
    }
    providers
}

pub(super) fn should_print_handoff(raw: bool, read_only_artifact: bool) -> bool {
    !raw && !read_only_artifact
}

pub(super) fn exec_workspace_context(
    runner_id: &str,
    cwd: Option<String>,
    sync_workspace: Option<String>,
    workspace_ref: Option<String>,
    verify_hydration_source: bool,
) -> homeboy::core::Result<(
    Option<String>,
    Option<homeboy::core::source_snapshot::SourceSnapshot>,
    Option<String>,
)> {
    if let Some(workspace_ref) = workspace_ref {
        if cwd.is_some() || sync_workspace.is_some() {
            return Err(Error::validation_invalid_argument(
                "workspace_ref",
                "--workspace-ref is mutually exclusive with --cwd and --sync-workspace",
                Some(workspace_ref),
                None,
            ));
        }
        let resolved = runner::resolve_workspace_ref(runner_id, &workspace_ref)?;
        if verify_hydration_source {
            runner::verify_workspace_ref_hydration_source(&resolved)?;
        }
        return Ok((
            Some(resolved.remote_path),
            Some(resolved.source_snapshot),
            Some(resolved.local_path),
        ));
    }

    let Some(local_path) = sync_workspace else {
        return Ok((cwd, None, None));
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

    // Non-dry-run synchronization belongs to the canonical RunnerExecRequest.
    Ok((None, None, Some(local_path)))
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
    if path == "-" {
        read_runner_exec_script_from_reader(io::stdin().lock(), "stdin")
    } else {
        let file = fs::File::open(path).map_err(|err| {
            homeboy::core::Error::internal_io(
                err.to_string(),
                Some(format!("read runner exec script {path}")),
            )
        })?;
        read_runner_exec_script_from_reader(file, path)
    }
}

pub(super) fn read_runner_exec_script_from_reader(
    reader: impl Read,
    source: &str,
) -> homeboy::core::Result<String> {
    let (bytes, capture) = read_bounded(reader, RUNNER_EXEC_SCRIPT_LIMIT_BYTES).map_err(|err| {
        homeboy::core::Error::internal_io(
            err.to_string(),
            Some(format!("read runner exec script from {source}")),
        )
    })?;

    if capture.truncated {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "script_file",
            format!(
                "runner exec script exceeds the {} byte limit (retained {} of {}+ bytes); refusing to execute a truncated script",
                capture.limit_bytes, capture.retained_bytes, capture.seen_bytes
            ),
            Some(source.to_string()),
            None,
        ));
    }

    if bytes.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "script_file",
            format!("runner exec script from {source} is empty; provide at least one byte"),
            Some(source.to_string()),
            None,
        ));
    }

    String::from_utf8(bytes).map_err(|err| {
        homeboy::core::Error::internal_io(
            err.to_string(),
            Some(format!("decode runner exec script from {source}")),
        )
    })
}

pub(super) fn prepare_runner_exec_command(
    script: Option<&String>,
    command: Vec<String>,
) -> homeboy::core::Result<Vec<String>> {
    if script.is_some_and(String::is_empty) {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "script_file",
            "runner exec script is empty; provide at least one byte",
            None,
            None,
        ));
    }

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
            runner_exec_script_wrapper(script.expect("script is present")),
        ]),
        (false, false) => Ok(command),
    }
}

/// Build the runner-side script lifecycle wrapper. `HOMEBOY_RUNNER_JOB_ID` is
/// injected after daemon admission, so the path is stable for a durable job
/// without trusting a controller-provided temporary path.
fn runner_exec_script_wrapper(script: &str) -> String {
    let digest = content_hash::sha256_hex(script.as_bytes());
    format!(
        r#"set -e
job_id="${{HOMEBOY_RUNNER_JOB_ID:-local-$$}}"
case "$job_id" in
  ''|*[!A-Za-z0-9._-]*) job_id="local-$$" ;;
esac
script_root="${{XDG_RUNTIME_DIR:-/tmp}}/homeboy-runner-exec/$job_id"
(umask 077; mkdir -p "$script_root")
chmod 700 "$script_root"
script_path="$script_root/script-{digest}.sh"
cleanup() {{ rm -f "$script_path"; rmdir "$script_root" 2>/dev/null || true; }}
trap cleanup EXIT
umask 077
printf '%s' {} > "$script_path"
chmod 500 "$script_path"
export HOMEBOY_RUNNER_EXEC_SCRIPT="$script_path"
export HOMEBOY_RUNNER_EXEC_SCRIPT_SHA256="sha256:{digest}"
bash "$script_path""#,
        shell::quote_arg(script),
    )
}

/// Validate only CLI-provided shape before a script source is opened. In
/// particular, `--script-file -` must not consume or block on stdin for an
/// invocation that already has a command argv.
pub(super) fn validate_runner_exec_invocation_shape(
    script_file: Option<&str>,
    command: &[String],
) -> homeboy::core::Result<()> {
    match (script_file.is_some(), command.is_empty()) {
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
        _ => Ok(()),
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
