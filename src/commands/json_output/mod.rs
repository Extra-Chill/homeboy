use serde_json::Value;

use crate::cli_surface::Commands;
use crate::command_contract::{registered_command_dispatch_family, CommandDispatchFamily};

use super::agent_task_summary::{agent_task_summary_kind, render_agent_task_summary};
use super::output_runtime::{CommandPresentation, JsonCommandRun};
use super::{adapter, runner, GlobalArgs};

mod ops;
mod quality;
mod workspace;

type JsonRun = (homeboy::core::Result<Value>, i32);

/// Dispatch a command to its handler and map the structured result to JSON.
pub fn run(command: Commands, global: &GlobalArgs) -> (homeboy::core::Result<Value>, i32) {
    crate::commands::utils::tty::status("homeboy is working...");

    dispatch(command, global)
}

pub fn run_command_output(
    command: Commands,
    global: &GlobalArgs,
    output_file: Option<&str>,
) -> JsonCommandRun {
    crate::commands::utils::tty::status("homeboy is working...");

    match command {
        Commands::AgentTask(args) => {
            let run_from_spec_output_ref =
                agent_task_controller_run_from_spec_output_ref_eligible(&args, output_file);
            let summary_kind = agent_task_summary_kind_for_output(&args);
            let (stdout_result, exit_code) = dispatch(Commands::AgentTask(args), global);
            let summary_stdout = stdout_result.as_ref().ok().and_then(|payload| {
                if let Some(output_file) = run_from_spec_output_ref {
                    return render_controller_run_from_spec_output_ref(
                        payload,
                        exit_code,
                        output_file,
                    );
                }

                summary_kind.and_then(|kind| render_agent_task_summary(kind, payload))
            });

            JsonCommandRun::from_stdout_result(stdout_result, exit_code).with_presentation(
                CommandPresentation {
                    stdout: summary_stdout,
                    stderr: None,
                },
            )
        }
        Commands::Ci(args) => {
            let summarize = ci_triage_summary_eligible(&args);
            let (stdout_result, exit_code) = dispatch(Commands::Ci(args), global);
            let summary_stdout = summarize
                .then(|| {
                    stdout_result
                        .as_ref()
                        .ok()
                        .and_then(render_ci_triage_summary)
                })
                .flatten();

            JsonCommandRun::from_stdout_result(stdout_result, exit_code).with_presentation(
                CommandPresentation {
                    stdout: summary_stdout,
                    stderr: None,
                },
            )
        }
        Commands::Runner(args) => runner::run_command_output(args, global),
        Commands::Bench(args) => {
            let summarize = bench_summary_eligible(&args);
            let (stdout_result, exit_code) = dispatch(Commands::Bench(args), global);
            let summary_stdout = summarize
                .then(|| {
                    stdout_result
                        .as_ref()
                        .ok()
                        .and_then(super::bench_summary::render_bench_summary)
                })
                .flatten();

            JsonCommandRun::from_stdout_result(stdout_result, exit_code).with_presentation(
                CommandPresentation {
                    stdout: summary_stdout,
                    stderr: None,
                },
            )
        }
        Commands::Runs(args) => {
            let summarize = runs_show_summary_eligible(&args);
            let (stdout_result, exit_code) = dispatch(Commands::Runs(args), global);
            let summary_stdout = summarize
                .then(|| {
                    stdout_result
                        .as_ref()
                        .ok()
                        .and_then(super::runs_summary::render_runs_show_summary)
                })
                .flatten();

            JsonCommandRun::from_stdout_result(stdout_result, exit_code).with_presentation(
                CommandPresentation {
                    stdout: summary_stdout,
                    stderr: None,
                },
            )
        }
        command => {
            let (stdout_result, exit_code) = dispatch(command, global);
            JsonCommandRun::from_stdout_result(stdout_result, exit_code)
        }
    }
}

fn agent_task_controller_run_from_spec_output_ref_eligible<'a>(
    args: &crate::commands::agent_task::AgentTaskArgs,
    output_file: Option<&'a str>,
) -> Option<&'a str> {
    let output_file = output_file?;
    match &args.command {
        crate::commands::agent_task::AgentTaskCommand::Controller(controller)
            if matches!(
                &controller.command,
                crate::commands::agent_task::AgentTaskControllerCommand::RunFromSpec(_)
            ) =>
        {
            Some(output_file)
        }
        _ => None,
    }
}

fn render_controller_run_from_spec_output_ref(
    payload: &Value,
    exit_code: i32,
    output_file: &str,
) -> Option<String> {
    if payload.get("schema").and_then(Value::as_str)?
        != "homeboy/agent-task-loop-controller-run-from-spec-result/v1"
    {
        return None;
    }

    let status = payload.get("status")?;
    let controller = status.get("controller")?;
    let diagnostics_summary = status
        .get("diagnostics")
        .and_then(|diagnostics| diagnostics.get("summary"))
        .cloned()
        .unwrap_or(Value::Null);
    let terminal_outcomes = controller
        .get("terminal_outcomes")
        .and_then(Value::as_array)
        .map(|outcomes| outcomes.len())
        .unwrap_or(0);
    let evidence_refs = controller_evidence_refs(controller);

    serde_json::to_string(&serde_json::json!({
        "success": exit_code == 0,
        "data": {
            "schema": "homeboy/agent-task-loop-controller-run-from-spec-output-ref/v1",
            "result_schema": "homeboy/agent-task-loop-controller-run-from-spec-result/v1",
            "loop_id": payload.get("loop_id").cloned().unwrap_or(Value::Null),
            "stopped_reason": payload.get("stopped_reason").cloned().unwrap_or(Value::Null),
            "max_actions": payload.get("max_actions").cloned().unwrap_or(Value::Null),
            "output_file": output_file,
            "result_ref": {
                "kind": "output_file",
                "path": output_file,
                "contains": "complete_json_result"
            },
            "materialization_ref": {
                "kind": "output_file_json_pointer",
                "path": output_file,
                "pointer": "/data/materialization"
            },
            "status_ref": {
                "kind": "output_file_json_pointer",
                "path": output_file,
                "pointer": "/data/status"
            },
            "status_summary": {
                "phase": controller.get("phase").cloned().unwrap_or(Value::Null),
                "state": controller.get("state").cloned().unwrap_or(Value::Null),
                "next_action_count": controller
                    .get("next_actions")
                    .and_then(Value::as_array)
                    .map(|actions| actions.len())
                    .unwrap_or(0),
                "entity_count": controller
                    .get("entities")
                    .and_then(Value::as_object)
                    .map(|entities| entities.len())
                    .unwrap_or(0),
                "terminal_outcome_count": terminal_outcomes,
                "diagnostics": diagnostics_summary,
                "evidence_refs": evidence_refs,
            }
        }
    }))
    .ok()
    .map(|json| format!("{}\n", json))
}

fn controller_evidence_refs(controller: &Value) -> Vec<Value> {
    let mut refs = Vec::new();
    if let Some(entities) = controller.get("entities").and_then(Value::as_object) {
        for entity in entities.values() {
            for key in ["artifacts", "artifact_refs", "evidence"] {
                if let Some(items) = entity.get(key).and_then(Value::as_array) {
                    refs.extend(items.iter().take(8 - refs.len()).cloned());
                    if refs.len() >= 8 {
                        return refs;
                    }
                }
            }
        }
    }
    refs
}

/// Whether `homeboy runs show` should render the compact human summary
/// instead of the full JSON envelope (#3260). Suppressed by `--json` and in
/// lab-offload subprocesses whose stdout must remain machine-readable.
fn runs_show_summary_eligible(args: &crate::commands::runs::RunsArgs) -> bool {
    args.show_summary_eligible() && !homeboy::core::lab_routing::is_lab_offload_subprocess()
}

fn ci_triage_summary_eligible(args: &crate::commands::ci::CiArgs) -> bool {
    matches!(&args.command, crate::commands::ci::CiCommand::Triage(_))
        && !homeboy::core::lab_routing::is_lab_offload_subprocess()
}

fn render_ci_triage_summary(payload: &Value) -> Option<String> {
    payload
        .get("human_summary")
        .and_then(Value::as_str)
        .map(|summary| format!("{}\n", summary))
}

/// Whether `homeboy bench` should render the compact human summary instead
/// of dumping the full JSON envelope. The full payload is kept for `--json`,
/// for non-run subcommands, and for lab-offload subprocesses (whose stdout
/// must stay machine-readable for the parent process).
fn bench_summary_eligible(args: &crate::commands::bench::BenchArgs) -> bool {
    args.is_run_invocation()
        && !args.wants_full_json()
        && !homeboy::core::lab_routing::is_lab_offload_subprocess()
}

fn agent_task_summary_kind_for_output(
    args: &crate::commands::agent_task::AgentTaskArgs,
) -> Option<super::agent_task_summary::AgentTaskSummaryKind> {
    agent_task_summary_kind_for_output_mode(
        args,
        homeboy::core::lab_routing::is_lab_offload_subprocess(),
    )
}

fn agent_task_summary_kind_for_output_mode(
    args: &crate::commands::agent_task::AgentTaskArgs,
    lab_offload_subprocess: bool,
) -> Option<super::agent_task_summary::AgentTaskSummaryKind> {
    if lab_offload_subprocess {
        None
    } else {
        agent_task_summary_kind(args)
    }
}

fn dispatch(command: Commands, global: &GlobalArgs) -> (homeboy::core::Result<Value>, i32) {
    let command = match adapter::command_adapter(
        command,
        crate::command_contract::CommandOutputFileMode::None,
    ) {
        Ok(adapter) => return adapter.run(global),
        Err(command) => command,
    };

    match dispatch_family(&command) {
        CommandDispatchFamily::Quality => quality::dispatch(command, global),
        CommandDispatchFamily::Workspace => workspace::dispatch(command, global),
        CommandDispatchFamily::Ops => ops::dispatch(command, global),
        CommandDispatchFamily::RawOnly => {
            unsupported_raw_command("List command uses raw output mode")
        }
    }
}

fn dispatch_family(command: &Commands) -> CommandDispatchFamily {
    registered_command_dispatch_family(command.top_level_name())
        .expect("top-level command should be registered")
}

fn map<T: serde::Serialize>(result: super::CmdResult<T>) -> JsonRun {
    crate::commands::utils::response::map_cmd_result_to_json(result)
}

fn unsupported_raw_command(message: &'static str) -> JsonRun {
    let err = homeboy::core::Error::validation_invalid_argument("output_mode", message, None, None);
    crate::commands::utils::response::map_cmd_result_to_json::<Value>(Err(err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_contract::CommandDispatchFamily;
    use crate::commands::agent_task::{
        AgentTaskArgs, AgentTaskCommand, AgentTaskControllerArgs, AgentTaskControllerCommand,
        AgentTaskControllerRunFromSpecArgs, StatusArgs,
    };

    #[test]
    fn manifest_dispatches_as_json_workspace_output() {
        let (result, exit_code) = dispatch(
            Commands::Manifest(crate::commands::manifest::ManifestArgs {}),
            &GlobalArgs {},
        );

        assert_eq!(exit_code, 0);
        let value = result.expect("manifest should dispatch as JSON");
        assert_eq!(value["command"], "manifest");
        assert!(value["commands"].is_array());
    }

    #[test]
    fn lab_offload_agent_task_subprocess_keeps_json_stdout() {
        let args = AgentTaskArgs {
            command: AgentTaskCommand::Status(StatusArgs {
                run_id: "run-1".to_string(),
                full: false,
            }),
        };

        assert!(agent_task_summary_kind_for_output_mode(&args, false).is_some());
        assert!(agent_task_summary_kind_for_output_mode(&args, true).is_none());
    }

    #[test]
    fn controller_run_from_spec_with_output_file_emits_bounded_result_ref() {
        let large = "x".repeat(2 * 1024 * 1024);
        let payload = serde_json::json!({
            "schema": "homeboy/agent-task-loop-controller-run-from-spec-result/v1",
            "loop_id": "loop-large",
            "max_actions": 3,
            "stopped_reason": "terminal_state",
            "materialization": {
                "spec": { "large": large },
                "proof": { "kind": "materialization-proof" }
            },
            "from_spec": { "initialized": true },
            "results": [{ "large": large }],
            "status": {
                "controller": {
                    "phase": "running",
                    "state": "completed",
                    "next_actions": [{ "action_id": "action-1" }],
                    "entities": {
                        "entity-1": {
                            "evidence": [{ "kind": "proof", "uri": "artifact://proof" }]
                        }
                    },
                    "terminal_outcomes": [{ "outcome_id": "done" }]
                },
                "diagnostics": {
                    "summary": {
                        "pending_action_count": 0,
                        "stale_pending_action_count": 0,
                        "orphaned_pending_action_count": 0,
                        "acceptance_gate_count": 1,
                        "missing_acceptance_gate_count": 0,
                        "failed_acceptance_gate_count": 0
                    }
                }
            }
        });

        let stdout = render_controller_run_from_spec_output_ref(&payload, 0, "result.json")
            .expect("bounded output ref");
        let rendered: Value = serde_json::from_str(&stdout).expect("json stdout");

        assert!(stdout.len() < 4096, "stdout was {} bytes", stdout.len());
        assert!(!stdout.contains(&large));
        assert_eq!(rendered["success"], true);
        assert_eq!(rendered["data"]["loop_id"], "loop-large");
        assert_eq!(rendered["data"]["result_ref"]["path"], "result.json");
        assert_eq!(
            rendered["data"]["materialization_ref"]["pointer"],
            "/data/materialization"
        );
        assert_eq!(rendered["data"]["status_ref"]["pointer"], "/data/status");
        assert_eq!(rendered["data"]["status_summary"]["state"], "completed");
        assert_eq!(
            rendered["data"]["status_summary"]["evidence_refs"][0]["uri"],
            "artifact://proof"
        );
    }

    #[test]
    fn controller_run_from_spec_output_ref_requires_global_output_file() {
        let args = AgentTaskArgs {
            command: AgentTaskCommand::Controller(AgentTaskControllerArgs {
                command: AgentTaskControllerCommand::RunFromSpec(
                    AgentTaskControllerRunFromSpecArgs {
                        spec: "{}".to_string(),
                        inputs: None,
                        policy_results: Vec::new(),
                        max_actions: 1,
                        reconcile_stale: false,
                        replace: false,
                        fork: false,
                        resume_existing: false,
                        dispatch_backend: None,
                        dispatch_selector: None,
                        dispatch_model: None,
                        dispatch_provider_config: None,
                    },
                ),
            }),
        };

        assert_eq!(
            agent_task_controller_run_from_spec_output_ref_eligible(&args, Some("result.json")),
            Some("result.json")
        );
        assert_eq!(
            agent_task_controller_run_from_spec_output_ref_eligible(&args, None),
            None
        );
    }

    #[test]
    fn json_dispatch_family_comes_from_command_registry() {
        assert_eq!(
            dispatch_family(&Commands::Manifest(
                crate::commands::manifest::ManifestArgs {}
            )),
            CommandDispatchFamily::Workspace
        );
    }
}
