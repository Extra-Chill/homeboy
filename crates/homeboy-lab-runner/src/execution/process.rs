use std::collections::{BTreeMap, HashMap};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
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
use sha2::{Digest, Sha256};

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
    let source_snapshot = homeboy_core::source_snapshot::existing_remote(
        &runner.id,
        &cwd,
        runner.workspace_root.as_deref(),
    );
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

    let mut source_snapshot = request
        .source_snapshot
        .unwrap_or_else(|| match runner.kind {
            RunnerKind::Local => homeboy_core::source_snapshot::collect_local(
                &runner.id,
                Path::new(&cwd),
                Some(&cwd),
                PATH_MATERIALIZATION_MODE_EXISTING_REMOTE,
            ),
            RunnerKind::Ssh => homeboy_core::source_snapshot::existing_remote(
                &runner.id,
                &cwd,
                runner.workspace_root.as_deref(),
            ),
        });
    crate::hydrate_prepared_workspace_source_snapshot(&runner.id, &cwd, &mut source_snapshot)?;
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
        resource_guard_env: request_env,
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
    // Execution provenance is distinct from the dispatch markers above. It
    // survives CLI routing so runner-local agent-task plans do not open a
    // second controller-to-runner handoff.
    env.entry(homeboy_core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV.to_string())
        .or_insert_with(|| runner.id.clone());
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
        homeboy_core::source_snapshot::collect_local(
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
        resource_guard_env: request_env,
        secret_env_names: secret_env_plan.secret_env_names(),
        source_snapshot,
        require_paths: request.require_paths,
    })
}

pub(crate) fn execute_runner_process(plan: &PreparedRunnerProcess) -> Result<ProcessOutput> {
    let mut command = std::process::Command::new(&plan.command[0]);
    command.args(&plan.command[1..]).current_dir(&plan.cwd);
    apply_runner_process_env(&mut command, plan)?;

    command_output(
        &mut command,
        &plan.resource_guard_env,
        plan.runner.settings.concurrency_limit,
    )
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
    apply_runner_process_env(&mut command, plan)?;

    command_output_until_cancelled_with_progress(
        &mut command,
        is_cancelled,
        progress_sink,
        require_child_identity_acknowledgement,
        child_started,
        &plan.env,
        &plan.resource_guard_env,
        &plan.secret_env_names,
        plan.runner.settings.concurrency_limit,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    const GUTENBERG_LAB_OFFLOAD_BYTES: usize = 142_952;
    const GUTENBERG_SOURCE_SNAPSHOT_BYTES: usize = 44_680;
    const GUTENBERG_EXECUTION_BUNDLE_BYTES: usize = 8_552;
    const SAFE_CHILD_PROVENANCE_ENV_BYTES: usize = 16 * 1024;

    fn json_payload(bytes: usize) -> String {
        let mut value = serde_json::json!({ "payload": "" });
        let base = serde_json::to_string(&value)
            .expect("JSON serializes")
            .len();
        value["payload"] = serde_json::Value::String("x".repeat(bytes - base));
        let serialized = serde_json::to_string(&value).expect("JSON serializes");
        assert_eq!(serialized.len(), bytes);
        serialized
    }

    fn execution_bundle(bytes: usize, provenance_dir: &Path) -> String {
        let mut value = serde_json::json!({
            "schema": crate::execution_bundle::LAB_EXECUTION_BUNDLE_SCHEMA,
            "binary": crate::execution_bundle::binary(
                "/runner/bin/homeboy",
                Some(homeboy_core::build_identity::current().display.as_str()),
            ),
            "provenance_dir": provenance_dir,
            "admission": { "daemon_lease_id": "lease", "reservation_job_id": "job" },
            "extension_runtime_home": "/runner/job/home",
            "extensions": [{ "id": "fixture", "remote_path": "/runner/job/fixture", "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }],
            "padding": "",
        });
        let base = serde_json::to_string(&value)
            .expect("bundle serializes")
            .len();
        value["padding"] = serde_json::Value::String("x".repeat(bytes - base));
        let serialized = serde_json::to_string(&value).expect("bundle serializes");
        assert_eq!(serialized.len(), bytes);
        serialized
    }

    #[test]
    fn oversized_lab_provenance_is_staged_within_a_safe_child_environment_budget() {
        let stage = tempfile::tempdir().expect("provenance stage");
        let provenance_dir = stage.path().join("run-artifacts/provenance");
        fs::create_dir_all(provenance_dir.parent().expect("artifact root")).expect("artifact root");
        let lab_offload = json_payload(GUTENBERG_LAB_OFFLOAD_BYTES);
        let source_snapshot = json_payload(GUTENBERG_SOURCE_SNAPSHOT_BYTES);
        let bundle = execution_bundle(GUTENBERG_EXECUTION_BUNDLE_BYTES, &provenance_dir);
        assert_eq!(
            (lab_offload.len(), source_snapshot.len(), bundle.len()),
            (
                GUTENBERG_LAB_OFFLOAD_BYTES,
                GUTENBERG_SOURCE_SNAPSHOT_BYTES,
                GUTENBERG_EXECUTION_BUNDLE_BYTES,
            )
        );

        let mut env = HashMap::from([
            (
                homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV.to_string(),
                lab_offload.clone(),
            ),
            (
                crate::execution_bundle::LAB_EXECUTION_BUNDLE_ENV.to_string(),
                bundle.clone(),
            ),
            (
                "HOMEBOY_EXISTING_BEHAVIOR".to_string(),
                "preserved".to_string(),
            ),
        ]);
        env = child_provenance_env(env, source_snapshot.clone()).expect("stage env");

        let provenance_names = [
            homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV,
            homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV,
            crate::execution_bundle::LAB_EXECUTION_BUNDLE_ENV,
        ];
        let child_provenance_bytes = provenance_names
            .iter()
            .map(|name| env.get(*name).expect("child provenance key").len())
            .sum::<usize>();
        assert!(child_provenance_bytes < SAFE_CHILD_PROVENANCE_ENV_BYTES);
        assert_eq!(env["HOMEBOY_EXISTING_BEHAVIOR"], "preserved");
        assert_eq!(
            homeboy_core::observation::resolve_json_value(
                env.get(homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV)
                    .expect("offload reference"),
            ),
            serde_json::from_str::<serde_json::Value>(&lab_offload).ok(),
        );
        assert_eq!(
            homeboy_core::observation::resolve_json_value(
                env.get(homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV)
                    .expect("snapshot reference"),
            ),
            serde_json::from_str::<serde_json::Value>(&source_snapshot).ok(),
        );
        assert!(crate::execution_bundle::validate_bundle_env(
            &env,
            &["/runner/bin/homeboy".to_string(), "fuzz".to_string()],
            &["fixture".to_string()],
        ));
        assert_eq!(
            fs::read_dir(stage.path().join("run-artifacts/provenance"))
                .expect("staged files")
                .count(),
            3
        );
        assert!(!stage.path().join("provenance").exists());
    }

    #[test]
    fn concurrent_staging_atomically_reuses_one_valid_hash_file() {
        let root = tempfile::tempdir().expect("run artifact root");
        let provenance_dir = root.path().join("provenance");
        let payload = json_payload(GUTENBERG_LAB_OFFLOAD_BYTES);
        let bundle = serde_json::json!({
            "schema": crate::execution_bundle::LAB_EXECUTION_BUNDLE_SCHEMA,
            "binary": {
                "path": "/runner/bin/homeboy",
                "build_identity": homeboy_core::build_identity::current().display,
            },
            "provenance_dir": provenance_dir,
            "admission": { "daemon_lease_id": "lease", "reservation_job_id": "job" },
        });
        let env = HashMap::from([
            (
                crate::execution_bundle::LAB_EXECUTION_BUNDLE_ENV.to_string(),
                bundle.to_string(),
            ),
            (
                homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV.to_string(),
                payload.clone(),
            ),
        ]);
        let barrier = Arc::new(Barrier::new(8));
        let handles = (0..8)
            .map(|_| {
                let env = env.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    child_provenance_env(env, "{}".to_string()).expect("concurrent stage")
                })
            })
            .collect::<Vec<_>>();
        let references = handles
            .into_iter()
            .map(|handle| {
                handle.join().expect("staging thread")
                    [homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV]
                    .clone()
            })
            .collect::<Vec<_>>();

        assert!(references
            .iter()
            .all(|reference| reference == &references[0]));
        let files = fs::read_dir(root.path().join("provenance"))
            .expect("provenance files")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("directory entries");
        assert_eq!(files.len(), 1);
        assert_eq!(
            fs::read(files[0].path()).expect("published payload"),
            payload.as_bytes()
        );
    }

    #[test]
    fn mixed_build_does_not_replace_established_inline_json() {
        let root = tempfile::tempdir().expect("run artifact root");
        let payload = json_payload(GUTENBERG_LAB_OFFLOAD_BYTES);
        let env = HashMap::from([
            (
                crate::execution_bundle::LAB_EXECUTION_BUNDLE_ENV.to_string(),
                serde_json::json!({
                    "schema": crate::execution_bundle::LAB_EXECUTION_BUNDLE_SCHEMA,
                    "binary": { "path": "/runner/bin/homeboy", "build_identity": "homeboy older-build" },
                    "provenance_dir": root.path().join("provenance"),
                    "admission": { "daemon_lease_id": "lease", "reservation_job_id": "job" },
                }).to_string(),
            ),
            (
                homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV.to_string(),
                payload,
            ),
        ]);

        let error = child_provenance_env(env, "{}".to_string())
            .expect_err("mixed build must fail before replacement");
        assert!(error
            .message
            .contains("does not support provenance references"));
        assert!(!root.path().join("provenance").exists());
    }

    #[test]
    fn missing_verified_build_identity_does_not_replace_oversized_json() {
        let root = tempfile::tempdir().expect("run artifact root");
        let env = HashMap::from([
            (
                crate::execution_bundle::LAB_EXECUTION_BUNDLE_ENV.to_string(),
                serde_json::json!({
                    "schema": crate::execution_bundle::LAB_EXECUTION_BUNDLE_SCHEMA,
                    "binary": crate::execution_bundle::binary("/runner/bin/homeboy", None),
                    "provenance_dir": root.path().join("provenance"),
                    "admission": { "daemon_lease_id": "lease", "reservation_job_id": "job" },
                })
                .to_string(),
            ),
            (
                homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV.to_string(),
                json_payload(GUTENBERG_LAB_OFFLOAD_BYTES),
            ),
        ]);

        let error = child_provenance_env(env, "{}".to_string())
            .expect_err("missing verified identity must fail before replacement");
        assert!(error
            .message
            .contains("child build identity is unavailable"));
        assert!(!root.path().join("provenance").exists());
    }

    #[cfg(unix)]
    #[test]
    fn staging_rejects_a_symlinked_provenance_directory() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("run artifact root");
        let outside = tempfile::tempdir().expect("outside directory");
        let provenance_dir = root.path().join("provenance");
        symlink(outside.path(), &provenance_dir).expect("symlink provenance directory");
        let env = HashMap::from([
            (
                crate::execution_bundle::LAB_EXECUTION_BUNDLE_ENV.to_string(),
                serde_json::json!({
                    "schema": crate::execution_bundle::LAB_EXECUTION_BUNDLE_SCHEMA,
                    "binary": {
                        "path": "/runner/bin/homeboy",
                        "build_identity": homeboy_core::build_identity::current().display,
                    },
                    "provenance_dir": provenance_dir,
                    "admission": { "daemon_lease_id": "lease", "reservation_job_id": "job" },
                })
                .to_string(),
            ),
            (
                homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV.to_string(),
                json_payload(GUTENBERG_LAB_OFFLOAD_BYTES),
            ),
        ]);

        let error = child_provenance_env(env, "{}".to_string())
            .expect_err("symlinked stage directory must fail closed");
        assert!(error.message.contains("not a real directory"));
        assert_eq!(
            fs::read_dir(outside.path())
                .expect("outside directory")
                .count(),
            0
        );
    }
}

pub(super) fn apply_runner_process_env(
    command: &mut std::process::Command,
    plan: &PreparedRunnerProcess,
) -> Result<()> {
    command.env_clear();
    for key in inherited_runner_process_env_keys() {
        if !plan.env.contains_key(*key) {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
    }
    let env = child_provenance_env(
        plan.env.clone(),
        serde_json::to_string(&plan.source_snapshot).unwrap_or_default(),
    )?;
    preserve_durable_homeboy_data_dir(command, &env);
    command.envs(env.iter());
    Ok(())
}

const PROVENANCE_ENV_INLINE_MAX_BYTES: usize = 8 * 1024;

fn child_provenance_env(
    mut env: HashMap<String, String>,
    source_snapshot: String,
) -> Result<HashMap<String, String>> {
    env.insert(
        homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV.to_string(),
        source_snapshot,
    );
    let provenance_names = [
        homeboy_core::observation::LAB_OFFLOAD_METADATA_ENV,
        homeboy_core::observation::SOURCE_SNAPSHOT_METADATA_ENV,
        crate::execution_bundle::LAB_EXECUTION_BUNDLE_ENV,
    ];
    if !provenance_names.iter().any(|name| {
        env.get(*name)
            .is_some_and(|raw| raw.len() > PROVENANCE_ENV_INLINE_MAX_BYTES)
    }) {
        return Ok(env);
    }
    let bundle = env
        .get(crate::execution_bundle::LAB_EXECUTION_BUNDLE_ENV)
        .and_then(|raw| homeboy_core::observation::resolve_json_value(raw))
        .ok_or_else(|| provenance_reference_error("execution bundle is missing or invalid"))?;
    let child_identity = bundle
        .pointer("/binary/build_identity")
        .and_then(serde_json::Value::as_str)
        .filter(|identity| !identity.trim().is_empty())
        .ok_or_else(|| provenance_reference_error("child build identity is unavailable"))?;
    if child_identity.trim() != homeboy_core::build_identity::current().display {
        return Err(provenance_reference_error(&format!(
            "child build `{child_identity}` does not support provenance references from runner build `{}`",
            homeboy_core::build_identity::current().display
        )));
    }
    let stage_dir = bundle
        .get("provenance_dir")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            provenance_reference_error("run-owned provenance directory is unavailable")
        })?;
    ensure_restricted_stage_dir(&stage_dir)?;

    for name in provenance_names {
        let Some(raw) = env.get(name).cloned() else {
            continue;
        };
        if raw.len() <= PROVENANCE_ENV_INLINE_MAX_BYTES {
            continue;
        }
        let hash = format!("{:x}", Sha256::digest(raw.as_bytes()));
        let path = stage_dir.join(format!("{hash}.json"));
        publish_provenance(&stage_dir, &path, raw.as_bytes(), &hash)?;
        let reference = serde_json::json!({
            "schema": homeboy_core::observation::PROVENANCE_REFERENCE_SCHEMA,
            "path": path,
            "sha256": hash,
        });
        env.insert(
            name.to_string(),
            serde_json::to_string(&reference).expect("provenance reference serializes"),
        );
    }
    Ok(env)
}

fn provenance_reference_error(reason: &str) -> Error {
    Error::validation_invalid_argument(
        "provenance_transport",
        format!("cannot safely bound child provenance environment: {reason}"),
        None,
        Some(vec![
            "Run the child with the admitted Homeboy build or refresh the runner before retrying."
                .to_string(),
        ]),
    )
}

fn ensure_restricted_stage_dir(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("inspect provenance directory".to_string()),
                )
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(provenance_reference_error(
                    "run-owned provenance path is not a real directory",
                ));
            }
        }
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some("create run-owned provenance directory".to_string()),
            ));
        }
    }
    restrict_provenance_permissions(path, 0o700)
}

fn publish_provenance(stage_dir: &Path, path: &Path, payload: &[u8], hash: &str) -> Result<()> {
    if path.exists() {
        return validate_published_provenance(path, hash);
    }
    let temp_path = stage_dir.join(format!(".{hash}.{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut temp = options.open(&temp_path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create exclusive provenance temp file".to_string()),
        )
    })?;
    let publish = (|| {
        temp.write_all(payload).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("write provenance temp file".to_string()),
            )
        })?;
        temp.sync_all().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("sync provenance temp file".to_string()),
            )
        })?;
        drop(temp);
        match fs::hard_link(&temp_path, path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                validate_published_provenance(path, hash)
            }
            Err(error) => Err(Error::internal_io(
                error.to_string(),
                Some("atomically publish provenance file".to_string()),
            )),
        }
    })();
    let _ = fs::remove_file(temp_path);
    publish
}

fn validate_published_provenance(path: &Path, expected_hash: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("inspect published provenance".to_string()),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(provenance_reference_error(
            "published provenance path is not a regular file",
        ));
    }
    let contents = fs::read(path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read published provenance".to_string()),
        )
    })?;
    let actual_hash = format!("{:x}", Sha256::digest(&contents));
    if actual_hash != expected_hash {
        return Err(provenance_reference_error(
            "existing content-addressed provenance file failed hash verification",
        ));
    }
    restrict_provenance_permissions(path, 0o600)
}

#[cfg(unix)]
fn restrict_provenance_permissions(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("restrict provenance permissions".to_string()),
        )
    })
}

#[cfg(not(unix))]
fn restrict_provenance_permissions(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}

fn preserve_durable_homeboy_data_dir(
    command: &mut std::process::Command,
    job_env: &HashMap<String, String>,
) {
    let data_dir = durable_homeboy_data_dir_for_job(
        job_env,
        std::env::var("HOME").ok().as_deref(),
        homeboy_core::paths::homeboy_data().ok(),
    );
    if let Some(data_dir) = data_dir {
        command.env(homeboy_core::paths::HOMEBOY_DATA_DIR_ENV, data_dir);
    }
}

pub(super) fn durable_homeboy_data_dir_for_job(
    job_env: &HashMap<String, String>,
    runner_home: Option<&str>,
    runner_data_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    if job_env.contains_key(homeboy_core::paths::HOMEBOY_DATA_DIR_ENV) {
        return None;
    }
    let job_home = job_env.get("HOME")?;
    if runner_home == Some(job_home) {
        return None;
    }
    runner_data_dir
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
    env: &HashMap<String, String>,
    concurrency_limit: Option<usize>,
) -> Result<ProcessOutput> {
    let measured = measured_command_output(command, env, concurrency_limit)?;
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
    resource_guard_env: &HashMap<String, String>,
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
        resource_guard_env,
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
