use serde_json::Value;

use crate::cli_surface::{CommandOutputArtifactPolicy, Commands};

use super::utils::response as output;
use super::{review, trace, GlobalArgs};

pub struct JsonCommandRun {
    pub stdout_result: homeboy::core::Result<Value>,
    pub exit_code: i32,
    pub output_file_result: Option<homeboy::core::Result<Value>>,
}

pub fn run_json(
    command: Commands,
    global: &GlobalArgs,
    policy: CommandOutputArtifactPolicy,
) -> JsonCommandRun {
    match (policy, command) {
        (CommandOutputArtifactPolicy::TraceJsonSummaryArtifact, Commands::Trace(args)) => {
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

            JsonCommandRun {
                stdout_result,
                exit_code,
                output_file_result: None,
            }
        }
    }
}

pub fn write_to_file(run: &JsonCommandRun, policy: CommandOutputArtifactPolicy, path: &str) {
    match policy {
        CommandOutputArtifactPolicy::ReviewStableArtifact => {
            if !review::write_artifact_to_file(&run.stdout_result, path, run.exit_code) {
                output::write_json_to_file(&run.stdout_result, path, run.exit_code);
            }
        }
        CommandOutputArtifactPolicy::TraceJsonSummaryArtifact => {
            output::write_json_to_file(select_output_file_result(run, policy), path, run.exit_code);
        }
        CommandOutputArtifactPolicy::GenericEnvelope => {
            output::write_json_to_file(&run.stdout_result, path, run.exit_code);
        }
    }
}

fn select_output_file_result(
    run: &JsonCommandRun,
    policy: CommandOutputArtifactPolicy,
) -> &homeboy::core::Result<Value> {
    match policy {
        CommandOutputArtifactPolicy::TraceJsonSummaryArtifact => run
            .output_file_result
            .as_ref()
            .unwrap_or(&run.stdout_result),
        CommandOutputArtifactPolicy::ReviewStableArtifact
        | CommandOutputArtifactPolicy::GenericEnvelope => &run.stdout_result,
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
            select_output_file_result(&run, CommandOutputArtifactPolicy::TraceJsonSummaryArtifact)
                .as_ref()
                .unwrap(),
            &json!({ "kind": "summary" })
        );
    }

    #[test]
    fn trace_output_file_falls_back_to_stdout_result() {
        let run = run_with_output_file_result(None);

        assert_eq!(
            select_output_file_result(&run, CommandOutputArtifactPolicy::TraceJsonSummaryArtifact)
                .as_ref()
                .unwrap(),
            &json!({ "kind": "stdout" })
        );
    }

    #[test]
    fn generic_output_file_uses_stdout_result() {
        let run = run_with_output_file_result(Some(Ok(json!({ "kind": "summary" }))));

        assert_eq!(
            select_output_file_result(&run, CommandOutputArtifactPolicy::GenericEnvelope)
                .as_ref()
                .unwrap(),
            &json!({ "kind": "stdout" })
        );
    }
}
