use serde_json::Value;

use crate::cli_surface::Commands;
use crate::command_contract::CommandOutputFileMode;

use crate::commands::utils::response as output;
use crate::commands::{review, trace, GlobalArgs};

pub struct CommandRun {
    pub command: String,
    pub stdout_result: homeboy::core::Result<Value>,
    pub exit_code: i32,
    pub output_file_result: Option<homeboy::core::Result<Value>>,
    pub presentation: CommandPresentation,
    pub raw_stdout: Option<homeboy::core::Result<String>>,
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
            stdout_result,
            exit_code,
            output_file_result: None,
            presentation: CommandPresentation::default(),
            raw_stdout: None,
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
            stdout_result,
            exit_code,
            output_file_result,
            presentation: CommandPresentation::default(),
            raw_stdout: Some(raw_stdout),
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
        let run = CommandRun::from_stdout_result(result, exit_code);
        self.write_output_file(&run, CommandOutputFileMode::GenericEnvelope);
        output::print_json_result_for_command(
            run.stdout_result,
            run.exit_code,
            &run.command,
            presentation_envelope(run.presentation),
        )
        .ok();
    }

    pub fn emit_run(&self, run: CommandRun, mode: CommandOutputFileMode) -> i32 {
        self.write_output_file(&run, mode);
        if let Some(raw_stdout) = run.raw_stdout {
            match raw_stdout {
                Ok(content) => print!("{}", content),
                Err(err) => {
                    output::print_json_result_for_command(
                        Err(err),
                        run.exit_code,
                        &run.command,
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
        output::print_json_result_for_command(
            run.stdout_result,
            run.exit_code,
            &run.command,
            presentation_envelope(run.presentation),
        )
        .ok();

        run.exit_code
    }

    pub fn write_output_file(&self, run: &CommandRun, mode: CommandOutputFileMode) {
        write_output_file(run, mode, self.output_file);
    }
}

pub fn run_command(
    command: Commands,
    global: &GlobalArgs,
    requested_output_file: Option<&str>,
) -> i32 {
    let output_file = command_runtime_output_file(&command, requested_output_file);
    let plan = command.response_plan(output_file.is_some());
    let output_service = OutputService::new(output_file);

    match crate::commands::raw_output::prepare_command_run(command, global, plan.stdout) {
        crate::commands::raw_output::CommandRunPreparation::Handled(exit_code) => exit_code,
        crate::commands::raw_output::CommandRunPreparation::Json(command) => {
            let run = run_json(*command, global, plan.output_file, output_file);
            output_service.emit_run(run, plan.output_file)
        }
        crate::commands::raw_output::CommandRunPreparation::Raw(run) => {
            output_service.emit_run(run, plan.output_file)
        }
    }
}

pub fn emit_json_result(
    result: homeboy::core::Result<Value>,
    output_file: Option<&str>,
    exit_code: i32,
) {
    OutputService::new(output_file).emit_json_result(result, exit_code);
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
    global: &GlobalArgs,
    mode: CommandOutputFileMode,
    output_file: Option<&str>,
) -> CommandRun {
    match (mode, command) {
        (CommandOutputFileMode::TraceJsonSummaryArtifact, Commands::Trace(args)) => {
            let (stdout_result, exit_code, output_file_result) =
                trace::run_json_with_output_artifact(args, global);

            CommandRun {
                command: "trace".to_string(),
                stdout_result,
                exit_code,
                output_file_result,
                presentation: CommandPresentation::default(),
                raw_stdout: None,
            }
        }
        (_, command) => {
            crate::commands::json_output::run_command_output(command, global, output_file)
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
        CommandOutputFileMode::TraceJsonSummaryArtifact => {
            output::write_json_to_file_for_command(
                select_output_file_result(run, mode),
                path,
                run.exit_code,
                &run.command,
                presentation_envelope(run.presentation.clone()),
            );
        }
        CommandOutputFileMode::GenericEnvelope => {
            output::write_json_to_file_for_command(
                &run.stdout_result,
                path,
                run.exit_code,
                &run.command,
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

pub fn select_output_file_result(
    run: &CommandRun,
    mode: CommandOutputFileMode,
) -> &homeboy::core::Result<Value> {
    match mode {
        CommandOutputFileMode::TraceJsonSummaryArtifact => run
            .output_file_result
            .as_ref()
            .unwrap_or(&run.stdout_result),
        CommandOutputFileMode::None
        | CommandOutputFileMode::ReviewStableArtifact
        | CommandOutputFileMode::GenericEnvelope => &run.stdout_result,
    }
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
            stdout_result: Ok(json!({ "kind": "stdout" })),
            exit_code: 0,
            output_file_result,
            presentation: CommandPresentation::default(),
            raw_stdout: None,
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
            select_output_file_result(&run, CommandOutputFileMode::TraceJsonSummaryArtifact)
                .as_ref()
                .unwrap(),
            &json!({ "kind": "summary" })
        );
    }

    #[test]
    fn trace_output_file_falls_back_to_stdout_result() {
        let run = run_with_output_file_result(None);

        assert_eq!(
            select_output_file_result(&run, CommandOutputFileMode::TraceJsonSummaryArtifact)
                .as_ref()
                .unwrap(),
            &json!({ "kind": "stdout" })
        );
    }

    #[test]
    fn generic_output_file_uses_stdout_result() {
        let run = run_with_output_file_result(Some(Ok(json!({ "kind": "summary" }))));

        assert_eq!(
            select_output_file_result(&run, CommandOutputFileMode::GenericEnvelope)
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
            select_output_file_result(&run, CommandOutputFileMode::GenericEnvelope)
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
