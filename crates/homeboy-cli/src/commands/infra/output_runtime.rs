use serde_json::Value;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::cli_surface::Commands;
use crate::command_contract::CommandOutputFileMode;

use crate::commands::utils::response::{self as output, CommandIdentity};
use crate::commands::{review, trace};

#[derive(Debug)]
pub(crate) struct CookOutputLease {
    path: PathBuf,
    lock_path: PathBuf,
    token: String,
    lock: std::fs::File,
}

impl CookOutputLease {
    pub(crate) fn claim(path: &str) -> homeboy::core::Result<Self> {
        let path = PathBuf::from(path);
        let lock_path = output_lock_path(&path);
        if !claim_local_lock(&lock_path) {
            return Err(output_contended_error(&path));
        }
        let token = uuid::Uuid::new_v4().to_string();
        let lock = match claim_lock(&lock_path, &path, &token) {
            Ok(lock) => lock,
            Err(error) => {
                release_local_lock(&lock_path);
                return Err(error);
            }
        };
        let lease = Self {
            path,
            lock_path,
            token,
            lock,
        };
        lease.write_in_flight("preparing", None, None)?;
        Ok(lease)
    }

    pub(crate) fn progress(
        &self,
        phase: &str,
        cook_id: Option<&str>,
        run_id: Option<&str>,
    ) -> homeboy::core::Result<()> {
        self.write_in_flight(phase, cook_id, run_id)
    }

    pub(crate) fn finish(
        &self,
        result: &homeboy::core::Result<Value>,
        exit_code: i32,
        identity: &CommandIdentity,
        presentation: Option<output::CommandPresentationEnvelope>,
    ) -> homeboy::core::Result<()> {
        let response = output::cli_response_for_json_result_for_identity(
            result,
            exit_code,
            identity,
            presentation,
        );
        let contents = serde_json::to_string_pretty(&response).map_err(|error| {
            homeboy::core::Error::internal_json(
                error.to_string(),
                Some("serialize Cook output".to_string()),
            )
        })?;
        self.write(&contents)
    }

    fn write_in_flight(
        &self,
        phase: &str,
        cook_id: Option<&str>,
        run_id: Option<&str>,
    ) -> homeboy::core::Result<()> {
        let mut value = serde_json::json!({
            "schema": "homeboy/agent-task-cook-output/v1",
            "command": "agent-task cook",
            "success": false,
            "exit_code": null,
            "status": if run_id.is_some() { "in_flight" } else { "preparing" },
            "invocation_id": self.token,
            "updated_at": chrono::Utc::now().to_rfc3339(),
            "phase": phase,
        });
        if let (Some(cook_id), Some(run_id)) = (cook_id, run_id) {
            let object = value.as_object_mut().expect("Cook output envelope object");
            object.insert("cook_id".to_string(), serde_json::json!(cook_id));
            object.insert(
                "run".to_string(),
                serde_json::json!({ "id": run_id, "kind": "agent-task" }),
            );
            object.insert(
                "recovery".to_string(),
                serde_json::json!({
                    "status": format!("homeboy agent-task status {run_id}"),
                    "logs": format!("homeboy agent-task logs {run_id}"),
                    "resume": format!("homeboy agent-task resume {cook_id}"),
                }),
            );
        }
        let contents = serde_json::to_string_pretty(&value).map_err(|error| {
            homeboy::core::Error::internal_json(
                error.to_string(),
                Some("serialize Cook in-flight output".to_string()),
            )
        })?;
        self.write(&contents)
    }

    fn write(&self, contents: &str) -> homeboy::core::Result<()> {
        homeboy::core::io::write_output_file_atomically(
            &self.path,
            contents,
            homeboy::core::io::OutputWriteOptions::json_output(),
        )
        .map_err(|error| output_io_error(&self.path, error))
    }
}

impl Drop for CookOutputLease {
    fn drop(&mut self) {
        // The advisory lock makes the read/compare/unlink sequence exclusive.
        // A waiter that opened this inode rechecks the pathname after locking, so
        // it cannot resurrect an unlinked stale lease beside a new owner.
        #[cfg(unix)]
        if lock_matches(&self.lock, &self.token) && path_matches_file(&self.lock_path, &self.lock) {
            let _ = std::fs::remove_file(&self.lock_path);
        }
        let _ = unlock_file(&self.lock);
        release_local_lock(&self.lock_path);
    }
}

fn output_lock_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("output");
    path.with_file_name(format!(".{name}.homeboy-cook-output.lock"))
}

fn claim_lock(
    lock_path: &Path,
    output_path: &Path,
    token: &str,
) -> homeboy::core::Result<std::fs::File> {
    for _ in 0..3 {
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(lock_path)
                .map_err(|error| output_io_error(lock_path, error))?,
            Err(error) => return Err(output_io_error(lock_path, error)),
        };
        match lock_file(&file) {
            Ok(()) => {
                // An owner may unlink while this waiter blocks. Never take an
                // unlinked inode; retry against the current path instead.
                #[cfg(unix)]
                if !path_matches_file(lock_path, &file) {
                    let _ = unlock_file(&file);
                    continue;
                }
                write_lock_record(&file, token)?;
                return Ok(file);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                return Err(output_contended_error(output_path))
            }
            Err(error) => return Err(output_io_error(lock_path, error)),
        }
    }
    Err(homeboy::core::Error::internal_unexpected(
        "Cook output ownership changed while reclaiming its stale lock",
    ))
}

fn local_locks() -> &'static Mutex<HashSet<PathBuf>> {
    static LOCKS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn claim_local_lock(path: &Path) -> bool {
    local_locks()
        .lock()
        .expect("Cook output lock registry")
        .insert(path.to_path_buf())
}

fn release_local_lock(path: &Path) {
    local_locks()
        .lock()
        .expect("Cook output lock registry")
        .remove(path);
}

fn output_contended_error(path: &Path) -> homeboy::core::Error {
    homeboy::core::Error::validation_invalid_argument(
        "output",
        format!(
            "Cook output `{}` is owned by another active invocation; choose a different --output path or wait for it to finish",
            path.display()
        ),
        None,
        None,
    )
}

fn write_lock_record(file: &std::fs::File, token: &str) -> homeboy::core::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    let mut file = file;
    file.set_len(0)
        .map_err(|error| output_io_error(Path::new("Cook output lock"), error))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| output_io_error(Path::new("Cook output lock"), error))?;
    let record = serde_json::json!({
        "pid": std::process::id(),
        "token": token,
    });
    file.write_all(record.to_string().as_bytes())
        .and_then(|_| file.sync_all())
        .map_err(|error| output_io_error(Path::new("Cook output lock"), error))
}

fn lock_matches(file: &std::fs::File, token: &str) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = file;
    let mut contents = String::new();
    file.seek(SeekFrom::Start(0)).is_ok()
        && file.read_to_string(&mut contents).is_ok()
        && serde_json::from_str::<Value>(&contents)
            .ok()
            .and_then(|value| value["token"].as_str().map(str::to_string))
            == Some(token.to_string())
}

#[cfg(unix)]
fn path_matches_file(path: &Path, file: &std::fs::File) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(path) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(file) = file.metadata() else {
        return false;
    };
    path.dev() == file.dev() && path.ino() == file.ino()
}

fn lock_file(file: &std::fs::File) -> std::io::Result<()> {
    use fs4::fs_std::FileExt;
    file.try_lock_exclusive().map(|_| ())
}

fn unlock_file(file: &std::fs::File) -> std::io::Result<()> {
    use fs4::fs_std::FileExt;
    FileExt::unlock(file)
}

fn output_io_error(path: &Path, error: std::io::Error) -> homeboy::core::Error {
    homeboy::core::Error::internal_io(
        error.to_string(),
        Some(format!("write Cook output {}", path.display())),
    )
}

pub struct CommandRun {
    pub command: String,
    pub operation: Option<String>,
    pub stdout_result: homeboy::core::Result<Value>,
    pub exit_code: i32,
    pub output_file_result: Option<homeboy::core::Result<Value>>,
    pub presentation: CommandPresentation,
    pub raw_stdout: Option<homeboy::core::Result<String>>,
    output_file_already_written: bool,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CommandPresentation {
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

impl CommandRun {
    pub fn from_stdout_result(stdout_result: homeboy::core::Result<Value>, exit_code: i32) -> Self {
        Self::from_command_stdout_result("unknown", stdout_result, exit_code)
    }

    pub fn from_command_stdout_result(
        command: impl Into<String>,
        stdout_result: homeboy::core::Result<Value>,
        exit_code: i32,
    ) -> Self {
        Self {
            command: command.into(),
            operation: None,
            stdout_result,
            exit_code,
            output_file_result: None,
            presentation: CommandPresentation::default(),
            raw_stdout: None,
            output_file_already_written: false,
        }
    }

    pub fn with_presentation(mut self, presentation: CommandPresentation) -> Self {
        self.presentation = presentation;
        self
    }

    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = command.into();
        self
    }

    pub fn with_identity(mut self, identity: &CommandIdentity) -> Self {
        self.command = identity.command.clone();
        self.operation = identity.operation.clone();
        self
    }

    pub fn with_output_file_already_written(mut self) -> Self {
        self.output_file_already_written = true;
        self
    }

    pub fn from_raw_stdout(
        command: impl Into<String>,
        raw_stdout: homeboy::core::Result<String>,
        exit_code: i32,
        output_file_result: Option<homeboy::core::Result<Value>>,
    ) -> Self {
        let stdout_result = match output_file_result.clone() {
            Some(result) => result,
            None => match raw_stdout.as_ref() {
                Ok(content) => Ok(Value::String(content.clone())),
                Err(err) => Err(err.clone()),
            },
        };

        Self {
            command: command.into(),
            operation: None,
            stdout_result,
            exit_code,
            output_file_result,
            presentation: CommandPresentation::default(),
            raw_stdout: Some(raw_stdout),
            output_file_already_written: false,
        }
    }

    pub(crate) fn output_file_result(
        &self,
        mode: CommandOutputFileMode,
    ) -> &homeboy::core::Result<Value> {
        match mode {
            CommandOutputFileMode::TraceJsonSummaryArtifact => self
                .output_file_result
                .as_ref()
                .unwrap_or(&self.stdout_result),
            _ => &self.stdout_result,
        }
    }
}

pub struct OutputService<'a> {
    output_file: Option<&'a str>,
}

impl<'a> OutputService<'a> {
    pub fn new(output_file: Option<&'a str>) -> Self {
        Self { output_file }
    }

    pub fn emit_json_result(&self, result: homeboy::core::Result<Value>, exit_code: i32) {
        self.emit_json_result_for_identity(
            result,
            exit_code,
            &CommandIdentity::top_level("unknown"),
        );
    }

    pub fn emit_json_result_for_identity(
        &self,
        result: homeboy::core::Result<Value>,
        exit_code: i32,
        identity: &CommandIdentity,
    ) {
        self.emit_run(
            CommandRun::from_stdout_result(result, exit_code).with_identity(identity),
            CommandOutputFileMode::GenericEnvelope,
        );
    }

    pub fn emit_run(&self, run: CommandRun, mode: CommandOutputFileMode) -> i32 {
        self.write_output_file(&run, mode);
        if let Some(raw_stdout) = run.raw_stdout {
            match raw_stdout {
                Ok(content) => print!("{}", content),
                Err(err) => {
                    output::print_json_result_for_identity(
                        Err(err),
                        run.exit_code,
                        &CommandIdentity {
                            command: run.command.clone(),
                            operation: run.operation.clone(),
                        },
                        None,
                    )
                    .ok();
                }
            }

            return run.exit_code;
        }

        if let Some(stderr) = &run.presentation.stderr {
            eprint!("{}", stderr);
        }
        output::print_json_result_for_identity(
            run.stdout_result,
            run.exit_code,
            &CommandIdentity {
                command: run.command.clone(),
                operation: run.operation.clone(),
            },
            presentation_envelope(run.presentation),
        )
        .ok();

        run.exit_code
    }

    pub fn write_output_file(&self, run: &CommandRun, mode: CommandOutputFileMode) {
        if !run.output_file_already_written {
            write_output_file(run, mode, self.output_file);
        }
    }
}

pub fn run_command(
    command: Commands,
    spec: &'static crate::command_contract::CommandSpec,
    requested_output_file: Option<&str>,
    identity: &CommandIdentity,
) -> i32 {
    let output_file = command_runtime_output_file(&command, requested_output_file);
    let plan = command.response_plan(spec, output_file.is_some());
    let output_service = OutputService::new(output_file);

    let run = match crate::commands::raw_output::prepare_command_run(command, plan.stdout) {
        crate::commands::raw_output::CommandRunPreparation::Handled(exit_code) => return exit_code,
        crate::commands::raw_output::CommandRunPreparation::Json(command) => {
            return output_service.emit_run(
                run_json(*command, spec, plan.output_file, output_file).with_identity(identity),
                plan.output_file,
            );
        }
        crate::commands::raw_output::CommandRunPreparation::Raw(run) => run,
    };
    output_service.emit_run(run.with_identity(identity), plan.output_file)
}

pub fn emit_json_result(
    result: homeboy::core::Result<Value>,
    output_file: Option<&str>,
    exit_code: i32,
) {
    OutputService::new(output_file).emit_json_result(result, exit_code);
}

pub fn emit_json_result_for_identity(
    result: homeboy::core::Result<Value>,
    output_file: Option<&str>,
    exit_code: i32,
    identity: &CommandIdentity,
) {
    OutputService::new(output_file).emit_json_result_for_identity(result, exit_code, identity);
}

pub fn validate_output_file_path(path: &str) -> Option<homeboy::core::Error> {
    let value = path.trim();
    let looks_like_format = matches!(
        value.to_ascii_lowercase().as_str(),
        "json" | "yaml" | "yml" | "table" | "csv" | "text" | "markdown" | "md"
    );

    if !looks_like_format {
        return None;
    }

    Some(homeboy::core::Error::validation_invalid_argument(
        "output",
        format!(
            "`--output {value}` looks like an output format, but --output writes to a file path"
        ),
        None,
        Some(vec![
            "Use an explicit file path, for example: --output ./homeboy-output.json".to_string(),
            "Use command-specific --format flags where available, for example: --format=json"
                .to_string(),
        ]),
    ))
}

pub fn command_runtime_output_file<'a>(
    command: &Commands,
    requested_output_file: Option<&'a str>,
) -> Option<&'a str> {
    if command.consumes_output_file_as_command_arg() {
        None
    } else {
        requested_output_file
    }
}

pub fn run_json(
    command: Commands,
    spec: &crate::command_contract::CommandSpec,
    mode: CommandOutputFileMode,
    output_file: Option<&str>,
) -> CommandRun {
    match (mode, command) {
        (CommandOutputFileMode::TraceJsonSummaryArtifact, Commands::Trace(args)) => {
            let (stdout_result, exit_code, output_file_result) =
                trace::run_json_with_output_artifact(args);

            CommandRun {
                command: "trace".to_string(),
                operation: None,
                stdout_result,
                exit_code,
                output_file_result,
                presentation: CommandPresentation::default(),
                raw_stdout: None,
                output_file_already_written: false,
            }
        }
        (_, command) => {
            crate::commands::json_output::run_command_output(command, spec, output_file)
        }
    }
}

pub fn write_output_file(run: &CommandRun, mode: CommandOutputFileMode, path: Option<&str>) {
    let Some(path) = path else {
        return;
    };

    match mode {
        CommandOutputFileMode::None => {}
        CommandOutputFileMode::ReviewStableArtifact => {
            if !review::write_artifact_to_file(&run.stdout_result, path, run.exit_code) {
                output::write_json_to_file(&run.stdout_result, path, run.exit_code);
            }
        }
        CommandOutputFileMode::TraceJsonSummaryArtifact
        | CommandOutputFileMode::GenericEnvelope => {
            output::write_json_to_file_for_identity(
                run.output_file_result(mode),
                path,
                run.exit_code,
                &CommandIdentity {
                    command: run.command.clone(),
                    operation: run.operation.clone(),
                },
                presentation_envelope(run.presentation.clone()),
            );
        }
    }
}

fn presentation_envelope(
    presentation: CommandPresentation,
) -> Option<output::CommandPresentationEnvelope> {
    if presentation.stdout.is_none() && presentation.stderr.is_none() {
        return None;
    }

    Some(output::CommandPresentationEnvelope {
        stdout: presentation.stdout,
        stderr: presentation.stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run_with_output_file_result(
        output_file_result: Option<homeboy::core::Result<Value>>,
    ) -> CommandRun {
        CommandRun {
            command: "test".to_string(),
            operation: None,
            stdout_result: Ok(json!({ "kind": "stdout" })),
            exit_code: 0,
            output_file_result,
            presentation: CommandPresentation::default(),
            raw_stdout: None,
            output_file_already_written: false,
        }
    }

    #[test]
    fn raw_command_run_without_artifact_uses_raw_stdout_for_file_payload() {
        let run = CommandRun::from_raw_stdout("test", Ok("plain output".to_string()), 0, None);

        assert_eq!(run.raw_stdout.unwrap().unwrap(), "plain output");
        assert_eq!(run.stdout_result.unwrap(), json!("plain output"));
    }

    #[test]
    fn raw_command_run_with_artifact_uses_artifact_for_file_payload() {
        let run = CommandRun::from_raw_stdout(
            "test",
            Ok("markdown output".to_string()),
            0,
            Some(Ok(json!({ "artifact": true }))),
        );

        assert_eq!(run.raw_stdout.unwrap().unwrap(), "markdown output");
        assert_eq!(run.stdout_result.unwrap(), json!({ "artifact": true }));
    }

    #[test]
    fn trace_output_file_prefers_summary_artifact_result() {
        let run = run_with_output_file_result(Some(Ok(json!({ "kind": "summary" }))));

        assert_eq!(
            run.output_file_result(CommandOutputFileMode::TraceJsonSummaryArtifact)
                .as_ref()
                .unwrap(),
            &json!({ "kind": "summary" })
        );
    }

    #[test]
    fn trace_output_file_falls_back_to_stdout_result() {
        let run = run_with_output_file_result(None);

        assert_eq!(
            run.output_file_result(CommandOutputFileMode::TraceJsonSummaryArtifact)
                .as_ref()
                .unwrap(),
            &json!({ "kind": "stdout" })
        );
    }

    #[test]
    fn generic_output_file_uses_stdout_result() {
        let run = run_with_output_file_result(Some(Ok(json!({ "kind": "summary" }))));

        assert_eq!(
            run.output_file_result(CommandOutputFileMode::GenericEnvelope)
                .as_ref()
                .unwrap(),
            &json!({ "kind": "stdout" })
        );
    }

    #[test]
    fn presentation_does_not_replace_structured_stdout_or_file_payload() {
        let run = CommandRun::from_stdout_result(Ok(json!({ "kind": "stdout" })), 0)
            .with_presentation(CommandPresentation {
                stdout: Some("short summary\n".to_string()),
                stderr: Some("progress\n".to_string()),
            });

        assert_eq!(run.presentation.stdout.as_deref(), Some("short summary\n"));
        assert_eq!(
            run.output_file_result(CommandOutputFileMode::GenericEnvelope)
                .as_ref()
                .unwrap(),
            &json!({ "kind": "stdout" })
        );
    }

    #[test]
    fn generic_output_file_keeps_complete_large_payload_with_compact_presentation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("controller-result.json");
        let large = "x".repeat(2 * 1024 * 1024);
        let run = CommandRun::from_stdout_result(
            Ok(json!({
                "schema": "homeboy/agent-task-loop-controller-run-from-spec-result/v1",
                "loop_id": "large-loop",
                "results": [{ "payload": large }]
            })),
            0,
        )
        .with_presentation(CommandPresentation {
            stdout: Some("{\"success\":true,\"data\":{\"loop_id\":\"large-loop\"}}\n".to_string()),
            stderr: None,
        });

        assert!(run.presentation.stdout.as_ref().expect("stdout").len() < 256);

        write_output_file(
            &run,
            CommandOutputFileMode::GenericEnvelope,
            Some(path.to_str().expect("utf8 path")),
        );

        let written = std::fs::read_to_string(path).expect("artifact written");
        let json: Value = serde_json::from_str(&written).expect("valid json");
        assert_eq!(json["success"], true);
        assert_eq!(json["data"]["loop_id"], "large-loop");
        assert_eq!(
            json["data"]["results"][0]["payload"]
                .as_str()
                .unwrap()
                .len(),
            2 * 1024 * 1024
        );
    }

    #[test]
    fn generic_output_file_writes_cli_envelope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("status.json");
        let run = run_with_output_file_result(None);

        write_output_file(
            &run,
            CommandOutputFileMode::GenericEnvelope,
            Some(path.to_str().expect("utf8 path")),
        );

        let written = std::fs::read_to_string(path).expect("artifact written");
        let json: Value = serde_json::from_str(&written).expect("valid json");
        assert_eq!(json["success"], true);
        assert_eq!(json["data"], json!({ "kind": "stdout" }));
    }

    #[test]
    fn generic_output_file_keeps_failed_test_result_available() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.json");
        let run = CommandRun::from_stdout_result(
            Ok(json!({
                "passed": false,
                "status": "failed",
                "test_counts": { "total": 36, "passed": 21, "failed": 15, "skipped": 0 }
            })),
            1,
        );

        write_output_file(
            &run,
            CommandOutputFileMode::GenericEnvelope,
            Some(path.to_str().expect("utf8 path")),
        );

        let written = std::fs::read_to_string(path).expect("failed result written");
        let json: Value = serde_json::from_str(&written).expect("valid json");
        assert_eq!(json["success"], false);
        assert_eq!(json["exit_code"], 1);
        assert_eq!(json["data"]["test_counts"]["failed"], 15);
    }

    #[test]
    fn cook_output_replaces_stale_terminal_result_with_current_run_then_terminal_result() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cook.json");
        std::fs::write(&path, r#"{"status":"failed","run":{"id":"old-run"}}"#)
            .expect("seed stale terminal result");

        let lease =
            CookOutputLease::claim(path.to_str().expect("utf8 path")).expect("claim output");
        lease
            .progress("in_flight", Some("cook-current"), Some("run-current"))
            .expect("record current durable run");
        let in_flight: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(in_flight["status"], "in_flight");
        assert_eq!(in_flight["phase"], "in_flight");
        assert_eq!(in_flight["cook_id"], "cook-current");
        assert_eq!(in_flight["run"]["id"], "run-current");
        assert!(in_flight["recovery"]["status"]
            .as_str()
            .unwrap()
            .contains("run-current"));

        lease
            .finish(
                &Ok(json!({ "status": "green_no_finalize", "latest_run_id": "run-current" })),
                0,
                &CommandIdentity::with_operation("agent-task", "cook"),
                None,
            )
            .expect("write terminal envelope");
        let terminal: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(terminal["schema"], "homeboy/command-result/v3");
        assert_eq!(terminal["success"], true);
        assert_eq!(terminal["data"]["latest_run_id"], "run-current");
    }

    #[test]
    fn preparing_output_has_no_durable_recovery_identity_before_recipe_persists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cook.json");
        let lease = CookOutputLease::claim(path.to_str().unwrap()).expect("claim output");

        let preparing: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(preparing["phase"], "preparing");
        assert_eq!(preparing["status"], "preparing");
        assert!(preparing.get("run").is_none());
        assert!(preparing.get("cook_id").is_none());
        assert!(preparing.get("recovery").is_none());

        lease
            .progress("in_flight", Some("cook-durable"), Some("run-durable"))
            .unwrap();
        let durable: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(durable["status"], "in_flight");
        assert_eq!(durable["run"]["id"], "run-durable");
    }

    #[test]
    fn cook_output_concurrent_writer_fails_closed_until_owner_releases_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cook.json");
        let first = CookOutputLease::claim(path.to_str().expect("utf8 path")).expect("first claim");

        let error = CookOutputLease::claim(path.to_str().expect("utf8 path"))
            .expect_err("second writer must not replace active output");
        assert!(error.message.contains("owned by another active invocation"));
        drop(first);

        CookOutputLease::claim(path.to_str().expect("utf8 path"))
            .expect("released output can be claimed by a later invocation");
    }

    #[test]
    fn rejected_writer_leaves_active_in_flight_output_byte_identical() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cook.json");
        let lease = CookOutputLease::claim(path.to_str().expect("utf8 path")).expect("claim");
        lease
            .progress("in_flight", Some("cook-a"), Some("run-a"))
            .unwrap();
        let before = std::fs::read(&path).expect("read active output");

        assert!(CookOutputLease::claim(path.to_str().expect("utf8 path")).is_err());
        let rejected = CommandRun::from_stdout_result(
            Err(homeboy::core::Error::validation_invalid_argument(
                "output",
                "contended",
                None,
                None,
            )),
            2,
        )
        .with_output_file_already_written();
        OutputService::new(Some(path.to_str().unwrap()))
            .write_output_file(&rejected, CommandOutputFileMode::GenericEnvelope);

        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn reclaimed_lock_replaces_reused_pid_record_with_a_new_nonce() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cook.json");
        let lock = output_lock_path(&path);
        std::fs::write(
            &lock,
            serde_json::json!({
                "pid": std::process::id(),
                "token": "old-owner"
            })
            .to_string(),
        )
        .unwrap();

        let lease = CookOutputLease::claim(path.to_str().unwrap()).expect("reclaim stale record");
        let record: Value = serde_json::from_str(&std::fs::read_to_string(&lock).unwrap()).unwrap();
        assert_eq!(record["pid"], std::process::id());
        assert_ne!(record["token"], "old-owner");
        assert!(record.get("process_start").is_none());
        drop(lease);
    }

    #[test]
    fn concurrent_reclaimers_admit_only_one_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cook.json");
        let lock = output_lock_path(&path);
        std::fs::write(&lock, "interrupted").unwrap();
        let (claimed_tx, claimed_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let path_for_owner = path.clone();
        let owner = std::thread::spawn(move || {
            let lease =
                CookOutputLease::claim(path_for_owner.to_str().unwrap()).expect("first reclaimer");
            claimed_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(lease);
        });
        claimed_rx.recv().unwrap();
        assert!(CookOutputLease::claim(path.to_str().unwrap()).is_err());
        release_tx.send(()).unwrap();
        owner.join().unwrap();
    }

    #[test]
    fn cook_output_reclaims_stale_lock_after_interruption() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cook.json");
        let lock = output_lock_path(&path);
        // An interrupted lock write has no trustworthy live owner and must not
        // permanently strand a reusable output path.
        std::fs::write(&lock, "\n").expect("seed interrupted lock");

        let lease = CookOutputLease::claim(path.to_str().expect("utf8 path"))
            .expect("reclaim dead owner lock");
        let output: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(output["status"], "preparing");
        drop(lease);
        assert!(!lock.exists());
    }

    #[test]
    fn review_output_file_writes_stable_artifact_without_envelope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("review.json");
        let run = CommandRun::from_stdout_result(
            Ok(json!({
                "command": "review",
                "artifact": {
                    "schema": "homeboy/review/v1",
                    "status": "passed",
                    "commands": []
                }
            })),
            0,
        );

        write_output_file(
            &run,
            CommandOutputFileMode::ReviewStableArtifact,
            Some(path.to_str().expect("utf8 path")),
        );

        let written = std::fs::read_to_string(path).expect("artifact written");
        let json: Value = serde_json::from_str(&written).expect("valid json");
        assert_eq!(json["schema"], "homeboy/review/v1");
        assert!(json.get("success").is_none());
    }
}
