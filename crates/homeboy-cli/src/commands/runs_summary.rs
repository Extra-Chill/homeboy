//! Compact human-readable summary for `homeboy runs show`.
//!
//! `runs show` returns a `RunDetail` that embeds full run metadata and the
//! complete artifact list. For bench runs in particular, the useful evidence
//! — shared-state files, runner artifact bundles, scenario-specific
//! artifacts — is buried in a large JSON payload (#3260).
//!
//! This module renders a compact summary from the serialized `RunsOutput`
//! value, surfacing run identity, status, and (prominently) each artifact's
//! locator plus a concise `homeboy runs artifact get ...` command to inspect
//! it. The full JSON remains available via `runs show <id> --json` and is
//! always written to `--output <file>` unchanged.

use homeboy::core::engine::shell::quote_arg;
use homeboy::core::observation::nested_failure_causes_from_run_detail;
use serde_json::{json, Value};

use super::summary_json::{string_value, value_at};

const PRIMARY_ARTIFACT_LIMIT: usize = 8;
const RECOVERY_COMMAND_LIMIT: usize = 4;
const RUN_SHOW_OUTPUT_BYTES: usize = 16 * 1024;
const COMMAND_ID_BYTES: usize = 512;
const SUMMARY_LINE_LIMIT: usize = 40;
const SUMMARY_LINE_BYTES: usize = 256;

/// Render a compact summary for a serialized `RunsOutput` value. Returns
/// `None` for any variant other than `show`, leaving other `runs`
/// subcommands with their existing full-JSON presentation.
pub(crate) fn render_runs_show_summary(payload: &Value) -> Option<String> {
    if payload.get("variant").and_then(Value::as_str)? != "show" {
        return None;
    }
    let run = value_at(payload, &["payload", "run"])?;
    Some(render_run_detail(run))
}

/// Keep the default command-result data aligned with the compact human view.
/// Explicit JSON and `--output` continue to use the untouched handler result.
pub(crate) fn project_runs_show_output(payload: &Value) -> Value {
    if payload.get("variant").and_then(Value::as_str) != Some("show") {
        return payload.clone();
    }
    let Some(show) = value_at(payload, &["payload"]) else {
        return payload.clone();
    };
    let Some(run) = show.get("run") else {
        return payload.clone();
    };
    let run_id = string_value(run, &["id"]).unwrap_or("<run-id>");
    let command_run_id = if serde_json::to_vec(&quote_arg(run_id))
        .is_ok_and(|encoded| encoded.len() <= COMMAND_ID_BYTES)
    {
        run_id
    } else {
        "<oversized-run-id>"
    };
    let quoted_run_id = quote_arg(command_run_id);
    let artifacts = run
        .get("artifacts")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let artifact_refs = artifacts
        .iter()
        .take(super::operator_projection::ITEM_LIMIT)
        .map(|artifact| {
            let id = string_value(artifact, &["id"]).unwrap_or("artifact");
            let command_id = if serde_json::to_vec(&quote_arg(id))
                .is_ok_and(|encoded| encoded.len() <= COMMAND_ID_BYTES)
            {
                id
            } else {
                "<oversized-artifact-id>"
            };
            let artifact_type = artifact
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("file");
            let command = match artifact_type {
                "file" => Some(format!(
                    "homeboy runs artifact get {quoted_run_id} {} -o <path>",
                    quote_arg(command_id)
                )),
                "directory" => Some(format!(
                    "homeboy runs artifact preview {quoted_run_id} {}",
                    quote_arg(command_id)
                )),
                _ => None,
            };
            json!({
                "id": super::operator_projection::text(id),
                "kind": artifact.get("kind").and_then(Value::as_str).map(super::operator_projection::text),
                "type": super::operator_projection::text(artifact_type),
                "size_bytes": artifact.get("size_bytes"),
                "command": command,
            })
        })
        .collect::<Vec<_>>();
    let metadata = run.get("metadata").unwrap_or(&Value::Null);
    let root_cause = nested_failure_causes_from_run_detail(run)
        .into_iter()
        .next()
        .and_then(|cause| serde_json::to_value(cause).ok())
        .map(|cause| super::operator_projection::value(&cause));
    let runner_terminal = runner_terminal_projection(metadata);
    let phase = first_value(
        metadata,
        &[
            "/phase",
            "/cook_progress/phase",
            "/runner_execution_record/phase",
            "/runner_terminal_projection/state",
        ],
    )
    .map(super::operator_projection::value);
    let mut recovery = Vec::new();
    collect_recovery_commands(metadata, &mut recovery);
    recovery.sort();
    recovery.dedup();
    let recovery_total = recovery.len();
    recovery.truncate(RECOVERY_COMMAND_LIMIT);
    let recovery = recovery
        .into_iter()
        .map(|command| super::operator_projection::text(&command))
        .collect::<Vec<_>>();

    let mut compact_run = json!({
        "id": run.get("id").map(super::operator_projection::value),
        "kind": run.get("kind").map(super::operator_projection::value),
        "status": run.get("status").map(super::operator_projection::value),
        "started_at": run.get("started_at").map(super::operator_projection::value),
        "finished_at": run.get("finished_at").map(super::operator_projection::value),
        "component_id": run.get("component_id").map(super::operator_projection::value),
        "rig_id": run.get("rig_id").map(super::operator_projection::value),
        "git_sha": run.get("git_sha").map(super::operator_projection::value),
        "command": run.get("command").map(super::operator_projection::value),
        "cwd": run.get("cwd").map(super::operator_projection::value),
        "status_note": run.get("status_note").map(super::operator_projection::value),
        "artifact_index": null,
        "homeboy_version": run.get("homeboy_version").map(super::operator_projection::value),
        "metadata": {
            "operator_projection": {
                "phase": phase,
                "root_cause": root_cause,
                "authoritative_runner_terminal_state": runner_terminal,
                "legal_recovery": recovery,
                "legal_recovery_total": recovery_total,
                "artifact_refs": artifact_refs,
                "artifact_total": artifacts.len(),
                "detail_refs": {
                    "full_run": format!("homeboy runs show {quoted_run_id} --format json"),
                    "evidence": format!("homeboy runs evidence {quoted_run_id}"),
                    "artifacts": format!("homeboy runs artifacts {quoted_run_id}"),
                    "export": format!("homeboy runs show {quoted_run_id} --output <path>"),
                },
                "omitted": ["handoff", "events", "source_snapshot", "transcript", "runtime_payload"],
            }
        },
        "artifacts": [],
    });
    let mut projected = json!({
        "variant": "show",
        "payload": {
            "command": show.get("command"),
            "run": compact_run,
            "_homeboy_actionable": {
                "next_actions": [
                    { "label": "show full run", "command": format!("homeboy runs show {quoted_run_id} --format json"), "kind": "show" },
                    { "label": "inspect evidence", "command": format!("homeboy runs evidence {quoted_run_id}"), "kind": "show" },
                    { "label": "list artifacts", "command": format!("homeboy runs artifacts {quoted_run_id}"), "kind": "artifacts" },
                ]
            }
        }
    });
    if super::operator_projection::serialized_len(&projected) > RUN_SHOW_OUTPUT_BYTES {
        compact_run["metadata"]["operator_projection"]["artifact_refs"] = json!([]);
        compact_run["metadata"]["operator_projection"]["artifact_refs_omitted"] =
            json!(artifacts.len());
        projected["payload"]["run"] = compact_run;
    }
    if super::operator_projection::serialized_len(&projected) > RUN_SHOW_OUTPUT_BYTES {
        let operator = projected["payload"]["run"]["metadata"]["operator_projection"].clone();
        let runner_terminal = compact_operator_fields(
            &operator["authoritative_runner_terminal_state"],
            &["state", "status", "job_id", "classification", "event_count"],
        );
        let runner_error = compact_operator_fields(
            &operator["authoritative_runner_terminal_state"]["error"],
            &["code", "message"],
        );
        let mut runner_terminal = runner_terminal;
        if !runner_error.is_null() {
            runner_terminal["error"] = runner_error;
        }
        let operator = json!({
            "phase": compact_operator_scalar(&operator["phase"]),
            "root_cause": compact_operator_fields(
                &operator["root_cause"],
                &["task_id", "class", "code", "status", "message", "source", "owner"],
            ),
            "authoritative_runner_terminal_state": runner_terminal,
            "legal_recovery": operator["legal_recovery"],
            "legal_recovery_total": operator["legal_recovery_total"],
            "artifact_refs": [],
            "artifact_refs_omitted": artifacts.len(),
            "artifact_total": artifacts.len(),
            "detail_refs": {
                "full_run": format!("homeboy runs show {quoted_run_id} --format json"),
                "evidence": format!("homeboy runs evidence {quoted_run_id}"),
                "artifacts": format!("homeboy runs artifacts {quoted_run_id}"),
                "export": format!("homeboy runs show {quoted_run_id} --output <path>"),
            },
            "omitted": ["handoff", "events", "source_snapshot", "transcript", "runtime_payload"],
        });
        projected = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": super::operator_projection::text(command_run_id),
                    "kind": compact_required_string(&projected["payload"]["run"]["kind"], "unknown"),
                    "status": compact_required_string(&projected["payload"]["run"]["status"], "unknown"),
                    "started_at": compact_required_string(&projected["payload"]["run"]["started_at"], ""),
                    "finished_at": projected["payload"]["run"]["finished_at"],
                    "component_id": null,
                    "rig_id": null,
                    "git_sha": null,
                    "command": null,
                    "cwd": null,
                    "status_note": null,
                    "artifact_index": null,
                    "homeboy_version": null,
                    "metadata": { "operator_projection": operator },
                    "artifacts": [],
                },
                "_homeboy_actionable": {
                    "next_actions": [
                        { "label": "show full run", "command": format!("homeboy runs show {quoted_run_id} --format json"), "kind": "show" },
                        { "label": "inspect evidence", "command": format!("homeboy runs evidence {quoted_run_id}"), "kind": "show" },
                    ]
                }
            }
        });
    }
    if super::operator_projection::serialized_len(&projected) > RUN_SHOW_OUTPUT_BYTES {
        let run = &projected["payload"]["run"];
        let operator = &run["metadata"]["operator_projection"];
        let mut runner_terminal = compact_operator_fields(
            &operator["authoritative_runner_terminal_state"],
            &["state", "status"],
        );
        let runner_error = compact_operator_fields(
            &operator["authoritative_runner_terminal_state"]["error"],
            &["code", "message"],
        );
        if !runner_error.is_null() {
            runner_terminal["error"] = runner_error;
        }
        let legal_recovery = operator["legal_recovery"]
            .as_array()
            .and_then(|commands| commands.first())
            .cloned()
            .into_iter()
            .collect::<Vec<_>>();
        projected = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": compact_required_string(&run["id"], command_run_id),
                    "kind": compact_required_string(&run["kind"], "unknown"),
                    "status": compact_required_string(&run["status"], "unknown"),
                    "started_at": compact_required_string(&run["started_at"], ""),
                    "finished_at": run["finished_at"],
                    "component_id": null,
                    "rig_id": null,
                    "git_sha": null,
                    "command": null,
                    "cwd": null,
                    "status_note": null,
                    "artifact_index": null,
                    "homeboy_version": null,
                    "metadata": {
                        "operator_projection": {
                            "phase": compact_operator_scalar(&operator["phase"]),
                            "root_cause": compact_operator_fields(
                                &operator["root_cause"],
                                &["class", "code", "message"],
                            ),
                            "authoritative_runner_terminal_state": runner_terminal,
                            "legal_recovery": legal_recovery,
                            "artifact_refs": [],
                            "artifact_refs_omitted": artifacts.len(),
                            "artifact_total": artifacts.len(),
                            "detail_refs": {
                                "full_run": format!("homeboy runs show {quoted_run_id} --format json"),
                                "evidence": format!("homeboy runs evidence {quoted_run_id}"),
                                "export": format!("homeboy runs show {quoted_run_id} --output <path>"),
                            },
                        }
                    },
                    "artifacts": [],
                },
                "_homeboy_actionable": {
                    "next_actions": [
                        { "label": "show full run", "command": format!("homeboy runs show {quoted_run_id} --format json"), "kind": "show" },
                        { "label": "inspect evidence", "command": format!("homeboy runs evidence {quoted_run_id}"), "kind": "show" },
                    ]
                }
            }
        });
    }
    debug_assert!(
        super::operator_projection::serialized_len(&projected) <= RUN_SHOW_OUTPUT_BYTES,
        "the minimal runs show operator projection must fit its byte budget"
    );
    projected
}

fn compact_operator_scalar(value: &Value) -> Value {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
        Value::String(value) => Value::String(super::operator_projection::text(value)),
        _ => Value::Null,
    }
}

fn compact_required_string(value: &Value, fallback: &str) -> String {
    value
        .as_str()
        .map(super::operator_projection::text)
        .unwrap_or_else(|| fallback.to_string())
}

fn compact_operator_fields(value: &Value, fields: &[&str]) -> Value {
    let Some(source) = value.as_object() else {
        return Value::Null;
    };
    let fields = fields
        .iter()
        .filter_map(|field| {
            let value = compact_operator_scalar(source.get(*field)?);
            (!value.is_null()).then(|| ((*field).to_string(), value))
        })
        .collect();
    Value::Object(fields)
}

fn first_value<'a>(value: &'a Value, pointers: &[&str]) -> Option<&'a Value> {
    pointers
        .iter()
        .find_map(|pointer| value.pointer(pointer))
        .filter(|value| !value.is_null())
}

fn runner_terminal_projection(metadata: &Value) -> Option<Value> {
    if let Some(terminal) = metadata
        .pointer("/runner_terminal_projection")
        .and_then(Value::as_object)
    {
        return Some(json!({
            "state": terminal.get("state").map(super::operator_projection::value),
            "status": terminal.get("status").map(super::operator_projection::value),
            "job_id": terminal.get("job_id").map(super::operator_projection::value),
            "classification": terminal.get("classification").map(super::operator_projection::value),
            "artifact_promotion": terminal.get("artifact_promotion").map(super::operator_projection::value),
            "event_count": terminal.get("event_count"),
            "error": terminal.get("error").map(|error| json!({
                "code": error.get("code").map(super::operator_projection::value),
                "message": error.get("message").map(super::operator_projection::value),
            })),
        }));
    }
    first_value(
        metadata,
        &[
            "/runner_execution_record/status",
            "/runner_job_status",
            "/lab/remote_job_status",
        ],
    )
    .map(super::operator_projection::value)
}

/// Render a `runs show -q` / `runs artifact get -q` field projection as plain
/// lines so the selectors work as a grep replacement. One value per selector,
/// in selector order; strings print raw, other JSON prints compactly. The full
/// labeled `{field, value}` structure stays available in the JSON output and in
/// `--output <file>`. Returns `None` for any other variant.
pub(crate) fn render_runs_field_selection(payload: &Value) -> Option<String> {
    if payload.get("variant").and_then(Value::as_str)? != "field_selection" {
        return None;
    }
    let fields = value_at(payload, &["payload", "fields"])?.as_array()?;
    let lines = fields
        .iter()
        .map(|field| field_value_line(field.get("value").unwrap_or(&Value::Null)))
        .collect::<Vec<_>>();
    Some(format!("{}\n", lines.join("\n")))
}

fn field_value_line(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

fn render_run_detail(run: &Value) -> String {
    let run_id = string_value(run, &["id"]).unwrap_or("<unknown>");
    let kind = string_value(run, &["kind"]).unwrap_or("run");
    let status = string_value(run, &["status"]).unwrap_or("unknown");

    // Put terminal state and a supported next action ahead of run metadata.
    let mut lines = vec![format!("Status: {status}")];
    lines.extend(failure_summary_lines(run));
    lines.extend(recovery_command_lines(run));
    lines.push(format!("Run {run_id} ({kind})"));

    if let Some(component) = string_value(run, &["component_id"]) {
        lines.push(format!("Component: {component}"));
    }
    if let Some(rig) = string_value(run, &["rig_id"]) {
        lines.push(format!("Rig: {rig}"));
    }
    if let Some(sha) = string_value(run, &["git_sha"]) {
        lines.push(format!("Component SHA: {sha}"));
    }
    if let Some(started) = string_value(run, &["started_at"]) {
        lines.push(format!("Started: {started}"));
    }
    if let Some(finished) = string_value(run, &["finished_at"]) {
        lines.push(format!("Finished: {finished}"));
    }
    lines.extend(execution_provenance_lines(run));

    if kind == "bench" {
        lines.extend(super::bench_summary::bench_hotspot_lines(run));
        lines.extend(super::bench_summary::bench_regression_threshold_lines(run));
    } else if kind == "fuzz" {
        lines.extend(super::runs::fuzz_hotspot_lines(run));
    }
    lines.extend(super::bench_summary::bench_coverage_lines(run));
    lines.extend(key_artifact_lines(run, run_id));
    lines.extend(artifact_lines(run, run_id));
    lines.extend(report_followup_lines(run, run_id, kind));
    lines.extend(detail_reference_lines(run_id));

    finish(lines)
}

fn execution_provenance_lines(run: &Value) -> Vec<String> {
    let Some(provenance) = value_at(run, &["metadata", "execution_provenance"]) else {
        return Vec::new();
    };
    let Some(intent) = provenance.get("operator_intent") else {
        return Vec::new();
    };
    let mut lines = vec!["Execution provenance:".to_string()];
    if let Some(command) = intent.get("rerun_command").and_then(Value::as_str) {
        lines.push(format!("  rerun: {command}"));
    }
    if let Some(placement) = intent.get("placement").and_then(Value::as_str) {
        lines.push(format!("  requested placement: {placement}"));
    }
    if let Some(runner) = intent.get("runner_id").and_then(Value::as_str) {
        lines.push(format!("  requested runner: {runner}"));
    }
    if let Some(execution) = provenance.get("resolved_execution") {
        let location = execution
            .get("location")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let runner = execution.get("runner_id").and_then(Value::as_str);
        lines.push(match runner {
            Some(runner) => format!("  resolved execution: {location} ({runner})"),
            None => format!("  resolved execution: {location}"),
        });
    }
    if let Some(origin) = provenance
        .pointer("/resource_policy/decision_origin")
        .and_then(Value::as_str)
    {
        lines.push(format!("  policy decision: {origin}"));
    }
    lines
}

fn failure_summary_lines(run: &Value) -> Vec<String> {
    let causes = nested_failure_causes_from_run_detail(run);
    if causes.is_empty() {
        return Vec::new();
    }

    let mut lines = vec!["Failure summary:".to_string()];
    for cause in causes {
        lines.push(format!(
            "  {}: {} ({})",
            cause.surface, cause.message, cause.source
        ));
    }
    lines
}

fn recovery_command_lines(run: &Value) -> Vec<String> {
    let mut commands = Vec::new();
    collect_recovery_commands(
        value_at(run, &["metadata"]).unwrap_or(&Value::Null),
        &mut commands,
    );
    commands.sort();
    commands.dedup();
    commands.truncate(RECOVERY_COMMAND_LIMIT);

    if commands.is_empty() {
        return Vec::new();
    }

    let mut lines = vec!["Recovery:".to_string()];
    lines.extend(commands.into_iter().map(|command| format!("  {command}")));
    lines
}

fn collect_recovery_commands(value: &Value, commands: &mut Vec<String>) {
    match value {
        Value::Object(values) => {
            for (key, nested) in values {
                let normalized = key.to_ascii_lowercase();
                // Cleanup inventories are evidence, not an operator recovery plan.
                if normalized.contains("cleanup") {
                    continue;
                }
                if matches!(
                    normalized.as_str(),
                    "recovery_commands"
                        | "rerun_command"
                        | "retry_command"
                        | "resume_command"
                        | "next_command"
                ) {
                    match nested {
                        Value::String(command) => commands.push(command.clone()),
                        Value::Array(values) => commands
                            .extend(values.iter().filter_map(Value::as_str).map(str::to_string)),
                        _ => {}
                    }
                }
                collect_recovery_commands(nested, commands);
            }
        }
        Value::Array(values) => {
            for nested in values {
                collect_recovery_commands(nested, commands);
            }
        }
        _ => {}
    }
}

fn report_followup_lines(run: &Value, run_id: &str, kind: &str) -> Vec<String> {
    if kind != "bench" {
        return Vec::new();
    }

    let Some(component) = string_value(run, &["component_id"]) else {
        return Vec::new();
    };

    let mut filter = format!("--kind bench --component {component}");
    if let Some(rig) = string_value(run, &["rig_id"]) {
        filter.push_str(&format!(" --rig {rig}"));
    }
    if let Some(scenario) = first_bench_scenario(run) {
        filter.push_str(&format!(" --scenario {scenario}"));
    }

    vec![
        "Reports:".to_string(),
        format!("  history: homeboy runs list {filter}"),
        format!("  distribution: homeboy runs distribution {filter} --field <metadata.path>"),
        format!(
            "  compare: homeboy runs bench-compare --from-run <other-run-id> --to-run {run_id}"
        ),
    ]
}

fn first_bench_scenario(run: &Value) -> Option<&str> {
    value_at(run, &["metadata", "scenario_metrics"])
        .and_then(Value::as_array)
        .and_then(|scenarios| scenarios.first())
        .and_then(|scenario| string_value(scenario, &["scenario_id"]))
}

/// Surface every recorded artifact with its best on-disk / network locator
/// and a concise command to fetch it (#3260). Local file paths are shown
/// directly; otherwise the public/viewer URL is shown.
fn artifact_lines(run: &Value, run_id: &str) -> Vec<String> {
    let Some(artifacts) = value_at(run, &["artifacts"]).and_then(Value::as_array) else {
        return Vec::new();
    };
    if artifacts.is_empty() {
        return vec![
            "Artifacts: none recorded".to_string(),
            format!(
                "  hint: if this run produced files that reviewers need, promote or attach the output directory before sharing evidence; see `homeboy self docs operators/artifact-loop-runner-matrix` and `homeboy runs evidence {run_id}`."
            ),
        ];
    }

    let primary = artifacts
        .iter()
        .filter(|artifact| !is_cleanup_inventory_artifact(artifact))
        .collect::<Vec<_>>();
    let selected = if primary.is_empty() {
        artifacts
            .iter()
            .take(PRIMARY_ARTIFACT_LIMIT)
            .collect::<Vec<_>>()
    } else {
        primary
            .into_iter()
            .take(PRIMARY_ARTIFACT_LIMIT)
            .collect::<Vec<_>>()
    };
    let omitted = artifacts.len().saturating_sub(selected.len());
    let mut lines = vec![if omitted == 0 {
        format!("Artifacts ({}):", artifacts.len())
    } else {
        format!(
            "Primary artifacts ({} of {}):",
            selected.len(),
            artifacts.len()
        )
    }];
    for artifact in selected {
        let id = string_value(artifact, &["id"]).unwrap_or("artifact");
        let kind = string_value(artifact, &["kind"]).unwrap_or("");
        let label = if kind.is_empty() {
            id.to_string()
        } else {
            format!("{id} [{kind}]")
        };
        match artifact_locator(artifact) {
            Some(locator) => lines.push(format!("  {label}: {locator}")),
            None => lines.push(format!("  {label}")),
        }
        // Only file artifacts are fetchable via `runs artifact get`.
        if string_value(artifact, &["type"]) == Some("file") {
            lines.push(format!(
                "    get: homeboy runs artifact get {run_id} {id} -o <path>"
            ));
        }
    }
    if omitted > 0 {
        lines.push(format!(
            "  omitted: {omitted} artifact records; inspect the full inventory with `homeboy runs artifacts {run_id}`"
        ));
    }
    lines
}

fn is_cleanup_inventory_artifact(artifact: &Value) -> bool {
    ["id", "name", "kind", "artifact_id"]
        .into_iter()
        .filter_map(|key| string_value(artifact, &[key]))
        .any(|value| value.to_ascii_lowercase().contains("cleanup"))
}

fn key_artifact_lines(run: &Value, run_id: &str) -> Vec<String> {
    value_at(run, &["artifacts"])
        .and_then(Value::as_array)
        .map(|artifacts| super::key_artifacts::key_artifact_lines(artifacts, Some(run_id), true))
        .unwrap_or_default()
}

fn artifact_locator(artifact: &Value) -> Option<String> {
    super::key_artifacts::artifact_locator(artifact).map(str::to_string)
}

fn detail_reference_lines(run_id: &str) -> Vec<String> {
    vec![
        "Details:".to_string(),
        format!("  full run: homeboy runs show {run_id} --json"),
        format!("  failure and evidence: homeboy runs evidence {run_id}"),
        format!("  full artifact inventory: homeboy runs artifacts {run_id}"),
    ]
}

fn finish(lines: Vec<String>) -> String {
    let total = lines.len();
    let bounded = if total > SUMMARY_LINE_LIMIT {
        let tail = lines.len().saturating_sub(3);
        let mut bounded = lines
            .iter()
            .take(SUMMARY_LINE_LIMIT - 4)
            .map(|line| super::operator_projection::text_with_limit(line, SUMMARY_LINE_BYTES))
            .collect::<Vec<_>>();
        bounded.push(format!(
            "[omitted {} summary line(s); use `runs show <run-id> --format json` for full detail]",
            total - (SUMMARY_LINE_LIMIT - 1)
        ));
        bounded.extend(
            lines[tail..]
                .iter()
                .map(|line| super::operator_projection::text_with_limit(line, SUMMARY_LINE_BYTES)),
        );
        bounded
    } else {
        lines
            .iter()
            .map(|line| super::operator_projection::text_with_limit(line, SUMMARY_LINE_BYTES))
            .collect()
    };
    let mut output = bounded.join("\n");
    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn non_show_variant_returns_none() {
        let payload = json!({ "variant": "list", "payload": { "runs": [] } });
        assert!(render_runs_show_summary(&payload).is_none());
    }

    #[test]
    fn default_show_projection_bounds_expanding_runner_payloads_and_keeps_drilldowns() {
        let event = json!({ "kind": "runner_event", "body": "event".repeat(1024) });
        let artifact = |index| {
            json!({
                "id": format!("artifact-{index}"),
                "run_id": "runner-run-1",
                "kind": "runtime_log",
                "type": "file",
                "path": format!("/tmp/runner-run-1/artifact-{index}.log"),
                "metadata": { "transcript": "transcript".repeat(4096) },
                "created_at": "2026-08-31T00:00:00Z",
            })
        };
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "runner-run-1",
                    "kind": "runner-exec",
                    "status": "failed",
                    "started_at": "2026-08-31T00:00:00Z",
                    "finished_at": "2026-08-31T00:01:00Z",
                    "metadata": {
                        "runner_handoff": { "remote_argv": vec!["x".repeat(4096); 100] },
                        "source_snapshot": { "files": vec!["source".repeat(1024); 100] },
                        "events": vec![event; 100],
                        "path_materialization_plan": { "entries": vec!["path".repeat(1024); 100] },
                        "runner_terminal_projection": {
                            "state": "terminal_checkpointed",
                            "status": "failed",
                            "job_id": "job-1",
                            "event_count": 100,
                        },
                        "failure": {
                            "status": "failed",
                            "message": "task worktree has no .git",
                        },
                        "retry_command": "homeboy runner retry job-1",
                    },
                    "artifacts": (0..100).map(artifact).collect::<Vec<_>>(),
                },
                "_homeboy_actionable": { "artifacts": vec!["expanded"; 100] },
            }
        });

        let projected = project_runs_show_output(&payload);
        let operator = &projected["payload"]["run"]["metadata"]["operator_projection"];

        assert!(
            super::super::operator_projection::serialized_len(&projected) <= RUN_SHOW_OUTPUT_BYTES
        );
        let envelope = crate::commands::utils::response::cli_response_for_json_result_for_command(
            &Ok(projected.clone()),
            1,
            "runs",
            None,
        );
        assert!(serde_json::to_vec_pretty(&envelope).unwrap().len() <= 24 * 1024);
        assert_eq!(operator["phase"], "terminal_checkpointed");
        assert_eq!(
            operator["authoritative_runner_terminal_state"]["status"],
            "failed"
        );
        assert_eq!(
            operator["root_cause"]["message"],
            "task worktree has no .git"
        );
        assert_eq!(operator["legal_recovery"][0], "homeboy runner retry job-1");
        assert_eq!(operator["artifact_total"], 100);
        assert_eq!(operator["artifact_refs"].as_array().unwrap().len(), 12);
        assert_eq!(
            operator["detail_refs"]["full_run"],
            "homeboy runs show runner-run-1 --format json"
        );
        assert!(projected["payload"]["run"]["artifacts"]
            .as_array()
            .unwrap()
            .is_empty());
        serde_json::from_value::<crate::commands::utils::response::CommandActionableMetadata>(
            projected["payload"]["_homeboy_actionable"].clone(),
        )
        .expect("compact actionable metadata retains its typed contract");
        assert!(projected.to_string().len() < payload.to_string().len() / 100);

        assert_eq!(
            payload["payload"]["run"]["artifacts"]
                .as_array()
                .unwrap()
                .len(),
            100
        );
        assert!(payload["payload"]["run"]["metadata"]["source_snapshot"].is_object());
    }

    #[test]
    fn show_projection_keeps_required_types_and_valid_directory_commands_at_the_byte_limit() {
        let scalar = "'\u{0}".repeat(250);
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": scalar,
                    "kind": "runner-exec",
                    "status": "failed",
                    "started_at": "2026-08-31T00:00:00Z",
                    "finished_at": "2026-08-31T00:01:00Z",
                    "component_id": "c".repeat(500),
                    "rig_id": "r".repeat(500),
                    "git_sha": "g".repeat(500),
                    "command": "x".repeat(500),
                    "cwd": "w".repeat(500),
                    "metadata": {
                        "phase": "p".repeat(500),
                        "runner_terminal_projection": {
                            "state": "s".repeat(500),
                            "status": "f".repeat(500),
                            "job_id": "j".repeat(500),
                            "classification": "c".repeat(500),
                            "error": { "code": "e".repeat(500), "message": "m".repeat(500) },
                            "artifact_promotion": (0..12)
                                .map(|index| (format!("field-{index}"), json!("a".repeat(500))))
                                .collect::<serde_json::Map<_, _>>(),
                        },
                        "failure": { "status": "failed", "message": "root".repeat(125) },
                        "retry_commands": vec!["retry".repeat(100); 4],
                    },
                    "artifacts": [],
                },
            },
        });

        let projected = project_runs_show_output(&payload);
        let run = &projected["payload"]["run"];

        assert!(
            super::super::operator_projection::serialized_len(&projected) <= RUN_SHOW_OUTPUT_BYTES
        );
        for field in ["id", "kind", "status", "started_at"] {
            assert!(run[field].is_string(), "{field} remains a required string");
        }
        assert_eq!(
            run["metadata"]["operator_projection"]["detail_refs"]["full_run"],
            "homeboy runs show '<oversized-run-id>' --format json"
        );

        let directory = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "run directory",
                    "kind": "runner-exec",
                    "status": "succeeded",
                    "started_at": "2026-08-31T00:00:00Z",
                    "metadata": {},
                    "artifacts": [{
                        "id": "directory's artifact",
                        "kind": "report",
                        "type": "directory",
                    }],
                },
            },
        });
        let directory = project_runs_show_output(&directory);
        assert_eq!(
            directory["payload"]["run"]["metadata"]["operator_projection"]["artifact_refs"][0]
                ["command"],
            "homeboy runs artifact preview 'run directory' 'directory'\''s artifact'"
        );
    }

    #[test]
    fn show_summary_surfaces_identity_and_artifact_pointers() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "bench-run-42",
                    "kind": "bench",
                    "status": "pass",
                    "started_at": "2026-06-19T00:00:00Z",
                    "finished_at": "2026-06-19T00:01:00Z",
                    "component_id": "homeboy",
                    "rig_id": "rtc",
                    "git_sha": "abcdef1234",
                    "homeboy_version": "0.232.0",
                    "metadata": {},
                    "artifacts": [
                        {
                            "id": "bench_artifact",
                            "run_id": "bench-run-42",
                            "kind": "bench_artifact",
                            "type": "file",
                            "path": "/var/lib/homeboy/runs/bench-run-42/response-rows.json",
                            "created_at": "2026-06-19T00:01:00Z"
                        },
                        {
                            "id": "admin_url",
                            "run_id": "bench-run-42",
                            "kind": "admin_url",
                            "type": "url",
                            "path": "",
                            "url": "https://example.test/wp-admin/",
                            "created_at": "2026-06-19T00:01:00Z"
                        }
                    ]
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.starts_with("Status: pass\nRun bench-run-42 (bench)\n"));
        assert!(summary.contains("Component: homeboy\n"));
        assert!(summary.contains("Rig: rtc\n"));
        assert!(summary.contains("Component SHA: abcdef1234\n"));
        assert!(summary.contains("Artifacts (2):\n"));
        assert!(summary.contains(
            "  bench_artifact [bench_artifact]: /var/lib/homeboy/runs/bench-run-42/response-rows.json\n"
        ));
        assert!(summary.contains(
            "    get: homeboy runs artifact get bench-run-42 bench_artifact -o <path>\n"
        ));
        assert!(summary.contains("  admin_url [admin_url]: https://example.test/wp-admin/\n"));
        assert!(summary.contains("Reports:\n"));
        assert!(summary
            .contains("  history: homeboy runs list --kind bench --component homeboy --rig rtc\n"));
        assert!(summary.contains(
            "  distribution: homeboy runs distribution --kind bench --component homeboy --rig rtc --field <metadata.path>\n"
        ));
        assert!(summary.contains(
            "  compare: homeboy runs bench-compare --from-run <other-run-id> --to-run bench-run-42\n"
        ));
        assert!(summary.contains("full run: homeboy runs show bench-run-42 --json\n"));
        // URL artifacts are not fetchable via `runs artifact get`.
        assert!(!summary.contains("get: homeboy runs artifact get bench-run-42 admin_url"));
        // Compact: no raw JSON braces.
        assert!(!summary.contains("{\n"));
    }

    #[test]
    fn show_summary_surfaces_execution_provenance_and_rerun_command() {
        let payload = json!({
            "variant": "show",
            "payload": { "run": {
                "id": "review-1", "kind": "review", "status": "pass", "artifacts": [],
                "metadata": { "execution_provenance": {
                    "operator_intent": {
                        "rerun_command": "homeboy --placement local review --changed-since=origin/main",
                        "placement": "local", "runner_id": null
                    },
                    "resolved_execution": { "location": "controller", "runner_id": null },
                    "resource_policy": { "decision_origin": "explicit" }
                }}
            }}
        });

        let summary = render_runs_show_summary(&payload).expect("summary");
        assert!(summary.contains("Execution provenance:\n"));
        assert!(summary
            .contains("rerun: homeboy --placement local review --changed-since=origin/main\n"));
        assert!(summary.contains("requested placement: local\n"));
        assert!(summary.contains("resolved execution: controller\n"));
        assert!(summary.contains("policy decision: explicit\n"));
    }

    #[test]
    fn bench_show_summary_surfaces_hotspots_from_metadata() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "bench-run-42",
                    "kind": "bench",
                    "status": "pass",
                    "metadata": {
                        "scenario_metrics": [
                            {
                                "scenario_id": "scenario-a",
                                "metrics": {
                                    "work_ms_per_item": 80.0,
                                    "work_queries_per_item": 11.0
                                }
                            },
                            {
                                "scenario_id": "scenario-b",
                                "metrics": {
                                    "work_ms_per_item": 240.0,
                                    "work_queries_per_item": 23.0
                                }
                            }
                        ]
                    },
                    "artifacts": []
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains("Hotspots:\n"));
        assert!(summary.contains("  Slowest timing metrics:\n"));
        assert!(summary.contains("    scenario-b work_ms_per_item=240\n"));
        assert!(summary.contains("  Hottest metric families:\n"));
        assert!(summary.contains("    work total=34 metrics=2\n"));
        assert!(summary.contains("Artifacts: none recorded\n"));
    }

    #[test]
    fn bench_show_summary_marks_failed_hotspots_from_run_metadata() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "bench-run-42",
                    "kind": "bench",
                    "status": "pass",
                    "metadata": {
                        "scenario_metrics": [
                            {
                                "scenario_id": "admin-page-coverage",
                                "metrics": {
                                    "duration_ms": 42000.0,
                                    "success_rate": 0.0,
                                    "http_error_count": 62.0,
                                    "status_counts": {
                                        "500": 47,
                                        "403": 15
                                    }
                                }
                            }
                        ]
                    },
                    "artifacts": [
                        {
                            "id": "fatal-log",
                            "run_id": "bench-run-42",
                            "scenario_id": "admin-page-coverage",
                            "kind": "log",
                            "type": "file",
                            "path": "/tmp/fatal.log",
                            "fatal_signatures": ["PHP Fatal error: sample"]
                        }
                    ]
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains(
            "admin-page-coverage duration_ms=42000 [failed: success_rate=0 http_errors=62 statuses=403:15,500:47 fatal=PHP Fatal error: sample]\n"
        ));
        assert!(summary.contains("  Failure context:\n"));
        assert!(summary.contains(
            "    admin-page-coverage: success_rate=0 http_errors=62 statuses=403:15,500:47 fatal=PHP Fatal error: sample\n"
        ));
    }

    #[test]
    fn bench_show_summary_surfaces_coverage_from_metadata() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "bench-run-42",
                    "kind": "bench",
                    "status": "pass",
                    "metadata": {
                        "coverage_summary": {
                            "surface_count": 44,
                            "exercised_count": 30,
                            "skipped_count": 8,
                            "failed_count": 1,
                            "coverage_gaps": [
                                "api::create",
                                "api::delete",
                                "cli::delete"
                            ]
                        }
                    },
                    "artifacts": []
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains("Coverage:\n"));
        assert!(
            summary.contains("  Surfaces: discovered=44 exercised=30 skipped_unsafe=8 failed=1\n")
        );
        assert!(summary.contains("  Coverage gaps: 3\n"));
        assert!(summary.contains("    api: 2\n"));
        assert!(summary.contains("    cli: 1\n"));
    }

    #[test]
    fn fuzz_show_summary_surfaces_generic_coverage_and_case_artifacts() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "fuzz-run-7",
                    "kind": "fuzz",
                    "status": "fail",
                    "metadata": {
                        "coverage_summary": {
                            "declared_count": 12,
                            "executable_count": 10,
                            "proven_count": 9,
                            "surface_count": 12,
                            "operation_count": 18,
                            "exercised_count": 9,
                            "failed_count": 2,
                            "skipped_reason_counts": {
                                "requires_confirmation": 2,
                                "missing_fixture": 1
                            },
                            "coverage_gaps": [
                                "parser::unicode",
                                "parser::empty",
                                "serializer::nested"
                            ]
                        }
                    },
                    "artifacts": [
                        {
                            "id": "seed-1",
                            "run_id": "fuzz-run-7",
                            "kind": "failing_case",
                            "type": "file",
                            "path": "/tmp/fuzz/failing-case.json"
                        },
                        {
                            "id": "repro-1",
                            "run_id": "fuzz-run-7",
                            "name": "repro-case",
                            "type": "file",
                            "path": "/tmp/fuzz/repro.txt"
                        },
                        {
                            "id": "coverage-report",
                            "run_id": "fuzz-run-7",
                            "kind": "coverage",
                            "type": "file",
                            "path": "/tmp/fuzz/coverage.json"
                        }
                    ]
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains("Run fuzz-run-7 (fuzz)\n"));
        assert!(summary.contains("Coverage:\n"));
        assert!(summary.contains("  Surfaces: discovered=12 exercised=9 failed=2\n"));
        assert!(summary.contains("  Proof states: declared=12 executable=10 proven=9\n"));
        assert!(summary.contains("  Operations: 18\n"));
        assert!(summary.contains("  Coverage gaps: 3\n"));
        assert!(summary.contains("  Skipped reasons:\n"));
        assert!(summary.contains("    requires_confirmation: 2\n"));
        assert!(summary.contains("    missing_fixture: 1\n"));
        assert!(summary.contains("    parser: 2\n"));
        assert!(summary.contains("    serializer: 1\n"));
        assert!(summary.contains("Key artifacts:\n"));
        assert!(summary.contains("  global/seed-1: /tmp/fuzz/failing-case.json\n"));
        assert!(summary.contains("  global/repro-case: /tmp/fuzz/repro.txt\n"));
        assert!(summary.contains("  global/coverage-report: /tmp/fuzz/coverage.json\n"));
        assert!(
            summary.contains("    get: homeboy runs artifact get fuzz-run-7 seed-1 -o <path>\n")
        );
        assert!(!summary.contains("Reports:\n"));
    }

    #[test]
    fn bench_show_summary_filters_followup_reports_by_scenario_when_available() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "bench-run-42",
                    "kind": "bench",
                    "status": "pass",
                    "component_id": "homeboy",
                    "metadata": {
                        "scenario_metrics": [{"scenario_id": "cold", "metrics": {"p95_ms": 42.0}}]
                    },
                    "artifacts": []
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains(
            "  history: homeboy runs list --kind bench --component homeboy --scenario cold\n"
        ));
        assert!(summary.contains(
            "  distribution: homeboy runs distribution --kind bench --component homeboy --scenario cold --field <metadata.path>\n"
        ));
    }

    #[test]
    fn fuzz_show_summary_surfaces_generic_hotspots_from_artifacts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let artifact_path = temp.path().join("fuzz-results.json");
        std::fs::write(
            &artifact_path,
            serde_json::json!({
                "schema": "homeboy/fuzz-campaign/v1",
                "id": "campaign-1",
                "hotspots": [
                    { "id": "parser::unicode", "score": 4.5, "label": "Unicode parser" },
                    { "id": "serializer::nested", "count": 2 }
                ]
            })
            .to_string(),
        )
        .expect("write artifact");
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "fuzz-run-7",
                    "kind": "fuzz",
                    "status": "fail",
                    "metadata": {},
                    "artifacts": [
                        {
                            "id": "fuzz-results",
                            "run_id": "fuzz-run-7",
                            "kind": "fuzz_results",
                            "type": "file",
                            "path": artifact_path
                        }
                    ]
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains("Hotspots:\n"));
        assert!(summary.contains("  Fuzz hotspots:\n"));
        assert!(summary
            .contains("    #1 parser::unicode (Unicode parser) score=4.5 occurrences=1 runs=1\n"));
        assert!(summary.contains("    #2 serializer::nested score=2 occurrences=1 runs=1\n"));
    }

    #[test]
    fn bench_show_summary_surfaces_regression_threshold_metadata() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "bench-run-42",
                    "kind": "bench",
                    "status": "fail",
                    "metadata": {
                        "baseline_thresholds": [
                            {
                                "scenario_id": "generic-case",
                                "metric": "work_units",
                                "current_value": 60.0,
                                "baseline_value": 50.0,
                                "threshold_value": 5.0,
                                "passed": false
                            }
                        ]
                    },
                    "artifacts": []
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains("Regression thresholds:\n"));
        assert!(
            summary.contains("  generic-case work_units current=60 baseline=50 threshold=5 FAIL\n")
        );
    }

    #[test]
    fn show_summary_surfaces_key_artifacts_before_full_artifact_list() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "run-1",
                    "kind": "test",
                    "status": "pass",
                    "metadata": {},
                    "artifacts": [
                        {
                            "id": "artifact-coverage",
                            "run_id": "run-1",
                            "scenario_id": "scenario-a",
                            "kind": "coverage",
                            "type": "file",
                            "path": "/tmp/coverage.json"
                        },
                        {
                            "id": "artifact-log",
                            "run_id": "run-1",
                            "scenario_id": "scenario-a",
                            "kind": "log",
                            "type": "file",
                            "path": "/tmp/log.txt"
                        }
                    ]
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");
        let key_index = summary.find("Key artifacts:\n").expect("key artifacts");
        let artifact_index = summary.find("Artifacts (2):\n").expect("artifacts");

        assert!(key_index < artifact_index);
        assert!(summary.contains("  scenario-a/artifact-coverage: /tmp/coverage.json\n"));
        assert!(summary
            .contains("    get: homeboy runs artifact get run-1 artifact-coverage -o <path>\n"));
        assert!(!summary.contains("Key artifacts:\n  scenario-a/artifact-log"));
    }

    #[test]
    fn failed_lab_show_summary_promotes_nested_recipe_and_browser_artifact_failures() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact_path = dir.path().join("sample-runtime-result.json");
        std::fs::write(
            &artifact_path,
            serde_json::to_vec(&json!({
                "success": false,
                "provider": "sample-runtime",
                "result": {
                    "recipe": {
                        "status": "failed",
                        "diagnostics": [{
                            "class": "recipe_validation",
                            "message": "Recipe validation failed: missing required step id"
                        }]
                    },
                    "browser": {
                        "status": "failed",
                        "error": {
                            "message": "Browser assertion failed: expected checkout button"
                        }
                    }
                }
            }))
            .expect("serialize fixture"),
        )
        .expect("write artifact");
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "lab-run-1",
                    "kind": "runner-exec",
                    "status": "fail",
                    "metadata": {},
                    "artifacts": [{
                        "id": "sandbox-result",
                        "run_id": "lab-run-1",
                        "kind": "selected_runtime_result",
                        "type": "file",
                        "mime": "application/json",
                        "path": artifact_path.display().to_string()
                    }]
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains("Failure summary:\n"));
        assert!(summary.contains(
            "  recipe: Recipe validation failed: missing required step id (artifact sandbox-result [selected_runtime_result])\n"
        ));
        assert!(summary.contains(
            "  browser: Browser assertion failed: expected checkout button (artifact sandbox-result [selected_runtime_result])\n"
        ));
        let failure_index = summary.find("Failure summary:\n").expect("failure summary");
        let artifact_index = summary.find("Artifacts (1):\n").expect("artifacts");
        assert!(failure_index < artifact_index);
    }

    #[test]
    fn failed_lab_show_summary_distinguishes_wrapper_parser_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let artifact_path = dir.path().join("structured-result.json");
        std::fs::write(&artifact_path, b"{ not json").expect("write artifact");
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "lab-run-2",
                    "kind": "runner-exec",
                    "status": "failed",
                    "metadata": {
                        "wrapper": {
                            "status": "failed",
                            "error": {
                                "code": "structured_output.parse_failed",
                                "message": "Could not parse Managed Sandbox structured output"
                            }
                        }
                    },
                    "artifacts": [{
                        "id": "structured-result",
                        "run_id": "lab-run-2",
                        "kind": "selected_runtime_result",
                        "type": "file",
                        "mime": "application/json",
                        "path": artifact_path.display().to_string()
                    }]
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");

        assert!(summary.contains(
            "  wrapper/parser: Could not parse Managed Sandbox structured output (metadata)\n"
        ));
        assert!(summary.contains("  wrapper/parser: could not parse structured artifact JSON:"));
        assert!(!summary.contains("  recipe:"));
        assert!(!summary.contains("  browser:"));
    }

    #[test]
    fn show_summary_reports_no_artifacts() {
        let payload = json!({
            "variant": "show",
            "payload": {
                "command": "runs.show",
                "run": {
                    "id": "run-1",
                    "kind": "test",
                    "status": "fail",
                    "started_at": "2026-06-19T00:00:00Z",
                    "finished_at": null,
                    "metadata": {},
                    "artifacts": []
                }
            }
        });

        let summary = render_runs_show_summary(&payload).expect("summary");
        assert!(summary.contains("Artifacts: none recorded\n"));
        assert!(summary.contains("full run: homeboy runs show run-1 --json\n"));
    }

    #[test]
    fn failed_show_summary_bounds_cleanup_inventory_without_losing_recovery_or_primary_artifact() {
        let mut artifacts = (1..=1_000)
            .map(|index| {
                json!({
                    "id": format!("cleanup-entry-{index}"),
                    "run_id": "failed-run",
                    "kind": "cleanup_inventory",
                    "type": "file",
                    "path": format!("/tmp/cleanup-{index}.json")
                })
            })
            .collect::<Vec<_>>();
        artifacts.push(json!({
            "id": "failure-report",
            "run_id": "failed-run",
            "kind": "raw_result",
            "type": "file",
            "path": "/tmp/failure-report.json"
        }));
        let payload = json!({
            "variant": "show",
            "payload": { "run": {
                "id": "failed-run",
                "kind": "runner-exec",
                "status": "fail",
                "metadata": {
                    "error": "selected runtime exited 1",
                    "recovery_commands": ["homeboy runner exec lab-1 -- retry failed-run"],
                    "cleanup": {
                        "recovery_commands": ["must-not-render cleanup recovery"]
                    }
                },
                "artifacts": artifacts
            }}
        });

        let summary = render_runs_show_summary(&payload).expect("summary");
        let failure_index = summary.find("Failure summary:\n").expect("failure summary");
        let recovery_index = summary.find("Recovery:\n").expect("recovery commands");
        let run_index = summary
            .find("Run failed-run (runner-exec)\n")
            .expect("run identity");
        let artifact_index = summary
            .find("Primary artifacts (1 of 1001):\n")
            .expect("primary artifacts");

        assert!(summary.starts_with("Status: fail\nFailure summary:\n"));
        assert!(failure_index < run_index);
        assert!(recovery_index < run_index);
        assert!(failure_index < artifact_index);
        assert!(recovery_index < artifact_index);
        assert!(summary.contains("selected runtime exited 1"));
        assert!(summary.contains("homeboy runner exec lab-1 -- retry failed-run"));
        assert!(summary.contains("failure-report [raw_result]: /tmp/failure-report.json"));
        assert!(summary.contains(
            "omitted: 1000 artifact records; inspect the full inventory with `homeboy runs artifacts failed-run`"
        ));
        assert!(summary.contains("failure and evidence: homeboy runs evidence failed-run"));
        assert!(summary.contains("full artifact inventory: homeboy runs artifacts failed-run"));
        assert!(!summary.contains("cleanup-entry-1"));
        assert!(!summary.contains("cleanup-entry-1000"));
        assert!(!summary.contains("must-not-render cleanup recovery"));
        assert!(summary.len() < 3_000, "summary must stay bounded");
    }
}
