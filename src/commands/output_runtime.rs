use serde_json::Value;

use crate::cli_surface::{CommandOutputFileMode, Commands};

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

pub fn run_command(
    command: Commands,
    global: &GlobalArgs,
    requested_output_file: Option<&str>,
) -> i32 {
    let output_file = command_runtime_output_file(&command, requested_output_file);
    let plan = command.response_plan(output_file.is_some());

    match super::raw_output::prepare_command_run(command, global, plan.stdout) {
        super::raw_output::CommandRunPreparation::Handled(exit_code) => exit_code,
        super::raw_output::CommandRunPreparation::Json(command) => {
            let run = run_json(*command, global, plan.output_file);
            emit_run(run, plan.output_file, output_file)
        }
        super::raw_output::CommandRunPreparation::Raw(raw_run) => {
            let exit_code = raw_run.exit_code;
            let output_file_result = match raw_run.output_file_result {
                Some(result) => result,
                None => match raw_run.stdout_result.as_ref() {
                    Ok(content) => Ok(Value::String(content.clone())),
                    Err(err) => Err(err.clone()),
                },
            };
            let json_run = JsonCommandRun {
                stdout_result: output_file_result,
                exit_code,
                output_file_result: None,
            };

            write_output_file(&json_run, plan.output_file, output_file);

            match raw_run.stdout_result {
                Ok(content) => print!("{}", content),
                Err(err) => {
                    output::print_result::<Value>(Err(err)).ok();
                }
            }

            exit_code
        }
    }
}

pub fn emit_json_result(
    result: homeboy::core::Result<Value>,
    output_file: Option<&str>,
    exit_code: i32,
) {
    let run = JsonCommandRun::from_stdout_result(result, exit_code);
    write_output_file(&run, CommandOutputFileMode::GenericEnvelope, output_file);
    output::print_json_result(run.stdout_result, run.exit_code).ok();
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

fn emit_run(run: JsonCommandRun, mode: CommandOutputFileMode, output_file: Option<&str>) -> i32 {
    write_output_file(&run, mode, output_file);
    output::print_json_result(run.stdout_result, run.exit_code).ok();

    run.exit_code
}

pub fn write_output_file(run: &JsonCommandRun, mode: CommandOutputFileMode, path: Option<&str>) {
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
            output::write_json_to_file(select_output_file_result(run, mode), path, run.exit_code);
        }
        CommandOutputFileMode::GenericEnvelope => {
            output::write_json_to_file(&run.stdout_result, path, run.exit_code);
        }
    }
}

pub fn select_output_file_result(
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
