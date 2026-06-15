//! Bridges between Lab offload execution and the local AgentTask lifecycle.
//!
//! - Inline `--plan` JSON arguments are materialized to a synced workspace file
//!   so the runner can resolve them remotely (see
//!   `materialize_inline_agent_task_plan_arg`).
//! - Once the runner streams output back, `mirror_agent_task_run_plan_lifecycle`
//!   replays the run-plan aggregate into the controller's lifecycle store so
//!   `homeboy agent-task status/logs` keeps working transparently.
//! - The dispatch-envelope parsers below let the offload orchestrator recover
//!   structured failure metadata from mixed stdout/stderr streams.

use std::fs;

use crate::core::agent_tasks::lifecycle as agent_task_lifecycle;
use crate::core::agent_tasks::scheduler::{AgentTaskAggregate, AgentTaskPlan};
use crate::core::{config, Error, Result};

use super::super::lab_workspaces::{workspace_mapping_entry, LabWorkspaceMappingEntry};
use super::super::{sync_workspace, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions};
use super::args_util::subcommand_index;

pub(super) fn materialize_inline_agent_task_plan_arg(
    runner_id: &str,
    args: &[String],
) -> Result<(Vec<String>, Option<LabWorkspaceMappingEntry>)> {
    if subcommand_index(args, "agent-task")
        .and_then(|index| {
            args.get(index + 1)
                .filter(|arg| arg.as_str() == "run-plan")
                .map(|_| index + 1)
        })
        .is_none()
    {
        return Ok((args.to_vec(), None));
    }

    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;

    while let Some(arg) = iter.next() {
        if passthrough {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            out.push(arg.clone());
            continue;
        }
        if arg == "--plan" {
            out.push(arg.clone());
            if let Some(spec) = iter.next() {
                if let Some((remapped_spec, entry)) = sync_inline_agent_task_plan(runner_id, spec)?
                {
                    out.push(remapped_spec);
                    out.extend(iter.cloned());
                    return Ok((out, Some(entry)));
                }
                out.push(spec.clone());
            }
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--plan=") {
            if let Some((remapped_spec, entry)) = sync_inline_agent_task_plan(runner_id, spec)? {
                out.push(format!("--plan={remapped_spec}"));
                out.extend(iter.cloned());
                return Ok((out, Some(entry)));
            }
        }
        out.push(arg.clone());
    }

    Ok((out, None))
}

pub(super) fn materialize_inline_agent_task_tasks_arg(
    runner_id: &str,
    args: &[String],
) -> Result<(Vec<String>, Option<LabWorkspaceMappingEntry>)> {
    materialize_inline_agent_task_tasks_arg_with(args, |spec| {
        sync_inline_agent_task_file(
            runner_id,
            spec,
            "agent-task-tasks.json",
            "agent_task_tasks_remapped",
        )
    })
}

fn materialize_inline_agent_task_tasks_arg_with(
    args: &[String],
    mut sync: impl FnMut(&str) -> Result<Option<(String, LabWorkspaceMappingEntry)>>,
) -> Result<(Vec<String>, Option<LabWorkspaceMappingEntry>)> {
    if subcommand_index(args, "agent-task")
        .and_then(|index| {
            args.get(index + 1)
                .filter(|arg| matches!(arg.as_str(), "dispatch" | "cook"))
                .map(|_| index + 1)
        })
        .is_none()
    {
        return Ok((args.to_vec(), None));
    }

    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;

    while let Some(arg) = iter.next() {
        if passthrough {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            out.push(arg.clone());
            continue;
        }
        if arg == "--tasks" {
            out.push(arg.clone());
            if let Some(spec) = iter.next() {
                if let Some((remapped_spec, entry)) = sync(spec)? {
                    out.push(remapped_spec);
                    out.extend(iter.cloned());
                    return Ok((out, Some(entry)));
                }
                out.push(spec.clone());
            }
            continue;
        }
        if let Some(spec) = arg.strip_prefix("--tasks=") {
            if let Some((remapped_spec, entry)) = sync(spec)? {
                out.push(format!("--tasks={remapped_spec}"));
                out.extend(iter.cloned());
                return Ok((out, Some(entry)));
            }
        }
        out.push(arg.clone());
    }

    Ok((out, None))
}

fn sync_inline_agent_task_plan(
    runner_id: &str,
    spec: &str,
) -> Result<Option<(String, LabWorkspaceMappingEntry)>> {
    sync_inline_agent_task_file(
        runner_id,
        spec,
        "agent-task-plan.json",
        "agent_task_plan_remapped",
    )
}

fn sync_inline_agent_task_file(
    runner_id: &str,
    spec: &str,
    filename: &str,
    role: &str,
) -> Result<Option<(String, LabWorkspaceMappingEntry)>> {
    if spec == "-" || spec.starts_with('@') || !looks_like_inline_json(spec) {
        return Ok(None);
    }
    serde_json::from_str::<serde_json::Value>(spec).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse remapped agent-task plan".to_string()),
        )
    })?;

    let temp = tempfile::tempdir().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("create remapped agent-task plan workspace".to_string()),
        )
    })?;
    let plan_file = temp.path().join(filename);
    fs::write(&plan_file, spec).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("write remapped agent-task plan".to_string()),
        )
    })?;
    let synced = sync_workspace(
        runner_id,
        RunnerWorkspaceSyncOptions {
            path: temp.path().display().to_string(),
            mode: RunnerWorkspaceSyncMode::Snapshot,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
        },
    )?
    .0;
    let remote_spec = format!("@{}/{}", synced.remote_path.trim_end_matches('/'), filename);
    let entry = workspace_mapping_entry(role, &synced);
    Ok(Some((remote_spec, entry)))
}

fn looks_like_inline_json(spec: &str) -> bool {
    let trimmed = spec.trim_start();
    trimmed.starts_with('{') || trimmed.starts_with('[')
}

pub(super) fn mirror_agent_task_run_plan_lifecycle(args: &[String], stdout: &str) -> Result<()> {
    let Some((plan_spec, run_id)) = agent_task_run_plan_recording_args(args) else {
        return Ok(());
    };
    if plan_spec == "-" {
        return Ok(());
    }
    let envelope = parse_offloaded_run_plan_envelope(stdout)?;
    if !is_agent_task_run_plan_envelope(&envelope) {
        return Ok(());
    }
    let Some(aggregate_value) = envelope.get("data").cloned() else {
        return Ok(());
    };
    let raw_plan = config::read_json_spec_to_string(&plan_spec)?;
    let plan: AgentTaskPlan = serde_json::from_str(&raw_plan).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some(format!("read agent-task plan {plan_spec}")),
        )
    })?;
    let aggregate: AgentTaskAggregate =
        serde_json::from_value(aggregate_value).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("parse offloaded agent-task aggregate".to_string()),
            )
        })?;

    agent_task_lifecycle::submit_plan(&plan, Some(&run_id))?;
    agent_task_lifecycle::mark_running(&run_id)?;
    agent_task_lifecycle::record_run_aggregate(&run_id, &plan, &aggregate)?;
    Ok(())
}

pub(super) fn parse_offloaded_run_plan_envelope(stdout: &str) -> Result<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) {
        return Ok(value);
    }

    let mut first_json = None;
    for (index, _) in stdout.match_indices('{') {
        let mut stream = serde_json::Deserializer::from_str(&stdout[index..]).into_iter();
        if let Some(Ok(value)) = stream.next() {
            if is_agent_task_run_plan_envelope(&value) {
                return Ok(value);
            }
            if first_json.is_none() {
                first_json = Some(value);
            }
        }
    }
    if let Some(value) = first_json {
        return Ok(value);
    }

    serde_json::from_str(stdout).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("parse offloaded agent-task run-plan output".to_string()),
        )
    })
}

fn is_agent_task_run_plan_envelope(value: &serde_json::Value) -> bool {
    value
        .get("data")
        .and_then(|data| data.get("schema"))
        .and_then(serde_json::Value::as_str)
        == Some("homeboy/agent-task-aggregate/v1")
        || value
            .get("data")
            .and_then(|data| data.get("plan_id"))
            .is_some()
}

pub(super) fn parse_offloaded_dispatch_envelope(stdout: &str) -> Result<Option<serde_json::Value>> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) {
        return Ok(agent_task_dispatch_envelope_value(&value).cloned());
    }

    for (index, _) in stdout.match_indices('{') {
        let mut stream = serde_json::Deserializer::from_str(&stdout[index..]).into_iter();
        if let Some(Ok(value)) = stream.next() {
            if let Some(envelope) = agent_task_dispatch_envelope_value(&value) {
                return Ok(Some(envelope.clone()));
            }
        }
    }

    Ok(None)
}

pub(super) fn parse_offloaded_dispatch_envelope_from_outputs(
    stdout: &str,
    stderr: &str,
) -> Result<Option<serde_json::Value>> {
    parse_offloaded_dispatch_envelope(stdout).and_then(|parsed| match parsed {
        Some(envelope) => Ok(Some(envelope)),
        None => parse_offloaded_dispatch_envelope(stderr),
    })
}

fn agent_task_dispatch_envelope_value(value: &serde_json::Value) -> Option<&serde_json::Value> {
    if value.get("schema").and_then(serde_json::Value::as_str)
        == Some("homeboy/agent-task-dispatch/v1")
    {
        return Some(value);
    }
    let data = value.get("data")?;
    (data.get("schema").and_then(serde_json::Value::as_str)
        == Some("homeboy/agent-task-dispatch/v1"))
    .then_some(data)
}

fn agent_task_run_plan_recording_args(args: &[String]) -> Option<(String, String)> {
    let run_plan_index = subcommand_index(args, "agent-task").and_then(|index| {
        args.get(index + 1)
            .filter(|arg| arg.as_str() == "run-plan")
            .map(|_| index + 1)
    })?;

    let mut plan = None;
    let mut record_run_id = None;
    let mut iter = args.iter().skip(run_plan_index + 1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        match arg.as_str() {
            "--plan" => plan = iter.next().cloned(),
            "--record-run-id" => record_run_id = iter.next().cloned(),
            _ => {
                if let Some(value) = arg.strip_prefix("--plan=") {
                    plan = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--record-run-id=") {
                    record_run_id = Some(value.to_string());
                }
            }
        }
    }

    Some((plan?, record_run_id?))
}

pub(super) fn agent_task_dispatch_requested_run_id(args: &[String]) -> Option<String> {
    let action_index = subcommand_index(args, "agent-task").and_then(|index| {
        args.get(index + 1)
            .filter(|arg| matches!(arg.as_str(), "dispatch" | "cook"))
            .map(|_| index + 1)
    })?;

    let mut iter = args.iter().skip(action_index + 1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--run-id" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--run-id=") {
            return (!value.is_empty()).then(|| value.to_string());
        }
    }

    None
}

pub(super) fn lab_pre_dispatch_failure_message(output: &str) -> Option<String> {
    if let Some(message) = lab_pre_dispatch_dependency_failure_message(output) {
        return Some(message);
    }

    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

fn lab_pre_dispatch_dependency_failure_message(output: &str) -> Option<String> {
    if !looks_like_prepared_dependency_failure(output) {
        return None;
    }

    let missing_path = first_quoted_prepared_dependency_path(output)
        .unwrap_or_else(|| "prepared dependency path".to_string());
    Some(format!(
        "Lab runtime failed before agent dispatch while staging dependency `{missing_path}`. The selected Lab runner has a stale or misconfigured runtime dependency; repair or refresh the runner runtime, then retry this cook run."
    ))
}

fn looks_like_prepared_dependency_failure(output: &str) -> bool {
    let lower = output.to_lowercase();
    lower.contains("prepared-plugins/")
        && (lower.contains("enoent")
            || lower.contains("no such file or directory")
            || lower.contains("lstat"))
}

fn first_quoted_prepared_dependency_path(output: &str) -> Option<String> {
    output
        .split(['\'', '"'])
        .find(|part| part.contains("prepared-plugins/"))
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offloaded_run_plan_envelope_parser_tolerates_extension_stdout_chatter() {
        let stdout = concat!(
            "Setting up WordPress extension...\n",
            "Installing npm dependencies...\n",
            "{\"success\":false,\"data\":{\"status\":\"failed\"}}\n",
            "trailing diagnostic\n"
        );

        let parsed = parse_offloaded_run_plan_envelope(stdout).expect("parse mixed stdout");

        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["data"]["status"], "failed");
    }

    #[test]
    fn offloaded_run_plan_envelope_parser_selects_aggregate_from_mixed_json() {
        let stdout = concat!(
            "{\"success\":true,\"data\":{\"command\":\"extension.setup\"}}\n",
            "setup complete\n",
            "{\"success\":true,\"data\":{\"schema\":\"homeboy/agent-task-aggregate/v1\",\"plan_id\":\"plan-1\",\"status\":\"succeeded\",\"totals\":{\"succeeded\":6}}}\n"
        );

        let parsed = parse_offloaded_run_plan_envelope(stdout).expect("parse aggregate envelope");

        assert_eq!(parsed["data"]["plan_id"], "plan-1");
        assert_eq!(parsed["data"]["totals"]["succeeded"], 6);
    }

    #[test]
    fn offloaded_dispatch_envelope_parser_selects_structured_failure_from_mixed_stdout() {
        let stdout = concat!(
            "remote setup complete\n",
            "{\"success\":true,\"data\":{\"command\":\"extension.setup\"}}\n",
            "{\"success\":false,\"data\":{\"schema\":\"homeboy/agent-task-dispatch/v1\",\"run_id\":\"run-1\",\"state\":\"failed\",\"record\":{},\"aggregate\":{\"status\":\"failed\"}}}\n"
        );

        let parsed = parse_offloaded_dispatch_envelope(stdout)
            .expect("parse dispatch stdout")
            .expect("dispatch envelope found");

        assert_eq!(parsed["run_id"], "run-1");
        assert_eq!(parsed["aggregate"]["status"], "failed");
    }

    #[test]
    fn offloaded_dispatch_envelope_parser_selects_structured_failure_from_stderr() {
        let stdout = "remote setup complete\n";
        let stderr = concat!(
            "{\n",
            "  \"success\": false,\n",
            "  \"data\": {\n",
            "    \"schema\": \"homeboy/agent-task-dispatch/v1\",\n",
            "    \"run_id\": \"conductor-full-loop-proof-retry3-20260612\",\n",
            "    \"state\": \"failed\",\n",
            "    \"aggregate\": {\n",
            "      \"status\": \"failed\",\n",
            "      \"outcomes\": [{\n",
            "        \"task_id\": \"cook-conductor\",\n",
            "        \"status\": \"failed\",\n",
            "        \"summary\": \"Remote agent task failed.\",\n",
            "        \"metadata\": {\n",
            "          \"provider\": \"remote.agent-task-executor\",\n",
            "          \"runtime_run_result\": {\n",
            "            \"schema\": \"remote/agent-task-run-result/v1\",\n",
            "            \"status\": \"failed\",\n",
            "            \"failure_classification\": \"runtime\"\n",
            "          }\n",
            "        }\n",
            "      }]\n",
            "    }\n",
            "  }\n",
            "}\n"
        );

        let parsed = parse_offloaded_dispatch_envelope_from_outputs(stdout, stderr)
            .expect("parse dispatch outputs")
            .expect("dispatch envelope found");

        assert_eq!(
            parsed["run_id"],
            "conductor-full-loop-proof-retry3-20260612"
        );
        assert_eq!(
            parsed["aggregate"]["outcomes"][0]["task_id"],
            "cook-conductor"
        );
        assert_eq!(
            parsed["aggregate"]["outcomes"][0]["metadata"]["provider"],
            "remote.agent-task-executor"
        );
        assert_eq!(
            parsed["aggregate"]["outcomes"][0]["metadata"]["runtime_run_result"]
                ["failure_classification"],
            "runtime"
        );
    }

    #[test]
    fn non_aggregate_offloaded_run_plan_stdout_is_not_mirrored() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
            "--plan".to_string(),
            "@/tmp/plan.json".to_string(),
            "--record-run-id".to_string(),
            "run-1".to_string(),
        ];
        let stdout = "{\"success\":true,\"data\":{\"command\":\"extension.setup\"}}";

        mirror_agent_task_run_plan_lifecycle(&args, stdout).expect("ignore non-aggregate output");
    }

    #[test]
    fn agent_task_dispatch_requested_run_id_accepts_cook_and_dispatch() {
        assert_eq!(
            agent_task_dispatch_requested_run_id(&[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--run-id".to_string(),
                "cook-run".to_string(),
            ]),
            Some("cook-run".to_string())
        );
        assert_eq!(
            agent_task_dispatch_requested_run_id(&[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "dispatch".to_string(),
                "--run-id=dispatch-run".to_string(),
            ]),
            Some("dispatch-run".to_string())
        );
        assert_eq!(
            agent_task_dispatch_requested_run_id(&[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "status".to_string(),
                "dispatch-run".to_string(),
            ]),
            None
        );
    }

    #[test]
    fn agent_task_dispatch_requested_run_id_allows_global_flags_before_agent_task() {
        assert_eq!(
            agent_task_dispatch_requested_run_id(&[
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "agent-task".to_string(),
                "dispatch".to_string(),
                "--run-id=dispatch-run".to_string(),
            ]),
            Some("dispatch-run".to_string())
        );
    }

    #[test]
    fn materializes_inline_agent_task_cook_tasks_json() {
        let prompt = "Cook sensitive implementation details";
        let tasks = serde_json::json!([{ "prompt": prompt }]).to_string();
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--tasks".to_string(),
            tasks.clone(),
            "--concurrency".to_string(),
            "4".to_string(),
        ];

        let (rewritten, entry) = materialize_inline_agent_task_tasks_arg_with(&args, |spec| {
            assert_eq!(spec, tasks);
            Ok(Some(fake_synced_file(
                "@/remote/input/agent-task-tasks.json",
                "agent_task_tasks_remapped",
            )))
        })
        .expect("rewrite tasks arg");

        assert_eq!(
            rewritten,
            vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
                "--tasks".to_string(),
                "@/remote/input/agent-task-tasks.json".to_string(),
                "--concurrency".to_string(),
                "4".to_string(),
            ]
        );
        assert!(!rewritten.join(" ").contains(prompt));
        assert_eq!(entry.expect("mapping entry").remote_path(), "/remote/input");
    }

    #[test]
    fn leaves_agent_task_tasks_file_specs_in_argv() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--tasks=@tasks.json".to_string(),
        ];

        let (rewritten, entry) = materialize_inline_agent_task_tasks_arg_with(&args, |spec| {
            assert_eq!(spec, "@tasks.json");
            Ok(None)
        })
        .expect("rewrite tasks arg");

        assert_eq!(rewritten, args);
        assert!(entry.is_none());
    }

    fn fake_synced_file(remote_spec: &str, role: &str) -> (String, LabWorkspaceMappingEntry) {
        let synced = crate::core::runner::RunnerWorkspaceSyncOutput {
            command: "runner.workspace.sync",
            runner_id: "lab".to_string(),
            local_path: "/local/input".to_string(),
            remote_path: "/remote/input".to_string(),
            sync_mode: RunnerWorkspaceSyncMode::Snapshot,
            snapshot_identity: "snapshot".to_string(),
            files: 1,
            bytes: 42,
            excludes: Vec::new(),
            includes: Vec::new(),
            workspace_cleanliness: "clean".to_string(),
        };
        (
            remote_spec.to_string(),
            workspace_mapping_entry(role, &synced),
        )
    }

    #[test]
    fn pre_dispatch_failure_message_summarizes_prepared_dependency_staging_failure() {
        let output = "ENOENT: no such file or directory, lstat '/home/chubes/Developer/.tmp/homeboy-artifacts/prepared-plugins/agents-api'";

        let message = lab_pre_dispatch_failure_message(output).expect("message");

        assert!(message.contains("Lab runtime failed before agent dispatch"));
        assert!(message.contains("prepared-plugins/agents-api"));
        assert!(message.contains("repair or refresh the runner runtime"));
    }
}
