use serde_json::Value;

use crate::cli_surface::Commands;
use crate::command_contract::CommandOutputFileMode;

use super::utils::response as output;
use super::{review, trace, GlobalArgs};

pub struct JsonCommandRun {
    pub stdout_result: homeboy::core::Result<Value>,
    pub exit_code: i32,
    pub output_file_result: Option<homeboy::core::Result<Value>>,
}

impl JsonCommandRun {
    pub fn from_stdout_result(stdout_result: homeboy::core::Result<Value>, exit_code: i32) -> Self {
        Self {
            stdout_result,
            exit_code,
            output_file_result: None,
        }
    }
}

pub fn run_and_print(
    command: Commands,
    global: &GlobalArgs,
    mode: CommandOutputFileMode,
    output_file: Option<&str>,
) -> i32 {
    let json_run = run_json(command, global, mode);

    if let Some(path) = output_file {
        write_to_file(&json_run, mode, path);
    }

    output::print_json_result(json_run.stdout_result, json_run.exit_code).ok();

    json_run.exit_code
}

pub fn run_json(
    command: Commands,
    global: &GlobalArgs,
    mode: CommandOutputFileMode,
) -> JsonCommandRun {
    match (mode, command) {
        (CommandOutputFileMode::TraceJsonSummaryArtifact, Commands::Trace(args)) => {
            let (stdout_result, exit_code, output_file_result) =
                trace::run_json_with_output_artifact(args, global);

            JsonCommandRun {
                stdout_result,
                exit_code,
                output_file_result,
            }
        }
        (_, command) => {
            let (stdout_result, exit_code) = super::json_output::run(command, global);

            JsonCommandRun::from_stdout_result(stdout_result, exit_code)
        }
    }
}

pub fn write_to_file(run: &JsonCommandRun, mode: CommandOutputFileMode, path: &str) {
    match mode {
        CommandOutputFileMode::None => {}
        CommandOutputFileMode::ReviewStableArtifact => {
            if !review::write_artifact_to_file(&run.stdout_result, path, run.exit_code) {
                output::write_json_to_file(&run.stdout_result, path, run.exit_code);
            }
        }
        CommandOutputFileMode::TraceJsonSummaryArtifact => {
            output::write_json_to_file(select_output_file_result(run, mode), path, run.exit_code);
        }
        CommandOutputFileMode::GenericEnvelope => {
            output::write_json_to_file(&run.stdout_result, path, run.exit_code);
        }
    }
}

fn select_output_file_result(
    run: &JsonCommandRun,
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
    ) -> JsonCommandRun {
        JsonCommandRun {
            stdout_result: Ok(json!({ "kind": "stdout" })),
            exit_code: 0,
            output_file_result,
        }
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
    fn generic_output_file_writes_cli_envelope() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("status.json");
        let run = run_with_output_file_result(None);

        write_to_file(
            &run,
            CommandOutputFileMode::GenericEnvelope,
            path.to_str().expect("utf8 path"),
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
        let run = JsonCommandRun::from_stdout_result(
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

        write_to_file(
            &run,
            CommandOutputFileMode::ReviewStableArtifact,
            path.to_str().expect("utf8 path"),
        );

        let written = std::fs::read_to_string(path).expect("artifact written");
        let json: Value = serde_json::from_str(&written).expect("valid json");
        assert_eq!(json["schema"], "homeboy/review/v1");
        assert!(json.get("success").is_none());
    }
}
