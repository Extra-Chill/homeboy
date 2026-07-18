use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use homeboy_core::engine::command::StdoutLineObserver;
use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};
use homeboy_core::redaction::{redact_argv, RedactionPolicy};
use homeboy_core::runner_execution_envelope::{
    PathMaterializationPlan, PATH_MATERIALIZATION_MODE_EXISTING_REMOTE,
};
use homeboy_core::secret_env_plan::SecretEnvPlan;
use homeboy_core::server::{self, SshClient};
use homeboy_core::source_snapshot::SourceSnapshot;

use super::super::normalize_runner_command_env_for_homeboy_path;
use super::super::resource_metrics::{
    measured_command_output, measured_command_output_until_cancelled_with_progress,
    RunnerCommandProgressSink,
};
use super::super::{load, Runner, RunnerKind};

use super::policy::{validate_runner_policy, RunnerPolicyRequest};

#[allow(unused_imports)]
use super::*;

pub(super) fn exec_local(plan: PreparedRunnerProcess) -> Result<(RunnerExecOutput, i32)> {
    let output = execute_runner_process(&plan)?;
    Ok(exec_output(
        &plan.runner,
        RunnerExecMode::Local,
        plan.cwd,
        plan.command,
        output,
        Some(plan.source_snapshot),
        None,
        plan.require_paths,
        &plan.env,
        &[],
    ))
}

pub(super) fn exec_diagnostic_ssh(
    runner: &Runner,
    cwd: String,
    command: Vec<String>,
    env: HashMap<String, String>,
    secret_env_names: &[String],
    require_paths: Vec<String>,
    path_materialization_plan: Option<PathMaterializationPlan>,
    timeout: Option<Duration>,
) -> Result<(RunnerExecOutput, i32)> {
    let server_id = runner.server_id.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "server_id",
            "SSH runner requires server_id",
            Some(runner.id.clone()),
            None,
        )
    })?;
    let server = server::load(server_id)?;
    let mut merged_env = runner.env.clone();
    merged_env.extend(env);
    // Keep the full merged env for stream redaction, but route secret-bearing
    // entries over stdin rather than the SSH command argv so tokens never land
    // in the controller `ps` table or the remote login-shell argv (#6676).
    let redaction_env = merged_env.clone();
    let (public_env, secret_env) = partition_runner_secret_env(merged_env, secret_env_names);
    let mut client = SshClient::from_server(&server, server_id)?;
    client.env.extend(public_env);
    validate_remote_required_paths(&mut client, &require_paths)?;
    let command_line = format!(
        "cd {} && {}",
        shell::quote_arg(&cwd),
        command
            .iter()
            .map(|arg| shell::quote_arg(arg))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let output = timeout
        .map(|timeout| {
            client.execute_with_secret_env_and_timeout(&command_line, &secret_env, timeout)
        })
        .unwrap_or_else(|| client.execute_with_secret_env(&command_line, &secret_env));
    let source_snapshot =
        SourceSnapshot::existing_remote(&runner.id, &cwd, runner.workspace_root.as_deref());
    Ok(exec_output(
        runner,
        RunnerExecMode::DiagnosticSsh,
        cwd,
        command,
        ProcessOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
            metrics: None,
            capture: None,
        },
        Some(source_snapshot),
        path_materialization_plan,
        require_paths,
        &redaction_env,
        &[],
    ))
}

/// Split a runner's merged environment into the public exports that stay inline
/// on the SSH command line and the secret values that must be streamed over
/// stdin instead.
///
/// A key is treated as secret when the caller explicitly declared it in
/// `secret_env_names` or when the shared redaction policy recognizes it as a
/// sensitive key (token/secret/api_key/...). This is defense in depth: any
/// credential-shaped value is kept off the argv even if a caller forgets to
/// declare its name.
fn partition_runner_secret_env(
    env: HashMap<String, String>,
    secret_env_names: &[String],
) -> (HashMap<String, String>, BTreeMap<String, String>) {
    let policy = RedactionPolicy::default();
    let mut public_env = HashMap::new();
    let mut secret_env = BTreeMap::new();
    for (key, value) in env {
        let is_secret =
            secret_env_names.iter().any(|name| name == &key) || policy.is_sensitive_key(&key);
        if is_secret {
            secret_env.insert(key, value);
        } else {
            public_env.insert(key, value);
        }
    }
    (public_env, secret_env)
}

pub(crate) fn prepare_runner_process(
    request: RunnerProcessRequest,
) -> Result<PreparedRunnerProcess> {
    if request.command.is_empty() {
        return Err(Error::validation_invalid_argument(
            "command",
            "runner exec requires a command after --",
            None,
            None,
        ));
    }

    let mut runner = request
        .runner
        .map(|mut runner| {
            if runner.id.is_empty() {
                runner.id = request.runner_id.clone();
            }
            runner
        })
        .map(Ok)
        .unwrap_or_else(|| load(&request.runner_id))?;
    let cwd = resolve_cwd(&runner, request.cwd.as_deref())?;
    validate_runner_process_cwd(&runner, &cwd)?;
    if runner.kind != RunnerKind::Local {
        super::super::source_materialization::validate_runner_exec_source_fetch(
            &request.command,
            &runner.id,
        )?;
        let secret_env_plan = runner_exec_secret_env_plan(
            &request.command,
            None,
            &request.secret_env_names,
            &request.env,
            request.secret_env_plan.clone(),
        );
        provision_provider_file_secret_sources_for_runner(
            &runner,
            &request.command,
            &secret_env_plan.secret_env_names(),
            &request.env,
        )?;
    }
    validate_runner_policy(
        &runner,
        &cwd,
        RunnerPolicyRequest {
            project_id: request.project_id.as_deref(),
            command: &request.command,
            capture_patch: request.capture_patch,
            raw_exec: request.raw_exec,
        },
    )?;

    let raw_runner_env = runner.env.clone();
    let request_env = request.env.clone();
    let mut env = raw_runner_env.clone();
    env.extend(request_env.clone());
    if runner.kind != RunnerKind::Local {
        env.insert(RUNNER_HOSTED_EXEC_ENV.to_string(), "1".to_string());
        env.insert(RUNNER_PLACEMENT_RESOLVED_ENV.to_string(), "1".to_string());
        env.insert(RUNNER_ID_ENV.to_string(), runner.id.clone());
    }
    let secret_env_plan = runner_exec_secret_env_plan(
        &request.command,
        None,
        &request.secret_env_names,
        &env,
        request.secret_env_plan.clone(),
    );

    if runner.kind == RunnerKind::Local {
        validate_runner_inherited_secret_env(
            &secret_env_plan,
            &request_env,
            "local controller env",
            true,
        )?;
        let runner_env = validated_runner_inherited_env(
            &secret_env_plan,
            raw_runner_env,
            "local controller runner.env",
            false,
        )?;
        runner.env = runner_env.clone();
        env = runner_env;
        env.extend(request_env.clone());
        // Stamp the dispatch-only runner-placement markers for local execs too.
        // These are kept out of argv so nested Homeboy commands (parity preflight
        // `extension show`, extension materialization, ready_check chains) do not
        // re-enter routing and dispatch themselves a second time. Without them a
        // local runner exec that carries an explicit `--placement` recursively
        // spawns `homeboy component show` / `extension show` and the extension
        // ready_check, saturating the host (#8115).
        env.insert(RUNNER_HOSTED_EXEC_ENV.to_string(), "1".to_string());
        env.insert(RUNNER_PLACEMENT_RESOLVED_ENV.to_string(), "1".to_string());
        env.insert(RUNNER_ID_ENV.to_string(), runner.id.clone());
        env.extend(resolve_runner_secret_env_for_plan(
            &runner.secret_env,
            &secret_env_plan,
            &env,
        )?);
        normalize_runner_command_env_for_homeboy_path(
            &mut env,
            runner.settings.homeboy_path.as_deref(),
        );
    } else {
        validate_runner_inherited_secret_env(
            &secret_env_plan,
            &request_env,
            "local controller env",
            true,
        )?;
        let runner_env = validated_runner_inherited_env(
            &secret_env_plan,
            raw_runner_env,
            "local controller runner.env",
            false,
        )?;
        runner.env = runner_env.clone();
        env = runner_env;
        env.extend(request_env.clone());
        env.insert(RUNNER_HOSTED_EXEC_ENV.to_string(), "1".to_string());
        env.insert(RUNNER_PLACEMENT_RESOLVED_ENV.to_string(), "1".to_string());
        env.insert(RUNNER_ID_ENV.to_string(), runner.id.clone());
        env.extend(resolve_controller_secret_env_for_command(
            &runner.secret_env,
            &secret_env_plan.secret_env_names(),
            &env,
        )?);
    }

    let source_snapshot = request
        .source_snapshot
        .unwrap_or_else(|| match runner.kind {
            RunnerKind::Local => SourceSnapshot::collect_local(
                &runner.id,
                Path::new(&cwd),
                Some(&cwd),
                PATH_MATERIALIZATION_MODE_EXISTING_REMOTE,
            ),
            RunnerKind::Ssh => {
                SourceSnapshot::existing_remote(&runner.id, &cwd, runner.workspace_root.as_deref())
            }
        });
    validate_required_paths(
        &runner,
        &request.require_paths,
        request.validate_require_paths_on_host,
    )?;

    Ok(PreparedRunnerProcess {
        runner,
        cwd,
        command: request.command,
        env,
        secret_env_names: secret_env_plan.secret_env_names(),
        source_snapshot,
        require_paths: request.require_paths,
    })
}

pub(crate) fn prepare_daemon_local_process(
    request: RunnerProcessRequest,
) -> Result<PreparedRunnerProcess> {
    if request.command.is_empty() {
        return Err(Error::validation_invalid_argument(
            "command",
            "runner exec requires a command after --",
            None,
            None,
        ));
    }

    let cwd = request.cwd.ok_or_else(|| {
        Error::validation_invalid_argument(
            "cwd",
            "daemon exec requires an absolute cwd",
            Some(request.runner_id.clone()),
            Some(vec![
                "Pass the synced remote workspace path as cwd when submitting daemon exec."
                    .to_string(),
            ]),
        )
    })?;
    let mut runner = request
        .runner
        .map(|mut runner| {
            if runner.id.is_empty() {
                runner.id = request.runner_id.clone();
            }
            runner.kind = RunnerKind::Local;
            runner.server_id = None;
            runner.workspace_root = runner.workspace_root.or_else(|| Some(cwd.clone()));
            runner
        })
        .unwrap_or_else(|| Runner {
            id: request.runner_id,
            kind: RunnerKind::Local,
            server_id: None,
            workspace_root: Some(cwd.clone()),
            settings: server::RunnerSettings::default(),
            env: HashMap::new(),
            secret_env: HashMap::new(),
            resources: HashMap::new(),
            policy: server::RunnerPolicy::default(),
        });
    validate_runner_process_cwd(&runner, &cwd)?;
    validate_required_paths(
        &runner,
        &request.require_paths,
        request.validate_require_paths_on_host,
    )?;

    let raw_runner_env = runner.env.clone();
    let request_env = request.env.clone();
    let mut env = raw_runner_env.clone();
    env.extend(request_env.clone());
    let secret_env_plan = runner_exec_secret_env_plan(
        &request.command,
        None,
        &request.secret_env_names,
        &env,
        request.secret_env_plan.clone(),
    );
    validate_runner_inherited_secret_env(
        &secret_env_plan,
        &request_env,
        "local controller env",
        true,
    )?;
    let runner_env = validated_runner_inherited_env(
        &secret_env_plan,
        raw_runner_env,
        "remote runner daemon env",
        false,
    )?;
    runner.env = runner_env.clone();
    env = runner_env;
    env.extend(request_env.clone());
    // Daemon jobs are already executing on the selected runner. Keep this
    // dispatch-only marker out of argv so nested Homeboy commands do not route
    // themselves a second time.
    env.insert(RUNNER_HOSTED_EXEC_ENV.to_string(), "1".to_string());
    env.insert(RUNNER_PLACEMENT_RESOLVED_ENV.to_string(), "1".to_string());
    env.insert(RUNNER_ID_ENV.to_string(), runner.id.clone());
    env.extend(resolve_runner_secret_env_for_plan(
        &runner.secret_env,
        &secret_env_plan,
        &env,
    )?);
    normalize_runner_command_env_for_homeboy_path(
        &mut env,
        runner.settings.homeboy_path.as_deref(),
    );
    let source_snapshot = request.source_snapshot.unwrap_or_else(|| {
        SourceSnapshot::collect_local(
            &runner.id,
            Path::new(&cwd),
            Some(&cwd),
            PATH_MATERIALIZATION_MODE_EXISTING_REMOTE,
        )
    });

    Ok(PreparedRunnerProcess {
        runner,
        cwd,
        command: request.command,
        env,
        secret_env_names: secret_env_plan.secret_env_names(),
        source_snapshot,
        require_paths: request.require_paths,
    })
}

pub(crate) fn execute_runner_process(plan: &PreparedRunnerProcess) -> Result<ProcessOutput> {
    let mut command = std::process::Command::new(&plan.command[0]);
    command.args(&plan.command[1..]).current_dir(&plan.cwd);
    apply_runner_process_env(&mut command, plan);

    command_output(&mut command, plan.runner.settings.concurrency_limit)
}

pub(crate) fn execute_runner_process_until_cancelled_with_progress(
    plan: &PreparedRunnerProcess,
    is_cancelled: impl FnMut() -> bool,
    progress_sink: Option<RunnerCommandProgressSink>,
    require_child_identity_acknowledgement: bool,
    child_started: Option<Arc<dyn Fn(u32) -> Result<()> + Send + Sync + 'static>>,
) -> Result<ProcessOutput> {
    let mut command = std::process::Command::new(&plan.command[0]);
    command.args(&plan.command[1..]).current_dir(&plan.cwd);
    apply_runner_process_env(&mut command, plan);

    command_output_until_cancelled_with_progress(
        &mut command,
        is_cancelled,
        progress_sink,
        require_child_identity_acknowledgement,
        child_started,
        &plan.env,
        &plan.secret_env_names,
        plan.runner.settings.concurrency_limit,
    )
}

pub(super) fn apply_runner_process_env(
    command: &mut std::process::Command,
    plan: &PreparedRunnerProcess,
) {
    apply_runner_process_env_with_inherited(command, plan, std::env::vars_os());
}

pub(super) fn apply_runner_process_env_with_inherited(
    command: &mut std::process::Command,
    plan: &PreparedRunnerProcess,
    inherited: impl IntoIterator<Item = (OsString, OsString)>,
) {
    let inherited = inherited.into_iter().collect::<HashMap<_, _>>();
    report_pruned_inherited_environment(&inherited);
    command.env_clear();
    for key in inherited_runner_process_env_keys() {
        if !plan.env.contains_key(*key) {
            if let Some(value) = inherited.get(&OsString::from(key)) {
                command.env(key, value);
            }
        }
    }
    command.envs(plan.env.iter()).env(
        homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV,
        serde_json::to_string(&plan.source_snapshot).unwrap_or_default(),
    );
}

/// Child processes receive only explicit job values plus the minimal host
/// baseline below. This avoids carrying a daemon's unrelated inherited payload
/// into `execve`, where it can exceed the platform argument/environment limit.
fn report_pruned_inherited_environment(inherited: &HashMap<OsString, OsString>) {
    const DIAGNOSTIC_THRESHOLD_BYTES: usize = 128 * 1024;

    let inherited = inherited
        .iter()
        .map(|(name, value)| name.to_string_lossy().len() + value.to_string_lossy().len() + 2)
        .sum::<usize>();
    if inherited >= DIAGNOSTIC_THRESHOLD_BYTES {
        eprintln!(
            "runner child spawn environment bounded: omitted {inherited} bytes of inherited environment; only explicit job variables and the minimal host baseline are forwarded"
        );
    }
}

fn validate_runner_inherited_secret_env(
    plan: &SecretEnvPlan,
    env: &HashMap<String, String>,
    source: &str,
    enforce_without_secret_contract: bool,
) -> Result<()> {
    if env.is_empty() || (!enforce_without_secret_contract && plan.secret_env_names().is_empty()) {
        return Ok(());
    }
    let env = env
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    plan.diagnose_inherited_env(&env, source)
        .map(|_| ())
        .map_err(|error| {
            Error::validation_invalid_argument(
                "env",
                format!("{} in {source}", error.message),
                None,
                Some(vec![
                    "Declare secret-bearing runtime env names in secret_env or HOMEBOY_AGENT_RUNTIME_SECRET_ENV before runner execution.".to_string(),
                    "Keep non-secret runtime configuration in explicit public env; Homeboy rejects inherited sensitive env names that are neither public nor secret-declared.".to_string(),
                ]),
            )
        })
}

fn validated_runner_inherited_env(
    plan: &SecretEnvPlan,
    env: HashMap<String, String>,
    source: &str,
    enforce_without_secret_contract: bool,
) -> Result<HashMap<String, String>> {
    validate_runner_inherited_secret_env(plan, &env, source, enforce_without_secret_contract)?;
    if enforce_without_secret_contract || !plan.secret_env_names().is_empty() {
        return Ok(env);
    }

    let policy = RedactionPolicy::default();
    Ok(env
        .into_iter()
        .filter(|(name, _)| !policy.is_sensitive_key(name))
        .collect())
}

pub(super) fn inherited_runner_process_env_keys() -> &'static [&'static str] {
    &["HOME", "USER", "LOGNAME", "SHELL", "TMPDIR", "TEMP", "TMP"]
}

pub(super) fn command_output(
    command: &mut std::process::Command,
    concurrency_limit: Option<usize>,
) -> Result<ProcessOutput> {
    let measured = measured_command_output(command, concurrency_limit)?;
    let output = measured.output;
    Ok(ProcessOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(1),
        metrics: Some(measured.metrics),
        capture: Some(measured.capture),
    })
}

pub(super) fn command_output_until_cancelled_with_progress(
    command: &mut std::process::Command,
    is_cancelled: impl FnMut() -> bool,
    progress_sink: Option<RunnerCommandProgressSink>,
    require_child_identity_acknowledgement: bool,
    child_started: Option<Arc<dyn Fn(u32) -> Result<()> + Send + Sync + 'static>>,
    env: &HashMap<String, String>,
    secret_env_names: &[String],
    concurrency_limit: Option<usize>,
) -> Result<ProcessOutput> {
    let stdout_line_observer = progress_sink.as_ref().map(|sink| {
        let sink = Arc::clone(sink);
        let env = env.clone();
        let secret_env_names = secret_env_names.to_vec();
        Arc::new(move |line: &str| {
            if let Some(progress) =
                crate::progress::parse_child_progress_line(line, &env, &secret_env_names)
            {
                let _ = sink(progress);
            }
        }) as StdoutLineObserver
    });
    let measured = measured_command_output_until_cancelled_with_progress(
        command,
        is_cancelled,
        progress_sink,
        require_child_identity_acknowledgement,
        stdout_line_observer,
        child_started,
        concurrency_limit,
    )?;
    let output = measured.output;
    Ok(ProcessOutput {
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        exit_code: output.status.code().unwrap_or(1),
        metrics: Some(measured.metrics),
        capture: Some(measured.capture),
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn exec_output(
    runner: &Runner,
    mode: RunnerExecMode,
    cwd: String,
    command: Vec<String>,
    output: ProcessOutput,
    source_snapshot: Option<SourceSnapshot>,
    path_materialization_plan: Option<PathMaterializationPlan>,
    require_paths: Vec<String>,
    redaction_env: &HashMap<String, String>,
    secret_env_names: &[String],
) -> (RunnerExecOutput, i32) {
    let exit_code = output.exit_code;
    let (stdout, stderr) = redact_runner_exec_streams(
        output.stdout,
        output.stderr,
        redaction_env,
        secret_env_names,
    );
    let transport = match mode {
        RunnerExecMode::Daemon => "daemon",
        RunnerExecMode::Local => "local",
        RunnerExecMode::ReverseBroker => "reverse_broker",
        RunnerExecMode::DiagnosticSsh => "diagnostic_ssh",
    };
    let runner_result = runner_result(None, exit_code, &stdout, &stderr, None, None);
    let handoff = lab_runner_handoff(runner, transport, None, Some(runner_result.clone()));
    let provenance_extensions = required_extensions_for_command(&command, &[]);
    let execution_record = runner_execution_record_for_output(
        runner,
        transport,
        exit_code,
        None,
        None,
        source_snapshot.as_ref(),
        path_materialization_plan,
        &require_paths,
        &provenance_extensions,
        &[],
        Some(&runner_result),
    );
    (
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: runner.id.clone(),
            dry_run: false,
            mode,
            argv: redact_argv(&command),
            remote_cwd: cwd,
            exit_code,
            stdout,
            stderr,
            source_snapshot: source_snapshot.clone(),
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
            metrics: output.metrics,
            capture: output.capture,
            execution_record: Some(execution_record),
            runner_result: Some(runner_result),
            handoff: Some(handoff),
            diagnostics: runner_exec_diagnostics(runner, source_snapshot.as_ref(), &require_paths),
        },
        exit_code,
    )
}
