use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use homeboy_engine_primitives::command::{ExecutionCleanup, ExecutionOutcome, ExecutionOwner};
use homeboy_engine_primitives::shell;
use serde::Serialize;
use serde_json::{json, Value};

use crate::error::{Error, ErrorCode, Result};
use crate::observation::{NewRunRecordBuilder, ObservationStore, RunStatus};
use crate::redaction::RedactionPolicy;

const TAIL_BYTES: u64 = 4096;
const SETUP_RESULT_FILE_ENV: &str = "HOMEBOY_EXTENSION_SETUP_RESULT_FILE";

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum SetupPhase {
    ExtensionRuntimeSetup,
}

#[derive(Serialize)]
struct SetupInvocation<'a> {
    phase: SetupPhase,
    extension_id: &'a str,
    executable: &'static str,
    arguments: [&'a str; 2],
    budget_ms: u128,
    retry_command: &'a str,
}

pub(super) fn execute(
    extension_id: &str,
    command: &str,
    cwd: &Path,
    budget: Duration,
    retry_args: &[String],
    timeout_override: Option<&str>,
) -> Result<i32> {
    let redactor = SetupRedactor::new();
    let (executable, shell_flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };
    let invocation = redactor.command(command);
    let retry = shell::quote_args(
        &redactor.policy.redact_argv(
            &retry_args
                .iter()
                .map(|arg| redactor.text(arg))
                .collect::<Vec<_>>(),
        ),
    );
    let retry = match timeout_override {
        Some(seconds) => format!("{}={seconds} {retry}", super::EXTENSION_SETUP_TIMEOUT_ENV),
        None => retry,
    };
    let identity = SetupInvocation {
        phase: SetupPhase::ExtensionRuntimeSetup,
        extension_id,
        executable,
        arguments: [shell_flag, &invocation],
        budget_ms: budget.as_millis(),
        retry_command: &retry,
    };

    // Commit the run before spawning; the extension tree may be rolled back on
    // failure. Do not use ActiveObservation, which would replace a parent's run ID.
    let store = ObservationStore::open_initialized()?;
    let run = store.start_run(
        NewRunRecordBuilder::new("extension.setup")
            .command("extension setup")
            .cwd_path(cwd)
            .current_homeboy_version()
            .metadata(json!({"setup": identity, "owner_pid": std::process::id()}))
            .build(),
    )?;
    let mut diagnostic = json!({
        "schema": "homeboy/extension-setup-result/v1",
        "run_id": run.id,
        "status": "running",
        "invocation": identity,
    });
    let mut timed_out = false;
    let result = (|| -> Result<i32> {
        write_sidecar(&diagnostic)?;
        let stdout = tempfile::tempfile().map_err(setup_io)?;
        let stderr = tempfile::tempfile().map_err(setup_io)?;
        let mut process = Command::new(executable);
        process.args([shell_flag, command]).current_dir(cwd);
        process.stdout(stdout.try_clone().map_err(setup_io)?);
        process.stderr(stderr.try_clone().map_err(setup_io)?);
        let started = Instant::now();
        let execution = wait_for_setup(&mut process, budget, &mut timed_out);
        diagnostic["elapsed_ms"] = json!(started.elapsed().as_millis());
        if let Ok(outcome) = &execution {
            diagnostic["termination"] = json!({
                "signals": outcome.signals,
                "no_child_left_running": outcome.cleanup == ExecutionCleanup::Complete,
                "root_status": outcome.root_status.to_string(),
            });
        } else {
            diagnostic["termination"] = json!({"no_child_left_running": false});
        }
        let exit_code = execution
            .as_ref()
            .ok()
            .and_then(|outcome| outcome.root_status.code());
        let succeeded = !timed_out
            && exit_code == Some(0)
            && execution
                .as_ref()
                .is_ok_and(|outcome| outcome.cleanup == ExecutionCleanup::Complete);
        for (stream, file) in [("stdout", stdout), ("stderr", stderr)] {
            let mut log = sanitized_log(file, &redactor).map_err(setup_io)?;
            diagnostic[format!("{stream}_tail")] =
                json!(log_tail(log.as_file_mut()).map_err(setup_io)?);
            let artifact =
                store.record_artifact(&run.id, &format!("extension-setup-{stream}"), log.path())?;
            diagnostic["artifacts"][stream] = json!(artifact.id);
            if succeeded {
                // Preserve command output on success without mixing failed setup
                // logs with a structured CLI error or echoing unredacted bytes.
                let mut log = File::open(log.path()).map_err(setup_io)?;
                if stream == "stdout" {
                    let _ = io::copy(&mut log, &mut io::stdout().lock());
                } else {
                    let _ = io::copy(&mut log, &mut io::stderr().lock());
                }
            }
        }
        let outcome = execution.map_err(setup_io)?;
        outcome.into_root_status().map_err(setup_io)?;
        Ok(if timed_out {
            124
        } else {
            exit_code.unwrap_or(-1)
        })
    })();

    let (status, classification, message) = match (timed_out, &result) {
        (true, _) => (
            "timeout",
            "transient_infrastructure_timeout",
            format!("Setup command timed out after {}s", budget.as_secs()),
        ),
        (false, Ok(0)) => ("succeeded", "success", String::new()),
        (false, Ok(code)) => (
            "failed",
            "setup_command_failure",
            format!("Setup command failed with exit code {code}"),
        ),
        (false, Err(_)) => (
            "failed",
            "setup_infrastructure_failure",
            "Setup command could not be supervised or recorded".to_string(),
        ),
    };
    let retryable = timed_out
        && diagnostic.pointer("/termination/no_child_left_running") == Some(&Value::Bool(true));
    diagnostic["status"] = json!(status);
    diagnostic["classification"] = json!(classification);
    diagnostic["retryable"] = json!(retryable);
    diagnostic["exit_code"] = json!(if timed_out {
        Some(124)
    } else {
        result.as_ref().ok().copied()
    });
    if let Err(error) = &result {
        diagnostic["infrastructure_error"] = json!(redactor.text(&error.details.to_string()));
    }
    if !message.is_empty() {
        diagnostic["error"] = json!(message);
    }
    let terminal = if status == "succeeded" {
        RunStatus::Pass
    } else {
        RunStatus::Error
    };
    let finish = store.finish_run(
        &run.id,
        terminal,
        Some(json!({
            "setup": diagnostic,
            "owner_pid": std::process::id(),
        })),
    );
    let sidecar = write_sidecar(&diagnostic);
    if status == "succeeded" {
        finish?;
        sidecar?;
        return Ok(0);
    }
    if let Err(error) = finish.map(|_| ()).and(sidecar) {
        diagnostic["persistence_error"] = json!(redactor.text(&error.details.to_string()));
    }
    let code = if timed_out {
        ErrorCode::ExtensionSetupTimeout
    } else {
        ErrorCode::InternalIoError
    };
    let mut error = Error::new(
        code,
        format!("{message} (extension '{extension_id}', runtime setup)"),
        diagnostic,
    )
    .with_retryable(retryable);
    if retryable {
        error = error.with_hint(format!("Retry: {retry}"));
    }
    Err(error)
}

fn wait_for_setup(
    process: &mut Command,
    budget: Duration,
    timed_out: &mut bool,
) -> io::Result<ExecutionOutcome> {
    let started = Instant::now();
    let mut owner = ExecutionOwner::spawn(process)?;
    loop {
        if let Some(outcome) = owner.try_wait_outcome()? {
            return Ok(outcome);
        }
        let remaining = budget.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            *timed_out = true;
            return owner.drain_and_reap();
        }
        std::thread::sleep(remaining.min(Duration::from_millis(20)));
    }
}

fn write_sidecar(diagnostic: &Value) -> Result<()> {
    let Some(path) = std::env::var_os(SETUP_RESULT_FILE_ENV) else {
        return Ok(());
    };
    let path = Path::new(&path);
    let mut temp = tempfile::NamedTempFile::new_in(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )
    .map_err(setup_io)?;
    serde_json::to_writer_pretty(temp.as_file_mut(), diagnostic)
        .map_err(io::Error::other)
        .map_err(setup_io)?;
    temp.as_file().sync_all().map_err(setup_io)?;
    temp.persist(path).map_err(|error| setup_io(error.error))?;
    Ok(())
}

fn setup_io(error: io::Error) -> Error {
    Error::internal_io(
        error.to_string(),
        Some("extension runtime setup".to_string()),
    )
}

struct SetupRedactor {
    policy: RedactionPolicy,
    secrets: Vec<String>,
}

impl SetupRedactor {
    fn new() -> Self {
        let policy = RedactionPolicy::default();
        let mut secrets: Vec<_> = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .filter(|(key, value)| policy.is_credential_key_name(key) && !value.is_empty())
            .map(|(_, value)| value)
            .collect();
        secrets.sort_unstable_by_key(|value| std::cmp::Reverse(value.len()));
        Self { policy, secrets }
    }

    fn text(&self, value: &str) -> String {
        let mut value = value.to_owned();
        for secret in &self.secrets {
            value = value.replace(secret, self.policy.replacement());
        }
        self.policy.redact_embedded_urls(&value)
    }

    fn command(&self, command: &str) -> String {
        // Apply argv redaction to recognizable credential flags as well as the
        // free-text rules for shell assignments and embedded URLs.
        let words = shell::normalize_args(&[command.replace(['\n', '\t'], " ")]);
        let mut redacted = self.policy.redact_argv(&words);
        for (word, display) in words.iter().zip(&mut redacted) {
            if let Some((key, _)) = word.split_once('=').filter(|(key, _)| {
                self.policy
                    .is_credential_key_name(key.trim_start_matches('-'))
            }) {
                *display = format!("{key}={}", self.policy.replacement());
            }
        }
        if redacted == words {
            self.text(command)
        } else {
            self.text(&shell::quote_args(&redacted))
        }
    }
}

fn sanitized_log(mut raw: File, redactor: &SetupRedactor) -> io::Result<tempfile::NamedTempFile> {
    raw.rewind()?;
    let mut reader = BufReader::new(raw);
    let mut log = tempfile::Builder::new().suffix(".log").tempfile()?;
    let mut writer = BufWriter::new(log.as_file_mut());
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line)? > 0 {
        writer.write_all(redactor.text(&String::from_utf8_lossy(&line)).as_bytes())?;
        line.clear();
    }
    writer.flush()?;
    drop(writer);
    Ok(log)
}

fn log_tail(file: &mut File) -> io::Result<String> {
    file.seek(SeekFrom::Start(
        file.metadata()?.len().saturating_sub(TAIL_BYTES),
    ))?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut bytes)?;
    // Skip a UTF-8 continuation at the cut rather than growing the byte cap.
    let first = bytes
        .iter()
        .position(|byte| byte & 0xc0 != 0x80)
        .unwrap_or(bytes.len());
    Ok(String::from_utf8_lossy(&bytes[first..]).into_owned())
}
