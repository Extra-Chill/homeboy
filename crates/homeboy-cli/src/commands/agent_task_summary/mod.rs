use serde_json::Value;

use super::agent_task::candidate::{classify_candidates, CandidateState};
use super::agent_task::{AgentTaskArgs, AgentTaskCommand};
use super::summary_json::{array_len, string_value, u64_value, usize_value, value_at};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentTaskSummaryKind {
    Cook,
    Recover,
    Status,
    Logs,
    Review,
    Providers,
    Capacity,
}

pub(crate) fn agent_task_summary_kind(args: &AgentTaskArgs) -> Option<AgentTaskSummaryKind> {
    match &args.command {
        AgentTaskCommand::Cook(_) => Some(AgentTaskSummaryKind::Cook),
        AgentTaskCommand::Recover(_) => Some(AgentTaskSummaryKind::Recover),
        AgentTaskCommand::Status(_) => Some(AgentTaskSummaryKind::Status),
        AgentTaskCommand::Review(_) => Some(AgentTaskSummaryKind::Review),
        AgentTaskCommand::Providers(_) => Some(AgentTaskSummaryKind::Providers),
        AgentTaskCommand::Capacity(_) => Some(AgentTaskSummaryKind::Capacity),
        _ => None,
    }
}

pub(crate) fn render_agent_task_summary(
    kind: AgentTaskSummaryKind,
    payload: &Value,
) -> Option<String> {
    let summary = match kind {
        AgentTaskSummaryKind::Cook => render_cook_summary(payload),
        AgentTaskSummaryKind::Recover => Some(render_recover_summary(payload)),
        AgentTaskSummaryKind::Status => render_status_summary(payload),
        AgentTaskSummaryKind::Logs => render_logs_summary(payload),
        AgentTaskSummaryKind::Review => render_review_summary(payload),
        AgentTaskSummaryKind::Providers => render_providers_summary(payload),
        AgentTaskSummaryKind::Capacity => render_capacity_summary(payload),
    };
    summary.map(|mut summary| {
        if matches!(
            kind,
            AgentTaskSummaryKind::Cook
                | AgentTaskSummaryKind::Status
                | AgentTaskSummaryKind::Recover
        ) && !summary.starts_with("Cook preview")
            && !summary.contains("Placement:")
        {
            summary.truncate(summary.trim_end().len());
            summary.push('\n');
            summary.push_str(&super::run_placement::line(payload));
            for child in payload["children"].as_array().into_iter().flatten() {
                let id = child["child_id"]
                    .as_str()
                    .or(child["run_id"].as_str())
                    .unwrap_or("child");
                summary.push_str(&format!("\n  {id}: {}", super::run_placement::line(child)));
            }
        }
        summary
    })
}

fn render_recover_summary(payload: &Value) -> String {
    let run_id = string_value(payload, &["run_id"]).unwrap_or("unknown");
    let mut lines = vec![
        format!(
            "Recovery step: {}",
            payload
                .pointer("/step/kind")
                .or_else(|| payload.get("step"))
                .and_then(Value::as_str)
                .unwrap_or("none")
        ),
        format!("Attempt: {run_id}"),
        format!(
            "Status: {}",
            string_value(payload, &["status"]).unwrap_or("unknown")
        ),
    ];
    // An admitted step runs as a step job on the local controller daemon; it
    // is not a fresh execution placement awaiting a decision (#18657).
    if let Some(admission) = payload.get("admission").filter(|value| value.is_object()) {
        if let Some(job) = string_value(admission, &["job_id"]) {
            lines.push(format!("Step job: {job}"));
            lines.push("Placement: local (controller daemon step job)".to_string());
        }
        let watch = string_value(admission, &["status_command"])
            .map(str::to_string)
            .unwrap_or_else(|| format!("homeboy agent-task status {run_id} --watch"));
        lines.push(format!("Next: {watch}"));
    }
    // A refused or failed recovery names why and what to run next, so the
    // human summary is never silent about a nonzero exit (#18845).
    if let Some(failure) = payload.get("failure").filter(|value| value.is_object()) {
        if let Some(message) =
            string_value(failure, &["message"]).filter(|message| !message.trim().is_empty())
        {
            lines.push(format!("Refused: {message}"));
        }
        if let Some(next) =
            string_value(failure, &["next_command"]).filter(|next| !next.trim().is_empty())
        {
            lines.push(format!("Next: {next}"));
        }
    } else if let Some(reason) = payload
        .pointer("/step/reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.trim().is_empty())
    {
        lines.push(format!("Reason: {reason}"));
    }
    lines.join("\n")
}

#[cfg(test)]
#[test]
fn recover_summary_names_the_refusal_and_next_command() {
    let summary = render_agent_task_summary(
        AgentTaskSummaryKind::Recover,
        &serde_json::json!({
            "step": {"kind": "none", "reason": "no admission inputs"},
            "run_id": "cook-1", "status": "failed",
            "failure": {
                "code": "agent_task.recover_no_step",
                "message": "no admission inputs",
                "next_command": "homeboy agent-task diagnose cook-1"
            }
        }),
    )
    .unwrap();
    assert!(
        summary.contains("Refused: no admission inputs"),
        "{summary}"
    );
    assert!(
        summary.contains("Next: homeboy agent-task diagnose cook-1"),
        "{summary}"
    );
}

#[cfg(test)]
#[test]
fn recover_summary_names_the_admitted_step_job_and_how_to_watch_it() {
    let summary = render_agent_task_summary(
        AgentTaskSummaryKind::Recover,
        &serde_json::json!({
            "step": {"kind": "regate"}, "run_id": "cook-attempt-1", "status": "admitted",
            "admission": {
                "status": "admitted", "job_id": "job-1",
                "status_command": "homeboy agent-task status cook-attempt-1 --watch"
            }
        }),
    )
    .unwrap();
    assert!(summary.contains("Recovery step: regate"), "{summary}");
    assert!(summary.contains("Attempt: cook-attempt-1"), "{summary}");
    assert!(summary.contains("Status: admitted"), "{summary}");
    assert!(summary.contains("Step job: job-1"), "{summary}");
    assert!(
        summary.contains("Next: homeboy agent-task status cook-attempt-1 --watch"),
        "{summary}"
    );
    assert!(!summary.contains("Placement: pending"), "{summary}");
}

fn render_providers_summary(payload: &Value) -> Option<String> {
    let summary = payload.get("operator_summary")?;
    let state = summary.get("state")?.as_str()?;
    let provider_count = array_len(payload, &["providers"]).unwrap_or(0);
    let mut lines = vec![
        "Agent task providers".to_string(),
        format!("Status: {state}"),
        format!("Providers shown: {provider_count}"),
    ];
    if state == "selection_required" {
        let choices = summary
            .get("selection_choices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|choice| choice.get("backend").and_then(Value::as_str))
            .collect::<Vec<_>>();
        if !choices.is_empty() {
            lines.push(format!("Choose backend: {}", choices.join(", ")));
        }
    }
    lines.extend(render_provider_capacity_lines(payload));
    if let Some(next_action) = summary.get("next_action").and_then(Value::as_str) {
        lines.push(format!("Next: {next_action}"));
    }
    if let Some(refresh_action) = summary.get("refresh_action").and_then(Value::as_str) {
        lines.push(format!("Refresh: {refresh_action}"));
    }
    Some(lines.join("\n"))
}

/// Live capacity for each provider whose readiness probe published it, with
/// one line per pooled account so every plan's reset instant is visible.
fn render_provider_capacity_lines(payload: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    for provider in payload
        .get("providers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(capacity) = provider.pointer("/dispatchability/capacity") else {
            continue;
        };
        let state = capacity.get("state").and_then(Value::as_str).unwrap_or("");
        if state != "known" && state != "exhausted" {
            continue;
        }
        let label = provider
            .get("id")
            .or_else(|| provider.get("backend"))
            .and_then(Value::as_str)
            .unwrap_or("provider");
        let unit = capacity.get("unit").and_then(Value::as_str);
        let headline = if state == "exhausted" {
            "exhausted".to_string()
        } else {
            capacity
                .get("remaining")
                .map(|remaining| format!("{} remaining", capacity_amount(remaining, unit)))
                .unwrap_or_else(|| "available".to_string())
        };
        lines.push(format!(
            "Capacity {label}: {headline}{}",
            capacity_reset_suffix(capacity)
        ));
        for account in capacity
            .get("accounts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = account
                .get("account")
                .and_then(Value::as_str)
                .unwrap_or("account");
            let account_state = account
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let remaining = match (account_state, account.get("remaining")) {
                ("available", Some(remaining)) => {
                    format!(", {} remaining", capacity_amount(remaining, unit))
                }
                _ => String::new(),
            };
            lines.push(format!(
                "  {name}: {account_state}{remaining}{}",
                capacity_reset_suffix(account)
            ));
        }
    }
    lines
}

fn capacity_amount(remaining: &Value, unit: Option<&str>) -> String {
    match unit {
        Some("percent") => format!("{remaining}%"),
        Some(unit) if !unit.is_empty() => format!("{remaining} {unit}"),
        _ => remaining.to_string(),
    }
}

fn capacity_reset_suffix(value: &Value) -> String {
    value
        .get("reset_at")
        .and_then(Value::as_str)
        .map(|reset_at| format!(", resets {reset_at}"))
        .unwrap_or_default()
}

/// Holders of the host-wide local gate queue, one indented line each.
fn render_local_holders(lines: &mut Vec<String>, local: &Value) {
    for holder in local["holders"].as_array().into_iter().flatten() {
        lines.push(format!(
            "  holder: {} {} ({}s elapsed)",
            holder["cook_id"].as_str().unwrap_or("unknown"),
            holder["gate"].as_str().unwrap_or("unknown"),
            holder["elapsed_seconds"].as_u64().unwrap_or(0)
        ));
    }
}

/// Host-wide local gate queue (#18555): slots, pressure, holders, then
/// waiters in admission order with the Lab step queue's line shape.
fn render_local_capacity(lines: &mut Vec<String>, local: &Value) {
    if local["available"].as_bool() != Some(true) {
        return;
    }
    let pressure = &local["pressure"];
    let load = pressure["load_average_1m"]
        .as_f64()
        .map(|load| format!("load {load:.2} of {}", pressure["max_load"]))
        .unwrap_or_else(|| "load unknown".to_string());
    let held = if pressure["blocked"].as_bool() == Some(true) {
        ", holding new gates"
    } else {
        ""
    };
    lines.push(format!(
        "Local: {} active slots, {} free slots (limit {}); pressure: {load}{held}",
        local["active_slots"], local["free_slots"], local["slot_limit"]
    ));
    render_local_holders(lines, local);
    render_queue_waiters(lines, "Local", local);
}

/// `agent-task capacity` summary (#15024): one block per route group, one
/// indented line per pooled account, so an operator reads every connected
/// plan's state and reset instant at a glance. Reuses the same capacity
/// amount/reset rendering as the `providers` summary above.
fn render_capacity_summary(payload: &Value) -> Option<String> {
    if payload.get("schema")?.as_str()? != "homeboy/agent-task-capacity/v1" {
        return None;
    }
    let routes = payload.get("routes").and_then(Value::as_array)?;
    let mut lines = Vec::new();
    if let Some(local) = payload.get("local_capacity") {
        render_local_capacity(&mut lines, local);
    }
    for route in payload
        .get("lab_routes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let runner = route
            .get("runner_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let slots = |key| {
            route
                .get(key)
                .and_then(Value::as_u64)
                .map(|n| n.to_string())
                .unwrap_or_else(|| "unknown".to_string())
        };
        let mut line = format!(
            "Lab/{runner}: {} active slots, {} free slots",
            slots("active_slots"),
            slots("free_slots")
        );
        if let Some(budget) = route.get("effective_budget").and_then(Value::as_u64) {
            line.push_str(&format!(" (effective budget {budget})"));
        }
        let next = route.get("next_admission").unwrap_or(&Value::Null);
        if next.get("refused").and_then(Value::as_bool) == Some(true) {
            let state = next
                .get("readiness_state")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let reasons = next
                .get("reasons")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            line.push_str(&format!("; next admission waits ({state}"));
            if !reasons.is_empty() {
                line.push_str(&format!(": {}", reasons.join(", ")));
            }
            line.push(')');
        }
        lines.push(line);
    }
    render_lab_step_queue(&mut lines, payload);
    if routes.is_empty() {
        lines.push("Agent task capacity: no configured provider routes".to_string());
    }
    for route in routes {
        let backend = capacity_route_label(route);
        let capacity = route.get("capacity").unwrap_or(&Value::Null);
        let accounts = capacity
            .get("accounts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let unit = capacity.get("unit").and_then(Value::as_str);
        let state = capacity.get("state").and_then(Value::as_str);
        let headline = match state {
            Some("exhausted") => match capacity_exhausted_until(capacity) {
                Some(reset_at) => format!("exhausted until {reset_at}"),
                None => "exhausted".to_string(),
            },
            Some("known") => capacity
                .get("remaining")
                .map(|remaining| format!("{} remaining", capacity_amount(remaining, unit)))
                .unwrap_or_else(|| "available".to_string()),
            _ => "capacity not published".to_string(),
        };
        if accounts.is_empty() {
            // An exhausted headline already names its reset instant.
            let suffix = if state == Some("exhausted") {
                String::new()
            } else {
                capacity_reset_suffix(capacity)
            };
            lines.push(format!("{backend}: {headline}{suffix}"));
        } else {
            lines.push(format!("{backend} ({} plans): {headline}", accounts.len()));
        }
        for account in accounts {
            let name = account
                .get("account")
                .and_then(Value::as_str)
                .unwrap_or("account");
            let state = account
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let mut line = format!("  {name}  {state}");
            if state == "available" {
                if let Some(remaining) = account.get("remaining") {
                    line.push_str(&format!("  {}", capacity_amount(remaining, unit)));
                }
            }
            if let Some(reset_at) = account.get("reset_at").and_then(Value::as_str) {
                line.push_str(&format!("  resets {reset_at}"));
            }
            lines.push(line);
        }
    }
    Some(lines.join("\n"))
}

/// One line per Lab step waiter, in admission order, with its upper-bound ETA.
fn render_lab_step_queue(lines: &mut Vec<String>, payload: &Value) {
    for runner in payload
        .get("lab_capacity")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let runner_id = runner["runner_id"].as_str().unwrap_or("unknown");
        render_queue_waiters(
            lines,
            &format!("Lab/{runner_id}"),
            &runner["observation"]["step_queue"],
        );
    }
}

/// One line per waiter of a ranked queue (Lab step queue or local gate
/// queue), in admission order, with its upper-bound ETA.
fn render_queue_waiters(lines: &mut Vec<String>, label: &str, queue: &Value) {
    for waiter in queue["waiting"].as_array().into_iter().flatten() {
        let position = waiter["queue_position"].as_u64().unwrap_or(0);
        let eta = waiter["eta_seconds"]
            .as_u64()
            .map(|seconds| format!(", ETA <= {seconds}s (estimate from declared budgets)"))
            .unwrap_or_else(|| ", ETA unknown".to_string());
        // Remote waiters are Cooks other controllers registered on this Lab.
        let origin = if waiter["remote"] == true {
            ", remote controller"
        } else {
            ""
        };
        lines.push(format!(
            "  {label} queued {position}: {} ({} lane{origin}, {} ahead, {} holders){eta}",
            waiter["owner"].as_str().unwrap_or("unknown"),
            waiter["priority"].as_str().unwrap_or("normal"),
            position.saturating_sub(1),
            queue["holders"].as_array().map_or(0, Vec::len),
        ));
    }
}

/// Name a capacity route by its account pool (`pi:anthropic` reads as
/// `anthropic`), else its models, else its backend, so blocks on one backend
/// stay distinguishable.
fn capacity_route_label(route: &Value) -> String {
    if let Some(scope) = route
        .pointer("/capacity/scope")
        .or_else(|| route.get("scope"))
        .and_then(Value::as_str)
        .filter(|scope| !scope.trim().is_empty())
    {
        return scope.rsplit(':').next().unwrap_or(scope).to_string();
    }
    let models = route
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    if !models.is_empty() {
        return models.join(", ");
    }
    route
        .get("backend")
        .and_then(Value::as_str)
        .unwrap_or("route")
        .to_string()
}

/// The soonest known reset among a capacity entry's exhausted accounts, so an
/// exhausted pool reads as "exhausted until X" rather than a dead end. Falls
/// back to the route-level reset instant when the accounts name none.
fn capacity_exhausted_until(capacity: &Value) -> Option<String> {
    let account_resets = capacity
        .get("accounts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|account| account.get("state").and_then(Value::as_str) == Some("exhausted"))
        .filter_map(|account| account.get("reset_at").and_then(Value::as_str))
        .collect::<Vec<_>>();
    let soonest = if account_resets.is_empty() {
        capacity
            .get("reset_at")
            .and_then(Value::as_str)
            .map(str::to_string)
    } else {
        account_resets
            .iter()
            .filter_map(|reset_at| {
                chrono::DateTime::parse_from_rfc3339(reset_at)
                    .ok()
                    .map(|instant| (instant, (*reset_at).to_string()))
            })
            .min_by_key(|(instant, _)| *instant)
            .map(|(_, raw)| raw)
    };
    soonest.filter(|reset_at| !reset_at.trim().is_empty())
}

fn render_cook_summary(payload: &Value) -> Option<String> {
    if payload.get("schema").and_then(Value::as_str) == Some("homeboy/agent-task-cook-preview/v1") {
        return render_cook_preview_summary(payload);
    }
    let run_id =
        string_value(payload, &["run_id"]).or_else(|| string_value(payload, &["latest_run_id"]))?;
    let raw_state = string_value(payload, &["state"])
        .or_else(|| string_value(payload, &["record", "state"]))
        .or_else(|| string_value(payload, &["status"]))
        .unwrap_or("unknown");
    let durable_unavailable =
        string_value(payload, &["durable_candidate", "status"]) == Some("unavailable");
    let tasks_planned = usize_value(payload, &["task_count"])
        .or_else(|| usize_value(payload, &["durable_candidate", "task_count"]))
        .or_else(|| array_len(payload, &["record", "tasks"]));
    let canonical = classify_candidates(payload);
    let tasks_attempted = usize_value(payload, &["durable_candidate", "provider_execution_count"])
        .or(canonical.provider_executions)
        .or_else(|| aggregate_outcome_count(payload));
    let aggregate_path = string_value(payload, &["aggregate_path"])
        .or_else(|| string_value(payload, &["durable_candidate", "aggregate_path"]))
        .or_else(|| string_value(payload, &["record", "aggregate_path"]));
    let metrics = code_production_metrics(payload);
    let state = effective_run_state(
        raw_state,
        tasks_attempted.unwrap_or(0),
        metrics.candidate_state,
        metrics.candidate_scan_degraded,
    );
    let artifact_count = usize_value(payload, &["durable_candidate", "artifact_count"])
        .or_else(|| Some(aggregate_artifact_count(payload)).filter(|_| !durable_unavailable));
    let first_artifact = string_value(
        payload,
        &["aggregate", "outcomes", "0", "artifacts", "0", "path"],
    )
    .or_else(|| {
        string_value(
            payload,
            &["aggregate", "outcomes", "0", "artifacts", "0", "id"],
        )
    });

    let primary_failure = payload
        .get("primary_failure")
        .filter(|value| value.is_object());
    let mut lines = vec!["Agent task cook".to_string()];
    if let Some(failure) = primary_failure {
        lines.push(format!(
            "Primary failure: provider {} {} failed during {} (exit {})",
            string_value(failure, &["provider_id"]).unwrap_or("unknown"),
            string_value(failure, &["operation"]).unwrap_or("unknown operation"),
            string_value(failure, &["phase"]).unwrap_or("pre-execution"),
            failure
                .get("exit_code")
                .and_then(Value::as_i64)
                .map(|code| code.to_string())
                .unwrap_or_else(|| "unknown".to_string()),
        ));
        let stderr = string_value(failure, &["stderr_excerpt"]).unwrap_or("");
        lines.push(format!(
            "Provider stderr: {}",
            if stderr.is_empty() { "(empty)" } else { stderr }
        ));
        if let Some(command) = string_value(failure, &["next_action", "command"]) {
            lines.push(format!("Next: {command}"));
        }
    }
    let unavailable_or_zero = || {
        if durable_unavailable {
            "unavailable".to_string()
        } else {
            "0".to_string()
        }
    };
    lines.extend([
        format!("Run: {run_id}"),
        format!("Status: {state}"),
        format!(
            "Tasks planned: {}",
            tasks_planned
                .map(|count| count.to_string())
                .unwrap_or_else(unavailable_or_zero)
        ),
        format!(
            "Tasks attempted: {}",
            tasks_attempted
                .map(|count| count.to_string())
                .unwrap_or_else(unavailable_or_zero)
        ),
    ]);
    // One bounded line per provider route the run skipped as unready before
    // dispatch (#15881): a credentials-dead primary is operator-visible, not
    // only an event-log line.
    if let Some(notes) = payload
        .get("provider_route_notes")
        .and_then(Value::as_array)
    {
        for note in notes.iter().filter_map(Value::as_str).take(4) {
            lines.push(note.to_string());
        }
    }
    if durable_unavailable {
        let reason = string_value(payload, &["durable_candidate", "reason"])
            .unwrap_or("authoritative candidate evidence could not be read");
        lines.push(format!("Candidate evidence: unavailable ({reason})"));
    } else {
        lines.extend(code_production_lines(&metrics));
    }
    if let Some(path) = aggregate_path {
        lines.push(format!("Aggregate: {path}"));
    }
    lines.push(format!(
        "Artifacts: {}",
        artifact_count
            .map(|count| count.to_string())
            .unwrap_or_else(unavailable_or_zero)
    ));
    if let Some(artifact) = first_artifact {
        lines.push(format!("First artifact: {artifact}"));
    }
    if string_value(payload, &["selected_candidate", "selected_task_id"]).is_some() {
        if let Some(task) = string_value(payload, &["selected_candidate", "selected_task_id"]) {
            let artifact = string_value(payload, &["selected_candidate", "selected_candidate_sha"])
                .unwrap_or("unknown");
            lines.push(format!(
                "Retained candidate: task {task}, artifact {artifact}"
            ));
        }
    }
    if primary_failure.is_some() {
        // Its exact action already leads the report.
    } else if let Some(command) = string_value(payload, &["promotion_replay", "command"]) {
        lines.push(format!("Next: {command}"));
    } else if payload
        .pointer("/promotion_replay/available")
        .and_then(Value::as_bool)
        == Some(false)
    {
        let reason = string_value(payload, &["promotion_replay", "reason"])
            .unwrap_or("promotion source is unavailable");
        lines.push(format!("Promotion unavailable: {reason}"));
        if let Some(remediation) = string_value(payload, &["promotion_replay", "remediation"]) {
            lines.push(format!("Remediation: {remediation}"));
        }
    } else if metrics.candidate_state.is_available() {
        lines.push(format!("Next: homeboy agent-task review {run_id}"));
    } else {
        lines.push(format!("Next: homeboy runs logs {run_id}"));
    }
    Some(finish(lines))
}

fn render_cook_preview_summary(payload: &Value) -> Option<String> {
    let resolved = payload.get("resolved")?;
    let requested_placement = resolved
        .pointer("/placement/requested")
        .and_then(Value::as_str)?;
    // `selected` is the resolved answer to "where will this actually run";
    // `requested` is only what the operator asked for. Reporting the request
    // as though it were the outcome is how a silent auto -> local degrade went
    // unreported in preview (#14729).
    let selected_placement = resolved
        .pointer("/placement/selected")
        .and_then(Value::as_str)
        .unwrap_or(requested_placement);
    let admission_state = resolved
        .pointer("/placement/admission/state")
        .and_then(Value::as_str);
    let admission_suffix = match admission_state {
        Some("blocked") => " (admission blocked)",
        Some("indeterminate") => " (admission indeterminate)",
        _ => "",
    };
    let placement_line = if selected_placement == requested_placement {
        format!("Placement: {selected_placement}{admission_suffix}")
    } else {
        let reason = resolved
            .pointer("/placement/fallback_reason")
            .and_then(Value::as_str)
            .map(|reason| format!(" — {reason}"))
            .unwrap_or_default();
        format!(
            "Placement: {selected_placement}{admission_suffix}  (requested: {requested_placement}{reason})"
        )
    };
    let placement_line = if let Some(summary) = resolved
        .pointer("/placement/admission/daemon/summary")
        .and_then(Value::as_str)
    {
        format!("{placement_line}\nDaemon: {summary}")
    } else {
        placement_line
    };
    let provider = resolved
        .get("provider")
        .and_then(Value::as_object)
        .and_then(|provider| {
            provider.get("backend").and_then(|backend| {
                backend
                    .as_str()
                    .or_else(|| backend.get("state").and_then(Value::as_str))
            })
        })
        .unwrap_or("unresolved");
    let model = resolved
        .pointer("/provider/model")
        .and_then(Value::as_str)
        .unwrap_or("unresolved");
    let destination = resolved
        .pointer("/workspace/path")
        .and_then(Value::as_str)
        .or_else(|| resolved.get("worktree").and_then(Value::as_str))
        .or_else(|| {
            resolved
                .pointer("/workspace/action")
                .and_then(Value::as_str)
        })
        .unwrap_or("unresolved");
    let public_gates = resolved
        .pointer("/gates/public")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let private_gates = resolved
        .pointer("/gates/private")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let replay = payload
        .get("replay_argv")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let mut lines = vec![
        "Cook preview".to_string(),
        placement_line,
        format!("Provider: {provider}"),
        format!("Model: {model}"),
        format!("Destination: {destination}"),
        format!("Gates: {public_gates} public, {private_gates} private"),
        format!(
            "Replay: {}",
            homeboy::core::engine::shell::quote_args(&replay)
        ),
    ];
    if let Some(limit) = resolved
        .pointer("/gates/timeout_seconds")
        .and_then(Value::as_u64)
    {
        let no_progress = resolved
            .pointer("/gates/no_progress_timeout_seconds")
            .and_then(Value::as_u64)
            .map(|seconds| format!(", no-progress limit {seconds}s"))
            .unwrap_or_default();
        lines.insert(
            6,
            format!(
                "Gate limits: {limit}s per gate{no_progress} (on Lab, dependency setup counts toward the gate limit)"
            ),
        );
    }
    if let Some(deferred) = resolved
        .pointer("/gate_contract_validation/deferred_public")
        .and_then(Value::as_array)
    {
        for gate in deferred {
            let kind = gate.get("kind").and_then(Value::as_str).unwrap_or("gate");
            let filter = gate
                .get("filter")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            let interpretation = gate
                .get("filter_interpretation")
                .and_then(Value::as_str)
                .unwrap_or("focused");
            let reason = match gate.get("validation").and_then(Value::as_str) {
                Some("test_inventory_unavailable_without_build") => {
                    "test inventory unavailable without building test binaries"
                }
                Some(other) => other,
                None => "selection not verified",
            };
            lines.push(format!(
                "Gate selection deferred: {kind} filter `{filter}` ({interpretation}) — {reason}"
            ));
        }
    }
    let deferred_private = resolved
        .pointer("/gate_contract_validation/deferred_private_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if deferred_private > 0 {
        lines.push(format!(
            "Private gate selections deferred: {deferred_private} (details withheld)"
        ));
    }
    if let Some(failure) = payload.pointer("/failure/message").and_then(Value::as_str) {
        lines.push(format!("Blocked: {failure}"));
    }
    if let Some(next_action) = payload
        .pointer("/failure/next_action")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .pointer("/resolved/placement/admission/next_action")
                .and_then(Value::as_str)
        })
    {
        lines.push(format!("Next: {next_action}"));
    }
    if payload
        .pointer("/resolved/placement/admission/state")
        .and_then(Value::as_str)
        == Some("indeterminate")
        && payload.pointer("/failure/message").is_none()
    {
        lines.push(
            payload
                .pointer("/resolved/placement/admission/reason")
                .and_then(Value::as_str)
                .unwrap_or("static inputs only; admission not checked")
                .to_string(),
        );
    }
    Some(finish(lines))
}

#[test]
fn agent_task_status_truncates_only_at_word_boundaries() {
    for (text, limit, expected) in [
        ("records a concrete remedy", 18, "records a…"),
        ("短い 言葉 長い説明", 7, "短い 言葉…"),
        ("unbrokenword", 5, "…"),
        ("short text", 10, "short text"),
    ] {
        assert_eq!(compact_status_text(text, limit), expected);
    }
    let message = "records a concrete remedy ".repeat(20);
    let payload = serde_json::json!({"schema": "homeboy/control-plane-run/v1", "run": "run", "state": "failed", "blocker": {"message": message}});
    let summary = render_status_summary(&payload).unwrap();
    assert!(summary.contains('…'));
    assert_eq!(payload["blocker"]["message"], message);
}

#[test]
fn agent_task_capacity_names_lab_runner_and_slots() {
    let payload = serde_json::json!({
        "schema": "homeboy/agent-task-capacity/v1",
        "routes": [{"backend": "roadie", "model": "subagent"}],
        "lab_routes": [{"runner_id": "lab-a", "active_slots": 2, "free_slots": 3}]
    });
    let summary = render_capacity_summary(&payload).unwrap();
    assert!(summary.contains("Lab/lab-a: 2 active slots, 3 free slots"));
    assert!(summary.contains("capacity not published"));
}

#[test]
fn agent_task_capacity_names_the_admission_refusal_reason() {
    let payload = serde_json::json!({
        "schema": "homeboy/agent-task-capacity/v1",
        "routes": [],
        "lab_routes": [{
            "runner_id": "homeboy-lab", "active_slots": 11, "free_slots": 0,
            "effective_budget": 1,
            "next_admission": {"refused": true, "readiness_state": "capacity_blocked", "reasons": ["pressure"]}
        }]
    });
    let summary = render_capacity_summary(&payload).unwrap();
    assert!(
        summary.contains("Lab/homeboy-lab: 11 active slots, 0 free slots (effective budget 1); next admission waits (capacity_blocked: pressure)"),
        "{summary}"
    );
}

#[test]
fn agent_task_capacity_lists_lab_step_queue_with_eta() {
    let payload = serde_json::json!({
        "schema": "homeboy/agent-task-capacity/v1",
        "routes": [],
        "lab_capacity": [{"runner_id": "lab-a", "observation": {"step_queue": {
            "holders": [{"owner": "wf-1"}, {"owner": "wf-1"}],
            "waiting": [
                {"queue_position": 1, "owner": "publish", "priority": "high", "eta_seconds": 90},
                {"queue_position": 2, "owner": "wf-1", "priority": "normal", "eta_seconds": null},
                {"queue_position": 3, "owner": "run-b", "priority": "normal", "remote": true, "eta_seconds": 300}
            ]
        }}}]
    });
    let summary = render_capacity_summary(&payload).unwrap();
    assert!(summary.contains(
        "Lab/lab-a queued 1: publish (high lane, 0 ahead, 2 holders), ETA <= 90s (estimate from declared budgets)"
    ), "{summary}");
    assert!(
        summary.contains("Lab/lab-a queued 2: wf-1 (normal lane, 1 ahead, 2 holders), ETA unknown"),
        "{summary}"
    );
    assert!(
        summary.contains("Lab/lab-a queued 3: run-b (normal lane, remote controller, 2 ahead, 2 holders), ETA <= 300s"),
        "{summary}"
    );
}

#[test]
fn local_capacity_admission_summary_exposes_queue_and_holders() {
    let local = serde_json::json!({
        "available": true, "active_slots": 1, "free_slots": 0, "slot_limit": 1,
        "pressure": {"load_average_1m": 2.5, "max_load": 8.0, "blocked": false},
        "holders": [{"owner": "cook-a", "cook_id": "cook-a", "gate": "gate-1", "elapsed_seconds": 50}],
        "waiting": [{"queue_position": 1, "owner": "wf-import", "cook_id": "cook-b", "gate": "gate-2",
            "priority": "normal", "waited_seconds": 20, "eta_seconds": 40}]
    });
    let capacity = serde_json::json!({"schema": "homeboy/agent-task-capacity/v1", "routes": [], "local_capacity": local});
    let summary = render_capacity_summary(&capacity).unwrap();
    assert!(
        summary
            .contains("Local: 1 active slots, 0 free slots (limit 1); pressure: load 2.50 of 8.0"),
        "{summary}"
    );
    assert!(
        summary.contains("holder: cook-a gate-1 (50s elapsed)"),
        "{summary}"
    );
    assert!(
        summary.contains("Local queued 1: wf-import (normal lane, 0 ahead, 1 holders), ETA <= 40s"),
        "{summary}"
    );
    let status = serde_json::json!({"schema": "homeboy/control-plane-run/v1", "run": "cook-b", "state": "running",
        "local_admission": {"queue_position": 1, "waiters_ahead": 0, "priority": "normal",
            "eta_seconds": 40, "holders": local["holders"]}});
    let summary = render_status_summary(&status).unwrap();
    assert!(
        summary.contains("Local gate queue position 1 (normal lane, 0 ahead), ETA <= 40s"),
        "{summary}"
    );
    assert!(summary.contains("holder: cook-a gate-1"), "{summary}");
}

#[test]
fn agent_task_status_names_missing_arguments() {
    let payload = serde_json::json!({"schema": "homeboy/control-plane-run/v1", "run": "run", "state": "failed",
        "blocker": {"message": "Missing required argument", "code": "validation.missing_argument", "details": {"args": ["prompt", "component"]}}});
    assert!(render_status_summary(&payload)
        .unwrap()
        .contains("Missing required argument: prompt, component"));
}

fn compact_status_text(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let prefix: String = text.chars().take(limit.saturating_sub(1)).collect();
    let boundary = prefix.rfind(char::is_whitespace).unwrap_or(0);
    format!("{}…", prefix[..boundary].trim_end())
}

fn render_status_summary(payload: &Value) -> Option<String> {
    if string_value(payload, &["schema"])? != "homeboy/control-plane-run/v1" {
        return None;
    }
    let run_id = string_value(payload, &["run"])?;
    let state = string_value(payload, &["state"]).unwrap_or("unknown");
    // The headline and the JSON envelope's `status` both name the task's own
    // state from this one projection, so they cannot disagree (#18638).
    let mut lines = vec![format!("Agent task {state}"), format!("Run: {run_id}")];
    if let Some(phase) = string_value(payload, &["phase"]) {
        lines.push(format!("Phase: {phase}"));
    }
    if let Some(waiting) = payload.get("local_admission") {
        let position = waiting["queue_position"].as_u64().unwrap_or(1);
        let eta = waiting["eta_seconds"]
            .as_u64()
            .map(|seconds| format!(", ETA <= {seconds}s (estimate from declared budgets)"))
            .unwrap_or_else(|| ", ETA unknown".to_string());
        lines.push(format!(
            "Local gate queue position {position} ({} lane, {} ahead){eta}",
            waiting["priority"].as_str().unwrap_or("normal"),
            position.saturating_sub(1),
        ));
        render_local_holders(&mut lines, waiting);
    }
    if let Some(mission) = string_value(payload, &["mission"]) {
        lines.push(format!("Mission: {mission}"));
    }
    if let Some(reference) = string_value(payload, &["candidate_ref"]) {
        lines.push(format!("Candidate ref: {reference}"));
    }
    for key in ["gated_sha", "publication_sha", "tree"] {
        if let Some(value) = string_value(payload, &[key]) {
            lines.push(format!("{key}: {value}"));
        }
    }
    if let Some(note) = string_value(payload, &["identity_note"]) {
        lines.push(note.to_string());
    }
    if let Some(path) = string_value(payload, &["candidate_repo_path"]) {
        lines.push(format!("Candidate repo: {path}"));
    }
    if let Some(command) = string_value(payload, &["candidate_inspect_command"]) {
        lines.push(format!("Inspect candidate: {command}"));
    }
    if let Some(candidate) = string_value(payload, &["candidate", "state"]) {
        lines.push(format!("Candidate: {candidate}"));
    }
    if let Some(gates) = payload.get("gates").and_then(Value::as_array) {
        let rendered = gates
            .iter()
            .filter_map(|gate| string_value(gate, &["state"]))
            .collect::<Vec<_>>()
            .join(", ");
        if !rendered.is_empty() {
            lines.push(format!("Gates: {rendered}"));
        }
    }
    // One line per Cook attempt (#17893).
    for attempt in payload
        .get("attempts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Ok(timing) = serde_json::from_value::<
            homeboy::agents::agent_task_service::CookAttemptTiming,
        >(attempt.clone())
        {
            lines.push(timing.summary_line());
        }
    }
    if let Some(progress) = string_value(payload, &["progress"]) {
        lines.push(format!("Progress: {progress}"));
    }
    if let Some(publication) = string_value(payload, &["publication", "state"]) {
        lines.push(format!("Publication: {publication}"));
        if let Some(identity) = string_value(payload, &["publication", "id"]) {
            lines.push(format!("PR: {identity}"));
        }
    }
    if let Some(blocker) = string_value(payload, &["blocker", "message"]) {
        let args = payload
            .pointer("/blocker/details/args")
            .and_then(Value::as_array)
            .map(|args| {
                args.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            });
        let full = match args.filter(|args| !args.is_empty()) {
            Some(args) => format!("{blocker}: {args}"),
            None => blocker.to_string(),
        };
        let blocker = compact_status_text(&full, 240);
        if let Some(code) = string_value(payload, &["blocker", "code"])
            .filter(|code| code.starts_with("resource_guard."))
        {
            lines.push(format!("Blocker: {code} ({blocker})"));
        } else {
            lines.push(format!("Blocker: {blocker}"));
        }
        // A failed gate's bounded output excerpt (#16218): the command alone
        // does not say why it failed.
        if let Some(output) =
            string_value(payload, &["blocker", "output"]).filter(|output| !output.trim().is_empty())
        {
            lines.push("Output:".to_string());
            lines.extend(output.lines().map(|line| format!("  {line}")));
        }
    }
    if let Some(details) = payload.pointer("/blocker/details") {
        if let Some(position) = details["queue_position"].as_u64() {
            lines.push(format!("Queue position: {position}"));
        }
        if let Some(policy) = details["queue_policy"].as_str() {
            lines.push(format!("Queue position / ETA: unknown ({policy})"));
        }
        if let Some(time) = details["next_attempt_at"].as_str() {
            lines.push(format!("Next attempt: {time}"));
        }
        for (field, label) in [
            ("pressure", "Pressure"),
            ("pressure_thresholds", "Pressure thresholds"),
            ("pressure_holders", "Pressure holders"),
        ] {
            if let Some(value) = details.get(field).filter(|value| !value.is_null()) {
                lines.push(format!("{label}: {value}"));
            }
        }
    }
    lines.extend(recovery_step_lines(payload));
    if payload.get("blocker").is_none_or(Value::is_null) {
        if let Some(reason) = string_value(payload, &["reason"]) {
            lines.push(format!("Reason: {reason}"));
        }
    }
    if string_value(payload, &["blocker", "retry", "disposition"])
        == Some("automatic_reconciliation_scheduled")
    {
        let retry = string_value(payload, &["blocker", "retry", "next_attempt_at"])
            .map(|time| format!("automatic reconciliation scheduled for {time}"))
            .unwrap_or_else(|| "waiting for runner eligibility; no fixed retry time".to_string());
        // Waiting is only recommended when a live daemon will actually pick
        // the work up; a dead daemon is the blocker and gets its recovery
        // command below (#16181).
        if payload.pointer("/blocker/daemon/admits_work") == Some(&Value::Bool(false)) {
            lines.push(format!(
                "Retry: {retry}; the local controller daemon is not admitting work"
            ));
            if let Some(summary) = string_value(payload, &["blocker", "daemon", "summary"]) {
                lines.push(format!("Daemon: {summary}"));
            }
        } else {
            lines.push(format!("Retry: {retry}; waiting is recommended"));
        }
    }
    lines.push(format!(
        "Artifacts: {}",
        payload
            .get("artifacts_count")
            .and_then(Value::as_u64)
            .map(|count| count as usize)
            .or_else(|| array_len(payload, &["artifacts"]))
            .unwrap_or(0)
    ));
    lines.push(format!(
        "Next: {}",
        control_plane_next_action(payload, run_id)
    ));
    Some(finish(lines))
}

/// The latest admitted recovery step and its recorded outcome (#18657): an
/// admitted `recover --regate` that already failed names its cause instead of
/// leaving the source attempt's original gate verdict as the only evidence.
fn recovery_step_lines(payload: &Value) -> Vec<String> {
    let Some(step) = payload.get("recovery_step").filter(|step| step.is_object()) else {
        return Vec::new();
    };
    let kind = string_value(step, &["step"]).unwrap_or("recovery");
    let state = string_value(step, &["state"]).unwrap_or("unknown");
    let job = string_value(step, &["job_id"])
        .map(|job| format!(" (job {job})"))
        .unwrap_or_default();
    let mut lines = vec![format!("Recovery step: {kind} {state}{job}")];
    if let Some(message) = string_value(step, &["failure", "message"]) {
        lines.push(format!(
            "Recovery step failure: {}",
            compact_status_text(message, 240)
        ));
    }
    lines
}

fn recovery_step_in_flight(payload: &Value) -> bool {
    string_value(payload, &["recovery_step", "state"]) == Some("in_flight")
}

#[test]
fn status_summary_names_a_failed_recovery_step_and_its_cause() {
    let lines = recovery_step_lines(&serde_json::json!({
        "recovery_step": {
            "step": "regate", "state": "failed", "job_id": "job-1", "exit_code": 2,
            "failure": { "message": "target filesystem is below the configured free-space reserve" }
        }
    }));
    assert_eq!(lines[0], "Recovery step: regate failed (job job-1)");
    assert!(lines[1].contains("free-space reserve"), "{lines:?}");
}

#[test]
fn in_flight_recovery_step_points_next_at_watching_status() {
    let payload = serde_json::json!({
        "state": "candidate_recoverable",
        "recovery": { "step": "regate", "command": "homeboy agent-task recover run" },
        "recovery_step": { "step": "regate", "state": "in_flight", "job_id": "job-1" },
        "action_eligibility": { "actions": [{ "action": "recover", "availability": "available" }] }
    });
    assert_eq!(
        control_plane_next_action(&payload, "run"),
        "homeboy agent-task status run --watch"
    );
    assert_eq!(
        recovery_step_lines(&payload),
        vec!["Recovery step: regate in_flight (job job-1)".to_string()]
    );
}

/// The control-plane run projects a live driver verifying its candidate.
pub(crate) fn live_gate_verifying(payload: &Value) -> bool {
    string_value(payload, &["state"]) == Some("running")
        && string_value(payload, &["phase"])
            == Some(homeboy::agents::orchestration::LIVE_GATE_VERIFYING_PHASE)
}

#[cfg(test)]
mod status_run_fixtures;

#[cfg(test)]
#[test]
fn control_plane_next_action_follows_owner_recovery_and_eligibility() {
    let cases = [
        // A live publication watches the continuation owner's Cook.
        (
            serde_json::json!({
                "state": "running", "phase": "publishing", "mission": "cook-1",
                "owner": {"kind": "cook_continuation", "id": "123"},
                "candidate": {"state": "verification_pending"}
            }),
            "homeboy agent-task status cook-1 --watch",
        ),
        // Recovery guidance cannot override empty eligibility.
        (
            serde_json::json!({
                "state": "candidate_recoverable",
                "recovery": { "step": "regate", "command": "homeboy agent-task recover run" },
                "action_eligibility": { "actions": [] }
            }),
            "homeboy runs evidence run",
        ),
        // `recover` is not a control-plane action: with only indeterminate
        // actions the projected recovery step wins.
        (
            serde_json::json!({
                "state": "candidate_recoverable",
                "recovery": { "step": "regate", "command": "homeboy agent-task recover run" },
                "action_eligibility": { "actions": [
                    { "action": "promote", "availability": "indeterminate" },
                    { "action": "cancel", "availability": "unavailable" }
                ] }
            }),
            "homeboy agent-task recover run",
        ),
    ];
    for (payload, expected) in cases {
        assert_eq!(
            control_plane_next_action(&payload, "run"),
            expected,
            "{payload}"
        );
    }
}

pub(crate) fn control_plane_next_action(payload: &Value, run_id: &str) -> String {
    // Render the canonical projection's live-owner phase, never infer it from artifacts.
    if recovery_step_in_flight(payload) {
        return format!("homeboy agent-task status {run_id} --watch");
    }
    if live_gate_verifying(payload)
        || (string_value(payload, &["state"]) == Some("running")
            && string_value(payload, &["owner", "kind"]) == Some("cook_continuation"))
    {
        let cook_id = string_value(payload, &["mission"]).unwrap_or(run_id);
        return format!("homeboy agent-task status {cook_id} --watch");
    }
    if string_value(payload, &["publication", "state"]).is_some_and(|state| {
        matches!(
            state,
            "review_ready" | "draft_published" | "succeeded_partial"
        )
    }) && string_value(payload, &["state"]) == Some("succeeded")
    {
        return string_value(payload, &["publication", "id"])
            .map(|identity| format!("review the published PR: {identity}"))
            .unwrap_or_else(|| format!("homeboy runs evidence {run_id}"));
    }
    if string_value(payload, &["blocker", "code"])
        .is_some_and(|code| code.starts_with("resource_guard.") || code.starts_with("outcome."))
    {
        if let Some(remedy) = string_value(payload, &["blocker", "reason"]) {
            return remedy.to_string();
        }
    }
    if string_value(payload, &["blocker", "retry", "disposition"])
        == Some("automatic_reconciliation_scheduled")
    {
        // The scheduled reconciliation runs on the local controller daemon.
        // When that daemon is down the wait can never be scheduled, so name
        // its recovery instead of recommending waiting (#16181).
        if payload.pointer("/blocker/daemon/admits_work") == Some(&Value::Bool(false)) {
            return "homeboy daemon recover".to_string();
        }
        return format!("homeboy agent-task status {run_id} --watch");
    }
    // A Cook's next step is the one `agent-task recover` performs, projected
    // from its durable state (#15925). A Cook with no step names `diagnose`,
    // which explains what is required; it is never re-decided here.
    // An incomplete agent review form names its operator remedy only when
    // the planner has no step of its own; the planner's step otherwise wins.
    if string_value(payload, &["blocker", "code"]) == Some("review_form.incomplete")
        && payload
            .get("recovery")
            .and_then(|recovery| string_value(recovery, &["step"]))
            .is_none_or(|step| step == "none")
    {
        if let Some(remedy) = string_value(payload, &["blocker", "reason"]) {
            return remedy.to_string();
        }
    }
    if let Some(recovery) = payload
        .get("recovery")
        .filter(|recovery| !recovery.is_null())
    {
        let command = string_value(recovery, &["command"]).unwrap_or_default();
        // The projection empties the eligibility report exactly when the
        // Cook's state makes no action legal, and then also projects step
        // `none`. A report with entries (available or indeterminate) never
        // overrides the planner's step: `recover` is not a control-plane
        // action, so "no action is available" does not mean "recover refuses".
        let legal_actions = payload
            .pointer("/action_eligibility/actions")
            .and_then(Value::as_array)
            .is_none_or(|actions| !actions.is_empty());
        if string_value(recovery, &["step"]) != Some("none") && legal_actions {
            return command.to_string();
        }
        if !legal_actions && command.contains("agent-task recover ") {
            return format!("homeboy runs evidence {run_id}");
        }
        let watching = matches!(
            string_value(payload, &["state"]),
            Some("running" | "queued")
        );
        return if watching {
            format!("homeboy agent-task status {run_id} --watch")
        } else {
            command.to_string()
        };
    }
    if string_value(payload, &["owner", "kind"]) == Some("runner") {
        let runner_id = string_value(payload, &["owner", "id"]);
        if string_value(payload, &["state"]) == Some("running") {
            return format!("homeboy agent-task status {run_id} --watch");
        }
        if string_value(payload, &["state"]) == Some("queued")
            && string_value(payload, &["phase"]) == Some("retry")
        {
            if let Some(runner_id) = runner_id {
                return format!("homeboy --runner {runner_id} agent-task run {run_id}");
            }
        }
    }
    const PREFERRED: [&str; 4] = ["reconcile", "resume", "retry", "review"];
    let Some(actions) = payload
        .pointer("/action_eligibility/actions")
        .and_then(Value::as_array)
    else {
        return format!("homeboy runs evidence {run_id}");
    };
    for name in PREFERRED {
        if actions.iter().any(|action| {
            action.get("action").and_then(Value::as_str) == Some(name)
                && action.get("availability").and_then(Value::as_str) == Some("available")
                && !action
                    .get("required_inputs")
                    .and_then(Value::as_array)
                    .is_some_and(|inputs| !inputs.is_empty())
        }) {
            let command = if name == "resume" { "recover" } else { name };
            return format!("homeboy agent-task {command} {run_id}");
        }
    }
    format!("homeboy runs evidence {run_id}")
}

fn render_logs_summary(payload: &Value) -> Option<String> {
    if string_value(payload, &["schema"])? != "homeboy/control-plane-event-page/v1" {
        return None;
    }
    let run_id = string_value(payload, &["run"])?;
    let event_count = array_len(payload, &["events"]).unwrap_or(0);
    let mut lines = vec![
        "Agent task logs".to_string(),
        format!("Run: {run_id}"),
        format!("Events: {event_count}"),
    ];
    if let Some(diagnostic) = first_actionable_diagnostic(payload) {
        lines.push(format!("Diagnostic: {diagnostic}"));
    }
    Some(finish(lines))
}

fn render_review_summary(payload: &Value) -> Option<String> {
    let run_id = string_value(payload, &["run_id"]).or_else(|| string_value(payload, &["run"]))?;
    let state = string_value(payload, &["state"])
        .or_else(|| string_value(payload, &["resource", "state"]))
        .unwrap_or("unknown");
    let summary = value_at(payload, &["aggregate_review", "summary"]);
    let raw_apply_candidates = summary
        .and_then(|_| {
            usize_value(
                payload,
                &["aggregate_review", "summary", "apply_candidates"],
            )
        })
        .unwrap_or(0);
    let failed = summary
        .and_then(|_| usize_value(payload, &["aggregate_review", "summary", "failed"]))
        .unwrap_or(0);
    let metrics = code_production_metrics(payload);
    let promotable = matches!(
        metrics.candidate_state,
        CandidateState::ApplyReady | CandidateState::PatchAvailable
    );
    let patch = promotable
        .then(|| string_value(payload, &["promotion_candidates", "0", "artifact_id"]))
        .flatten();
    let patch_path = patch.and_then(|artifact_id| artifact_path(payload, artifact_id));
    let next = first_string(payload, &["next_actions"]);
    let command = promotable
        .then(|| command_line(payload, &["promotion_candidates", "0", "command"]))
        .flatten();

    let target_applied = payload
        .pointer("/execution_states/promotion/patch_promoted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let verified = payload
        .pointer("/execution_states/promotion/verified")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let outcome = if metrics.candidate_state == CandidateState::Finalized {
        "pull request finalized"
    } else if target_applied && verified {
        "patch promoted and verified"
    } else if target_applied {
        "patch promoted"
    } else if promotable || metrics.non_empty_patches > 0 {
        "patch produced, not promoted"
    } else if raw_apply_candidates > 0 {
        "no-op: patch artifacts produced but empty"
    } else if failed > 0 || state == "failed" || state == "partial_failure" {
        "failed or partial failure"
    } else {
        "no patch candidates"
    };

    let mut lines = vec![
        "Agent task review".to_string(),
        format!("Run: {run_id}"),
        format!("Status: {state}"),
        format!("Outcome: {outcome}"),
    ];
    lines.extend(code_production_lines(&metrics));
    if let Some(line) = target_application_line(payload) {
        lines.push(line);
    }
    if let Some(diagnostic) = first_actionable_diagnostic(payload) {
        lines.push(format!("Diagnostic: {diagnostic}"));
    }
    lines.extend(review_form_lines(payload));
    if let Some(patch_path) = patch_path {
        lines.push(format!("Patch: {patch_path}"));
    } else if let Some(patch) = patch {
        lines.push(format!("Patch: {patch}"));
    }
    if let Some(command) = command {
        lines.push(format!("Next: {command}"));
    } else if let Some(next) = next {
        lines.push(format!("Next: {next}"));
    }
    Some(finish(lines))
}

/// The agent's verdict from the latest attempt's review form. The incomplete
/// reason is the agent's own account of the run, so it is shown in full.
fn review_form_lines(payload: &Value) -> Vec<String> {
    let Some(form) = value_at(payload, &["review_form"])
        .or_else(|| value_at(payload, &["evidence", "review_form"]))
    else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    if let Some(completion) = string_value(form, &["completion"]) {
        lines.push(format!("Agent verdict: {completion}"));
    }
    if let Some(reason) =
        string_value(form, &["incomplete_reason"]).filter(|reason| !reason.trim().is_empty())
    {
        lines.push(format!("Incomplete reason: {reason}"));
    }
    lines
}

fn target_application_line(payload: &Value) -> Option<String> {
    let promotion = value_at(payload, &["execution_states", "promotion"])?;
    let target = string_value(promotion, &["target", "worktree"]).unwrap_or("not declared");
    let target_state = string_value(promotion, &["target", "state"]).unwrap_or("not_applied");
    let fingerprint = promotion
        .pointer("/target/candidate_fingerprint_matches")
        .and_then(Value::as_bool)
        .map(|matches| if matches { "matches" } else { "does not match" })
        .unwrap_or("unknown");
    let verification = promotion
        .get("verified")
        .and_then(Value::as_bool)
        .or_else(|| {
            (string_value(promotion, &["verification_phase"]) == Some("pre_apply")).then_some(false)
        })
        .map(|verified| if verified { "verified" } else { "not verified" })
        .unwrap_or("unknown");
    Some(format!(
        "Target application: {target_state} (worktree: {target}; candidate fingerprint: {fingerprint}; verification: {verification})"
    ))
}

fn first_actionable_diagnostic(payload: &Value) -> Option<&str> {
    string_value(payload, &["diagnostic_summary", "message"])
        .or_else(|| first_diagnostic_message(payload, &["aggregate", "outcomes"]))
        .or_else(|| first_diagnostic_message(payload, &["aggregate_review", "tasks"]))
}

fn first_diagnostic_message<'a>(payload: &'a Value, path: &[&str]) -> Option<&'a str> {
    value_at(payload, path)?
        .as_array()?
        .iter()
        .find_map(|item| {
            value_at(item, &["diagnostics"])?
                .as_array()?
                .iter()
                .find_map(|diagnostic| string_value(diagnostic, &["message"]))
        })
}

fn aggregate_outcome_count(payload: &Value) -> Option<usize> {
    array_len(payload, &["aggregate", "outcomes"])
        .or_else(|| usize_value(payload, &["durable_candidate", "outcome_count"]))
}

fn aggregate_artifact_count(payload: &Value) -> usize {
    value_at(payload, &["aggregate", "outcomes"])
        .and_then(Value::as_array)
        .map(|outcomes| {
            outcomes
                .iter()
                .map(|outcome| array_len(outcome, &["artifacts"]).unwrap_or(0))
                .sum()
        })
        .unwrap_or_else(|| array_len(payload, &["artifact_refs"]).unwrap_or(0))
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct CodeProductionMetrics {
    non_empty_patches: usize,
    empty_patches: usize,
    unknown_size_patches: usize,
    diff_bytes: u64,
    changed_files: usize,
    /// Number of non-empty patches whose changed-file count could not be
    /// determined (no metadata and the patch content was unavailable or
    /// unparseable). Used to render `unknown` instead of a misleading verified
    /// zero (#9742).
    changed_files_unknown_patches: usize,
    candidate_state: CandidateState,
    candidate_scan_degraded: bool,
}

fn code_production_lines(metrics: &CodeProductionMetrics) -> Vec<String> {
    let patch_candidates = if metrics.unknown_size_patches > 0 {
        format!(
            "Patch candidates: {} non-empty / {} empty / {} unknown",
            metrics.non_empty_patches, metrics.empty_patches, metrics.unknown_size_patches
        )
    } else {
        format!(
            "Patch candidates: {} non-empty / {} empty",
            metrics.non_empty_patches, metrics.empty_patches
        )
    };
    // Distinguish an authoritative zero from an unknown count: if any non-empty
    // patch's change set could not be parsed, mark the total unknown rather than
    // claiming zero changed files for a substantive patch.
    let changed_files = if metrics.changed_files_unknown_patches > 0 {
        if metrics.changed_files > 0 {
            format!("Changed files: {} (+unknown)", metrics.changed_files)
        } else {
            "Changed files: unknown".to_string()
        }
    } else {
        format!("Changed files: {}", metrics.changed_files)
    };
    vec![
        patch_candidates,
        format!("Candidate state: {}", metrics.candidate_state.as_str()),
        changed_files,
        format!("Diff bytes: {}", metrics.diff_bytes),
    ]
}

fn code_production_metrics(payload: &Value) -> CodeProductionMetrics {
    let mut metrics = CodeProductionMetrics::default();
    let canonical = classify_candidates(payload);
    if let Some(patch) = selected_candidate_patch(payload) {
        match patch.size_bytes {
            Some(size) if size > 0 => {
                metrics.non_empty_patches = 1;
                metrics.diff_bytes = size;
                match patch.changed_files {
                    Some(count) => metrics.changed_files = count,
                    None => metrics.changed_files_unknown_patches = 1,
                }
            }
            Some(_) => metrics.empty_patches = 1,
            None => metrics.unknown_size_patches = 1,
        }
        metrics.candidate_state = canonical.state();
        metrics.candidate_scan_degraded = canonical.is_degraded();
        return metrics;
    }
    metrics.non_empty_patches = canonical.available;
    metrics.empty_patches = canonical.empty;
    metrics.diff_bytes = canonical.diff_bytes;
    metrics.changed_files = canonical.changed_files;
    metrics.changed_files_unknown_patches = canonical.changed_files_unknown_patches;
    metrics.unknown_size_patches = canonical.unknown
        + canonical.missing
        + canonical.unreadable
        + canonical.conflicting
        + canonical.retained_only;
    metrics.candidate_state = canonical.state();
    metrics.candidate_scan_degraded = canonical.is_degraded();
    metrics
}

struct PatchArtifact {
    size_bytes: Option<u64>,
    /// `Some(n)` when the changed-file count is known (from metadata or by
    /// parsing the patch content); `None` when it could not be determined.
    changed_files: Option<usize>,
}

fn selected_candidate_patch(payload: &Value) -> Option<PatchArtifact> {
    let candidate = payload.get("selected_candidate")?;
    let artifact = candidate.get("artifact")?;
    let changed_files = candidate
        .get("changed_files")
        .and_then(Value::as_array)
        .map(Vec::len)
        .or_else(|| {
            artifact
                .pointer("/metadata/changed_files")
                .and_then(Value::as_array)
                .map(Vec::len)
        })
        .or_else(|| {
            artifact
                .pointer("/metadata/changed_file_count")
                .and_then(Value::as_u64)
                .and_then(|count| usize::try_from(count).ok())
        });
    Some(PatchArtifact {
        size_bytes: candidate
            .get("size_bytes")
            .and_then(Value::as_u64)
            .or_else(|| u64_value(artifact, &["size_bytes"])),
        changed_files,
    })
}

/// A run whose lifecycle state is `succeeded` but that produced zero promotion
/// candidates did not actually patch anything. Surface that honestly as
/// `no_patch_produced` instead of advertising success (#4610).
fn effective_run_state(
    raw_state: &str,
    tasks_attempted: usize,
    candidate_state: CandidateState,
    candidate_scan_degraded: bool,
) -> &str {
    if raw_state == "succeeded"
        && candidate_state == CandidateState::Unknown
        && candidate_scan_degraded
    {
        "unknown"
    } else if raw_state == "succeeded" && tasks_attempted > 0 && !candidate_state.is_available() {
        "no_patch_produced"
    } else {
        raw_state
    }
}

fn first_string<'a>(payload: &'a Value, path: &[&str]) -> Option<&'a str> {
    value_at(payload, path)?.as_array()?.first()?.as_str()
}

fn command_line(payload: &Value, path: &[&str]) -> Option<String> {
    let command = value_at(payload, path)?.as_array()?;
    let parts: Vec<_> = command.iter().filter_map(Value::as_str).collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn artifact_path<'a>(payload: &'a Value, artifact_id: &str) -> Option<&'a str> {
    value_at(payload, &["aggregate_review", "artifact_inventory"])?
        .as_array()?
        .iter()
        .find(|artifact| string_value(artifact, &["artifact_id"]) == Some(artifact_id))
        .and_then(|artifact| string_value(artifact, &["path"]))
}

fn finish(lines: Vec<String>) -> String {
    let mut output = lines.join("\n");
    output.push('\n');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An admission-pending Cook renders the same state and phase in
    /// the human summary and the JSON envelope; neither reports `succeeded`.
    #[test]
    fn admission_pending_cook_renders_the_same_state_in_human_and_json() {
        use crate::commands::utils::response::{
            cli_response_for_json_result_for_identity, CommandIdentity,
        };
        let payload = json!({
            "schema": "homeboy/control-plane-run/v1",
            "run": "agent-task-1",
            "state": "running",
            "phase": "admission",
            "gates": [],
            "blocker": {
                "code": "pressure",
                "state": "waiting",
                "message": "runner homeboy-lab: pressure; waiting since 2026-11-20T00:00:00Z",
            },
        });

        let human = render_status_summary(&payload).expect("human summary");
        assert!(human.starts_with("Agent task running\n"), "{human}");
        assert!(human.contains("Phase: admission"), "{human}");
        assert!(
            human.contains("Blocker: runner homeboy-lab: pressure"),
            "{human}"
        );

        let envelope = serde_json::to_value(cli_response_for_json_result_for_identity(
            &Ok(payload),
            0,
            &CommandIdentity::with_operation("agent-task", "status"),
            None,
        ))
        .expect("serialize envelope");
        assert_eq!(envelope["status"], "running");
        assert_eq!(envelope["subject_state"], "running");
        assert_eq!(envelope["data"]["phase"], "admission");
        assert_ne!(envelope["status"], "succeeded");
    }

    fn queued_status(daemon: Option<Value>) -> Value {
        let mut blocker = json!({
            "reason": "queued: admitted to homeboy-lab; waiting for execution scheduling",
            "retry": {
                "disposition": "automatic_reconciliation_scheduled",
                "next_attempt_at": "2026-10-19T22:40:00Z"
            }
        });
        if let Some(daemon) = daemon {
            blocker["daemon"] = daemon;
        }
        json!({
            "schema": "homeboy/control-plane-run/v1",
            "run": "agent-task-1",
            "state": "running",
            "blocker": blocker,
        })
    }

    #[test]
    fn queued_status_waits_unless_the_daemon_cannot_admit_work() {
        let dead = queued_status(Some(json!({
            "admits_work": false,
            "summary": "no daemon lease is recorded and the daemon API is unreachable"
        })));
        let summary =
            render_agent_task_summary(AgentTaskSummaryKind::Status, &dead).expect("status summary");
        assert!(!summary.contains("waiting is recommended"), "{summary}");
        assert!(
            summary.contains("Daemon: no daemon lease is recorded"),
            "{summary}"
        );
        assert!(
            summary.contains("Next: homeboy daemon recover"),
            "{summary}"
        );

        for daemon in [
            None,
            Some(json!({ "admits_work": true, "summary": "live" })),
        ] {
            let payload = queued_status(daemon);
            let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload)
                .expect("status summary");
            assert!(summary.contains("waiting is recommended"), "{summary}");
            assert!(
                summary.contains("Next: homeboy agent-task status agent-task-1 --watch"),
                "{summary}"
            );
        }
    }

    #[test]
    fn projected_adopt_recovery_is_the_next_action() {
        let payload = json!({
            "schema": "homeboy/control-plane-run/v1",
            "run": "agent-task-1-attempt-3",
            "mission": "agent-task-1",
            "state": "partial_failure",
            "candidate": { "state": "no_changes" },
            "recovery": {
                "cook_id": "agent-task-1",
                "run": "agent-task-1-attempt-3",
                "step": "adopt",
                "candidate_ref": "abc123",
                "command": "homeboy agent-task recover agent-task-1 --candidate-ref abc123"
            }
        });
        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload)
            .expect("status summary");
        assert!(
            summary
                .contains("Next: homeboy agent-task recover agent-task-1 --candidate-ref abc123"),
            "{summary}"
        );
    }

    #[test]
    fn cook_preview_summary_renders_the_terminal_preview_payload() {
        let payload = json!({
            "schema": "homeboy/agent-task-cook-preview/v1",
            "resolved": {
                "placement": { "requested": "local" },
                "provider": { "backend": "fixture", "model": "test-model" },
                "workspace": { "path": "/tmp/worktree" },
                "gates": { "public": 1, "private": 2 },
            },
            "replay_argv": ["homeboy", "agent-task", "cook", "--backend", "fixture"],
        });

        assert_eq!(
            render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload),
            Some("Cook preview\nPlacement: local\nProvider: fixture\nModel: test-model\nDestination: /tmp/worktree\nGates: 1 public, 2 private\nReplay: homeboy agent-task cook --backend fixture\n".to_string())
        );
    }

    #[test]
    fn cook_preview_summary_names_the_effective_gate_limits() {
        let payload = json!({
            "schema": "homeboy/agent-task-cook-preview/v1",
            "resolved": {
                "placement": { "requested": "lab" },
                "provider": { "backend": "fixture", "model": "test-model" },
                "workspace": { "path": "/tmp/worktree" },
                "gates": {
                    "public": 1,
                    "private": 0,
                    "timeout_seconds": 900,
                    "no_progress_timeout_seconds": 300,
                },
            },
            "replay_argv": ["homeboy", "agent-task", "cook"],
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(
            summary.contains("Gates: 1 public, 0 private\nGate limits: 900s per gate, no-progress limit 300s (on Lab, dependency setup counts toward the gate limit)\nReplay:"),
            "{summary}"
        );
    }

    #[test]
    fn cook_preview_summary_exposes_deferred_exact_gate_without_private_details() {
        let payload = json!({
            "schema": "homeboy/agent-task-cook-preview/v1",
            "resolved": {
                "placement": { "requested": "local" },
                "provider": { "backend": "fixture", "model": "test-model" },
                "workspace": { "path": "/tmp/worktree" },
                "gates": { "public": 1, "private": 1 },
                "gate_contract_validation": {
                    "schema": "homeboy/cook-preview-gate-validation/v1",
                    "deferred_public": [{
                        "kind": "cargo",
                        "status": "selection_deferred",
                        "filter_interpretation": "exact",
                        "filter": "commands::agent_task::gate_contract::tests::preview_validates_cargo_shape_without_compiling_the_workspace",
                        "validation": "test_inventory_unavailable_without_build"
                    }],
                    "deferred_private_count": 1
                }
            },
            "replay_argv": ["homeboy", "agent-task", "cook", "--backend", "fixture"],
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload)
            .expect("cook preview summary");
        assert!(summary.contains(
            "Gate selection deferred: cargo filter `commands::agent_task::gate_contract::tests::preview_validates_cargo_shape_without_compiling_the_workspace` (exact) — test inventory unavailable without building test binaries"
        ));
        assert!(summary.contains("Private gate selections deferred: 1 (details withheld)"));
        assert!(!summary.contains("private-command-secret"));
    }

    /// Preview must report the *resolved* placement, not the
    /// operator's request, and must name why they diverge when a Lab-desiring
    /// request degrades to local execution.
    #[test]
    fn cook_preview_summary_reports_resolved_placement_and_fallback_reason() {
        let payload = json!({
            "schema": "homeboy/agent-task-cook-preview/v1",
            "resolved": {
                "placement": {
                    "requested": "auto",
                    "selected": "local",
                    "fallback_reason": "no Lab runner is configured",
                },
                "provider": { "backend": "fixture", "model": "test-model" },
                "workspace": { "path": "/tmp/worktree" },
                "gates": { "public": 1, "private": 2 },
            },
            "replay_argv": ["homeboy", "agent-task", "cook", "--backend", "fixture"],
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload)
            .expect("cook preview summary");

        assert!(
            summary.starts_with(
                "Cook preview\nPlacement: local  (requested: auto — no Lab runner is configured)\n"
            ),
            "summary did not report the resolved placement and fallback reason: {summary}"
        );
    }

    #[test]
    fn cook_preview_summary_names_unchecked_admission_instead_of_an_unqualified_plan() {
        let payload = json!({
            "schema": "homeboy/agent-task-cook-preview/v1",
            "resolved": {
                "placement": {
                    "requested": "auto",
                    "selected": "lab",
                    "selected_runner": "homeboy-lab",
                    "admission": {
                        "schema": "homeboy/cook-preview-placement-admission/v1",
                        "state": "indeterminate",
                        "reason": "static inputs only; admission not checked",
                    },
                },
                "provider": { "backend": "fixture", "model": "test-model" },
                "workspace": { "path": "/tmp/worktree" },
                "gates": { "public": 1, "private": 0 },
            },
            "replay_argv": ["homeboy", "agent-task", "cook", "--backend", "fixture"],
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload)
            .expect("cook preview summary");
        assert!(
            summary.contains("static inputs only; admission not checked"),
            "unchecked admission must be explicit in stdout: {summary}"
        );
        assert!(!summary.contains("Blocked:"), "{summary}");
    }

    #[test]
    fn cook_preview_summary_reports_admission_blocker_and_recovery_action() {
        let payload = json!({
            "schema": "homeboy/agent-task-cook-preview/v1",
            "resolved": {
                "placement": {
                    "requested": "auto",
                    "selected": "lab",
                    "selected_runner": "homeboy-lab",
                    "admission": {
                        "schema": "homeboy/cook-preview-placement-admission/v1",
                        "state": "blocked",
                        "remaining_blocker": "daemon_compatibility",
                        "next_action": "homeboy runner status homeboy-lab --full",
                    },
                },
                "provider": { "backend": "fixture", "model": "test-model" },
                "workspace": { "path": "/tmp/worktree" },
                "gates": { "public": 1, "private": 0 },
            },
            "failure": {
                "message": "daemon_compatibility",
                "next_action": "homeboy runner status homeboy-lab --full",
            },
            "replay_argv": ["homeboy", "agent-task", "cook", "--backend", "fixture"],
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload)
            .expect("cook preview summary");
        assert!(
            summary.contains("Blocked: daemon_compatibility"),
            "blocked admission must lead the operator-facing plan: {summary}"
        );
        assert!(
            summary.contains("Next: homeboy runner status homeboy-lab --full"),
            "recovery action must match runner status: {summary}"
        );
    }

    #[test]
    fn providers_summary_presents_selection_without_calling_it_blocked() {
        let payload = json!({
            "providers": [{ "backend": "alpha" }, { "backend": "zeta" }],
            "operator_summary": {
                "state": "selection_required",
                "selection_choices": [
                    { "backend": "alpha", "command": "homeboy agent-task providers --backend alpha --validate-readiness" },
                    { "backend": "zeta", "command": "homeboy agent-task providers --backend zeta --validate-readiness" }
                ],
                "next_action": "homeboy agent-task providers --backend alpha --validate-readiness"
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Providers, &payload)
            .expect("providers summary");

        assert_eq!(
            summary,
            "Agent task providers\nStatus: selection_required\nProviders shown: 2\nChoose backend: alpha, zeta\nNext: homeboy agent-task providers --backend alpha --validate-readiness"
        );
    }

    #[test]
    fn providers_summary_lists_route_and_pooled_account_capacity() {
        let payload = json!({
            "providers": [
                {
                    "id": "pi.agent-task-executor",
                    "dispatchability": { "capacity": {
                        "state": "known", "remaining": 91, "limit": 100, "unit": "percent",
                        "reset_at": "2026-09-24T15:50:00+00:00",
                        "accounts": [
                            { "account": "plan-a", "state": "exhausted", "remaining": 0, "reset_at": "2026-09-25T03:00:00+00:00" },
                            { "account": "plan-b", "state": "available", "remaining": 91, "reset_at": "2026-09-24T15:50:00+00:00" },
                            { "account": "old-login", "state": "credential_expired" }
                        ]
                    } }
                },
                { "id": "silent", "dispatchability": { "capacity": { "state": "unknown", "reason": "not reported" } } }
            ],
            "operator_summary": { "state": "ready" }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Providers, &payload)
            .expect("providers summary");

        assert_eq!(
            summary,
            [
                "Agent task providers",
                "Status: ready",
                "Providers shown: 2",
                "Capacity pi.agent-task-executor: 91% remaining, resets 2026-09-24T15:50:00+00:00",
                "  plan-a: exhausted, resets 2026-09-25T03:00:00+00:00",
                "  plan-b: available, 91% remaining, resets 2026-09-24T15:50:00+00:00",
                "  old-login: credential_expired",
            ]
            .join("\n")
        );
    }

    #[test]
    fn cook_summary_leads_with_run_status_and_review_next_step() {
        let payload = json!({
            "canonical_candidate": {"schema": "homeboy/agent-task-candidate/v1", "state": "patch_available", "counts": {"patch_available": 1}, "diff_bytes": 128, "changed_files": 0, "changed_files_unknown_patches": 1},
            "run_id": "homeboy-4345",
            "state": "succeeded",
            "task_count": 1,
            "aggregate_path": "/tmp/aggregate.json",
            "aggregate": {
                "outcomes": [{
                    "task_id": "homeboy-4345",
                    "artifacts": [{ "id": "patch", "kind": "patch", "path": "/tmp/patch.diff", "size_bytes": 128 }]
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.starts_with("Agent task cook\nRun: homeboy-4345\nStatus: succeeded"));
        assert!(summary.contains("Tasks planned: 1\n"));
        assert!(summary.contains("Tasks attempted: 1\n"));
        assert!(summary.contains("Patch candidates: 1 non-empty / 0 empty\n"));
        assert!(summary.contains("Diff bytes: 128\n"));
        assert!(summary.contains("First artifact: /tmp/patch.diff\n"));
        assert!(summary.contains("Next: homeboy agent-task review homeboy-4345\n"));
        assert!(!summary.contains("{\n"));
    }

    /// One bounded line per provider route the run skipped as unready
    /// before dispatch, so a credentials-dead primary is operator-visible.
    #[test]
    fn cook_summary_names_each_skipped_provider_route() {
        let payload = json!({
            "run_id": "skipped-route-run",
            "state": "succeeded",
            "task_count": 1,
            "provider_route_notes": [
                "Ran on openai/gpt-6-luna; anthropic/claude-sonnet-5-5 skipped: credentials_unusable (Refresh the Roadie credential pool key, then retry.)",
                "xai/grok-4.7 skipped: usage_capped (Wait for the provider usage cap to reset.)"
            ]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload);

        let summary = summary.expect("cook summary renders");
        assert!(
            summary.contains("Ran on openai/gpt-6-luna; anthropic/claude-sonnet-5-5 skipped: credentials_unusable (Refresh the Roadie credential pool key, then retry.)\n"),
            "{summary}"
        );
        assert!(
            summary.contains("xai/grok-4.7 skipped: usage_capped"),
            "{summary}"
        );

        // A report without route notes renders unchanged (no empty block).
        let plain = render_agent_task_summary(
            AgentTaskSummaryKind::Cook,
            &json!({
                "run_id": "plain-run",
                "state": "succeeded",
                "task_count": 1,
            }),
        )
        .expect("plain summary");
        assert!(
            !plain.contains("skipped:"),
            "no route notes must not invent a skip line: {plain}"
        );
    }

    #[test]
    fn cook_summary_marks_authoritative_candidate_counts_unavailable() {
        let payload = json!({
            "run_id": "candidate-read-unavailable",
            "status": "candidate_recoverable",
            "durable_candidate": {
                "status": "unavailable",
                "reason": "the authoritative observation has no aggregate"
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(
            summary.contains("Tasks planned: unavailable\n"),
            "{summary}"
        );
        assert!(
            summary.contains("Tasks attempted: unavailable\n"),
            "{summary}"
        );
        assert!(summary.contains("Artifacts: unavailable\n"), "{summary}");
        assert!(summary.contains(
            "Candidate evidence: unavailable (the authoritative observation has no aggregate)\n"
        ));
        assert!(!summary.contains("Candidate state: unknown"), "{summary}");
    }

    #[test]
    fn cook_summary_prefers_durable_provider_executions_over_outcomes() {
        let payload = json!({
            "run_id": "candidate-after-retries",
            "status": "candidate_recoverable",
            "durable_candidate": {
                "status": "available",
                "outcome_count": 1,
                "provider_execution_count": 3,
                "canonical_candidate": {
                    "schema": "homeboy/agent-task-candidate/v1",
                    "state": "patch_available",
                    "provider_executions": 3,
                    "counts": { "patch_available": 1 },
                    "scan": {}
                }
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Tasks attempted: 3\n"), "{summary}");
    }

    #[test]
    fn cook_summary_leads_with_provider_primary_failure_and_one_exact_action() {
        let payload = json!({
            "run_id": "agent-task-provider-failure",
            "state": "failed",
            "task_count": 1,
            "primary_failure": {
                "schema": "homeboy/agent-task-cook-primary-failure/v1",
                "provider_id": "dmc",
                "operation": "resolve",
                "phase": "transport_dispatcher_prepare",
                "exit_code": 1,
                "stderr_excerpt": "DMC standalone identity does not provide tracker ownership.",
                "evidence_ref": "homeboy://agent-task/run/agent-task-provider-failure/status",
                "next_action": {
                    "action": "diagnose",
                    "command": "homeboy agent-task diagnose agent-task-provider-failure --full"
                }
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.starts_with(
            "Agent task cook\nPrimary failure: provider dmc resolve failed during transport_dispatcher_prepare (exit 1)\nProvider stderr: DMC standalone identity does not provide tracker ownership.\nNext: homeboy agent-task diagnose agent-task-provider-failure --full\nRun: agent-task-provider-failure"
        ));
        assert_eq!(summary.matches("Next:").count(), 1);

        let mut empty_stderr = payload;
        empty_stderr["primary_failure"]["stderr_excerpt"] = json!("");
        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &empty_stderr).unwrap();
        assert!(summary.contains("Provider stderr: (empty)\n"), "{summary}");
    }

    #[test]
    fn cook_summary_reports_no_patch_produced_when_all_cells_are_empty() {
        // Three succeeded cells, but every patch artifact is 0 bytes. The
        // summary must not advertise success.
        let payload = json!({
            "canonical_candidate": {"schema": "homeboy/agent-task-candidate/v1", "state": "empty", "counts": {"empty": 3}, "diff_bytes": 0, "changed_files": 0, "changed_files_unknown_patches": 0},
            "run_id": "agent-task-abe47e4d",
            "state": "succeeded",
            "task_count": 3,
            "aggregate_path": "/tmp/aggregate.json",
            "aggregate_review": {
                "summary": { "apply_candidates": 0 }
            },
            "aggregate": {
                "outcomes": [
                    { "task_id": "cell-1", "artifacts": [{ "id": "patch", "kind": "patch", "path": "/tmp/patch.diff", "size_bytes": 0 }] },
                    { "task_id": "cell-2", "artifacts": [{ "id": "patch", "kind": "patch", "path": "/tmp/patch.diff", "size_bytes": 0 }] },
                    { "task_id": "cell-3", "artifacts": [{ "id": "patch", "kind": "patch", "path": "/tmp/patch.diff", "size_bytes": 0 }] }
                ]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Status: no_patch_produced\n"));
        assert!(summary.contains("Patch candidates: 0 non-empty / 3 empty\n"));
        assert!(summary.contains("Next: homeboy runs logs agent-task-abe47e4d\n"));
        assert!(!summary.contains("Next: homeboy agent-task review"));
    }

    #[test]
    fn cook_summary_renders_replayable_promotion_and_unavailable_remediation() {
        let replayable = json!({
            "run_id": "cook-timeout-attempt-1",
            "state": "candidate_recoverable",
            "promotion_replay": {
                "available": true,
                "command": "homeboy agent-task promote cook-timeout-attempt-1 --dry-run"
            }
        });
        let replayable = render_agent_task_summary(AgentTaskSummaryKind::Cook, &replayable)
            .expect("replayable Cook summary");
        assert!(replayable
            .contains("Next: homeboy agent-task promote cook-timeout-attempt-1 --dry-run\n"));

        let unavailable = json!({
            "run_id": "cook-timeout-attempt-1",
            "state": "candidate_recoverable",
            "promotion_replay": {
                "available": false,
                "reason": "aggregate key agent_task_aggregate is missing",
                "remediation": "restore aggregate.json or executor evidence"
            }
        });
        let unavailable = render_agent_task_summary(AgentTaskSummaryKind::Cook, &unavailable)
            .expect("unavailable Cook summary");
        assert!(unavailable
            .contains("Promotion unavailable: aggregate key agent_task_aggregate is missing\n"));
        assert!(unavailable.contains("Remediation: restore aggregate.json or executor evidence\n"));
        assert!(!unavailable.contains("Next: homeboy runs logs"));
    }

    #[test]
    fn cook_summary_keeps_a_canonical_candidate_after_an_empty_retry() {
        let payload = json!({
            "canonical_candidate": {"schema": "homeboy/agent-task-candidate/v1", "state": "patch_available", "counts": {"patch_available": 1}, "diff_bytes": 32318, "changed_files": 0, "changed_files_unknown_patches": 1},
            "run_id": "agent-task-canonical",
            "state": "succeeded",
            "task_count": 2,
            "attempts": [
                {
                    "aggregate": {
                        "outcomes": [{
                            "artifacts": [{
                                "id": "canonical-patch",
                                "kind": "patch",
                                "size_bytes": 32318,
                                "url": "homeboy://agent-task/run/agent-task-canonical/artifacts#task=cook&artifact=canonical-patch",
                                "metadata": { "executor_artifact_finalized": true }
                            }]
                        }]
                    }
                },
                {
                    "aggregate": {
                        "outcomes": [{
                            "artifacts": [{ "id": "empty-retry", "kind": "patch", "size_bytes": 0 }]
                        }]
                    }
                }
            ]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Status: succeeded\n"));
        assert!(summary.contains("Patch candidates: 1 non-empty / 0 empty\n"));
        assert!(summary.contains("Candidate state: patch_available\n"));
        assert!(summary.contains("Next: homeboy agent-task review agent-task-canonical\n"));
        assert!(!summary.contains("no_patch_produced"));
    }

    #[test]
    fn finalized_pr_summary_is_not_reported_as_an_unpromoted_empty_patch() {
        let payload = json!({
            "canonical_candidate": {"schema": "homeboy/agent-task-candidate/v1", "state": "finalized", "counts": {}, "diff_bytes": 0, "changed_files": 0, "changed_files_unknown_patches": 0},
            "run_id": "agent-task-finalized",
            "state": "succeeded",
            "task_count": 1,
            "aggregate": { "outcomes": [{ "artifacts": [{ "id": "empty", "kind": "patch", "size_bytes": 0 }] }] },
            "finalization": { "status": "review_ready", "pr_url": "https://example.test/pull/1" }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Status: succeeded\n"));
        assert!(summary.contains("Candidate state: finalized\n"));
        assert!(summary.contains("Next: homeboy agent-task review agent-task-finalized\n"));
        assert!(!summary.contains("no_patch_produced"));
    }

    #[test]
    fn degraded_candidate_scan_keeps_the_terminal_summary_unknown() {
        // An omitted-artifact scan must not be read as an all-empty run.
        let payload = json!({
            "canonical_candidate": {"schema": "homeboy/agent-task-candidate/v1", "state": "unknown", "scan": {"artifacts_omitted": 1}},
            "run_id": "agent-task-truncated",
            "state": "succeeded",
            "task_count": 1,
            "aggregate": { "outcomes": [{ "artifacts": [{ "id": "empty", "kind": "patch", "size_bytes": 0 }] }] }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Status: unknown\n"));
        assert!(summary.contains("Candidate state: unknown\n"));
        assert!(!summary.contains("Status: no_patch_produced\n"));
    }

    #[test]
    fn cook_summary_treats_unknown_size_patch_as_zero_candidates() {
        let payload = json!({
            "canonical_candidate": {"schema": "homeboy/agent-task-candidate/v1", "state": "unknown", "counts": {"unknown": 1}, "diff_bytes": 0, "changed_files": 0, "changed_files_unknown_patches": 0},
            "run_id": "homeboy-4345",
            "state": "succeeded",
            "task_count": 1,
            "aggregate": {
                "outcomes": [{
                    "task_id": "homeboy-4345",
                    "artifacts": [{ "id": "patch", "kind": "patch", "path": "/tmp/patch.diff" }]
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Status: no_patch_produced\n"));
        assert!(summary.contains("Patch candidates: 0 non-empty / 0 empty / 1 unknown\n"));
        assert!(summary.contains("Next: homeboy runs logs homeboy-4345\n"));
    }

    #[test]
    fn cook_summary_reports_known_or_unknown_changed_files() {
        // (changed_files, changed_files_unknown_patches, expected line)
        for (changed, unknown, expected) in [
            (2, 0, "Changed files: 2\n"),
            (0, 1, "Changed files: unknown\n"),
        ] {
            let payload = json!({
                "canonical_candidate": {"schema": "homeboy/agent-task-candidate/v1", "state": "patch_available", "counts": {"patch_available": 1}, "diff_bytes": 256, "changed_files": changed, "changed_files_unknown_patches": unknown},
                "run_id": "homeboy-4345",
                "state": "succeeded",
                "task_count": 1
            });

            let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

            assert!(
                summary.contains("Patch candidates: 1 non-empty / 0 empty\n"),
                "{summary}"
            );
            assert!(summary.contains(expected), "{summary}");
            assert!(summary.contains("Diff bytes: 256\n"), "{summary}");
        }
    }

    #[test]
    fn review_summary_uses_a_target_applied_candidate_fingerprint() {
        // Target application is independent from retained candidate selection.
        let payload = json!({
            "run_id": "agent-task-11805",
            "state": "succeeded",
            "canonical_candidate": {
                "schema": "homeboy/agent-task-candidate/v1",
                "state": "apply_ready",
                "diff_bytes": 0,
                "counts": { "patch_available": 1 },
                "scan": { "degraded": false }
            },
            "selected_candidate": {
                "status": "applied",
                "artifact": { "id": "candidate", "kind": "patch" },
                "size_bytes": 7635,
                "changed_files": ["a.rs", "b.rs", "c.rs"]
            },
            "execution_states": {
                "promotion": {
                    "patch_promoted": true,
                    "verified": true,
                    "target": { "state": "applied", "worktree": "fixture@target", "candidate_fingerprint_matches": true }
                }
            },
            "aggregate_review": { "summary": { "apply_candidates": 0, "failed": 0 } },
            "next_actions": ["finalize the pull request"]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.contains("Outcome: patch promoted and verified\n"));
        assert!(summary.contains("Target application: applied (worktree: fixture@target; candidate fingerprint: matches; verification: verified)\n"));
        assert!(summary.contains("Changed files: 3\n"));
        assert!(summary.contains("Diff bytes: 7635\n"));
    }

    #[test]
    fn review_summary_keeps_a_pre_apply_candidate_out_of_the_promoted_outcome() {
        let payload = json!({
            "run_id": "retained-candidate",
            "state": "candidate_recoverable",
            "canonical_candidate": {
                "schema": "homeboy/agent-task-candidate/v1",
                "state": "apply_ready",
                "counts": { "patch_available": 1 }, "scan": { "degraded": false }
            },
            "selected_candidate": {
                "status": "verification_pending",
                "artifact": { "id": "candidate", "kind": "patch" },
                "size_bytes": 7635,
                "changed_files": ["a.rs", "b.rs", "c.rs"]
            },
            "execution_states": {
                "promotion": {
                    "state": "verification_pending",
                    "patch_promoted": false,
                    "verified": false,
                    "verification_phase": "pre_apply",
                    "target": { "state": "not_applied", "worktree": "fixture@clean-target", "candidate_fingerprint_matches": false }
                }
            },
            "aggregate_review": { "summary": { "apply_candidates": 1, "failed": 0 } }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(
            summary.contains("Outcome: patch produced, not promoted\n"),
            "{summary}"
        );
        assert!(!summary.contains("Outcome: patch promoted\n"), "{summary}");
        assert!(summary.contains("Target application: not_applied (worktree: fixture@clean-target; candidate fingerprint: does not match; verification: not verified)\n"), "{summary}");
    }

    #[test]
    fn selection_required_timeout_recovery_keeps_review_inventory() {
        let candidates = [
            ("timeout-recovered-1", 32_318, 2),
            ("timeout-recovered-2", 32_318, 3),
            ("timeout-recovered-3", 32_412, 4),
        ]
        .into_iter()
        .map(|(id, size_bytes, changed_file_count)| {
            json!({
                "id": id,
                "kind": "patch",
                "size_bytes": size_bytes,
                "url": format!("homeboy://agent-task/run/selection-required/artifacts#{id}"),
                "metadata": {
                    "executor_artifact_finalized": true,
                    "changed_file_count": changed_file_count,
                    "recovered_from": "scheduler_timeout",
                },
            })
        })
        .collect::<Vec<_>>();
        let review = json!({
            "canonical_candidate": {"schema": "homeboy/agent-task-candidate/v1", "state": "patch_available", "counts": {"patch_available": 3}, "diff_bytes": 97048, "changed_files": 9, "changed_files_unknown_patches": 0},

            "run_id": "selection-required",
            "state": "candidate_recoverable",
            "record": { "metadata": { "provider_executions_consumed": 3 } },
            "aggregate": { "outcomes": [{ "task_id": "cook", "artifacts": candidates }] },
            "aggregate_review": { "summary": { "apply_candidates": 3, "failed": 0 } },
        });

        let review_summary =
            render_agent_task_summary(AgentTaskSummaryKind::Review, &review).expect("review");
        assert!(
            review_summary.contains("Patch candidates: 3 non-empty / 0 empty\n"),
            "{review_summary}"
        );
        assert!(
            review_summary.contains("Candidate state: patch_available\n"),
            "{review_summary}"
        );
        assert!(
            review_summary.contains("Changed files: 9\n"),
            "{review_summary}"
        );
        assert!(
            review_summary.contains("Diff bytes: 97048\n"),
            "{review_summary}"
        );
    }

    #[test]
    fn cook_summary_does_not_count_provider_failures_as_patch_candidates() {
        let payload = json!({
            "run_id": "agent-task-22bb7835",
            "state": "failed",
            "task_count": 4,
            "aggregate_path": "/tmp/aggregate.json",
            "aggregate": {
                "outcomes": [
                    { "task_id": "cell-1", "status": "provider_error", "summary": "no extension agent-task provider found for backend wordpress", "artifacts": [] },
                    { "task_id": "cell-2", "status": "provider_error", "summary": "no extension agent-task provider found for backend wordpress", "artifacts": [] },
                    { "task_id": "cell-3", "status": "provider_error", "summary": "no extension agent-task provider found for backend wordpress", "artifacts": [] },
                    { "task_id": "cell-4", "status": "provider_error", "summary": "no extension agent-task provider found for backend wordpress", "artifacts": [] }
                ]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Cook, &payload).unwrap();

        assert!(summary.contains("Tasks planned: 4\n"));
        assert!(summary.contains("Tasks attempted: 4\n"));
        assert!(summary.contains("Patch candidates: 0 non-empty / 0 empty\n"));
        assert!(summary.contains("Artifacts: 0\n"));
        assert!(summary.contains("Next: homeboy runs logs agent-task-22bb7835\n"));
        assert!(!summary.contains("Next: homeboy agent-task review"));
    }

    #[test]
    fn review_summary_surfaces_patch_candidate_before_next_command() {
        let payload = json!({
            "canonical_candidate": {"schema": "homeboy/agent-task-candidate/v1", "state": "patch_available", "counts": {"patch_available": 1}, "diff_bytes": 128, "changed_files": 0, "changed_files_unknown_patches": 1},
            "run_id": "homeboy-4345",
            "state": "succeeded",
            "aggregate_review": {
                "summary": {
                    "apply_candidates": 1,
                    "failed": 0
                },
                "apply_candidates": [{
                    "task_id": "homeboy-4345",
                    "decision": "apply_candidate",
                    "reason": "succeeded with reviewable patch/artifact output",
                    "artifact_ids": ["patch.diff"]
                }],
                "artifact_inventory": [{
                    "task_id": "homeboy-4345",
                    "artifact_id": "patch.diff",
                    "kind": "patch",
                    "path": "/tmp/patch.diff",
                    "size_bytes": 128
                }]
            },
            "promotion_candidates": [{
                "artifact_id": "patch.diff",
                "command": ["homeboy", "agent-task", "promote", "homeboy-4345", "--artifact-id", "patch.diff"]
            }]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.starts_with("Agent task review\nRun: homeboy-4345\nStatus: succeeded"));
        assert!(summary.contains("Outcome: patch produced, not promoted\n"));
        assert!(summary.contains("Patch candidates: 1 non-empty / 0 empty\n"));
        assert!(summary.contains("Diff bytes: 128\n"));
        assert!(summary.contains("Patch: /tmp/patch.diff\n"));
        assert!(summary
            .contains("Next: homeboy agent-task promote homeboy-4345 --artifact-id patch.diff\n"));
        assert!(!summary.contains("promotion_candidates"));
    }

    #[test]
    fn review_summary_keeps_the_default_compact_apply_candidate() {
        // Default review must preserve the same substantive candidate
        // selected for promotion even though full artifact inventories are omitted.
        let payload = json!({
            "run_id": "agent-task-11982",
            "state": "succeeded",
            "aggregate_review": { "summary": { "apply_candidates": 1, "failed": 0 } },
            "canonical_candidate": {
                "schema": "homeboy/agent-task-candidate/v1",
                "state": "patch_available",
                "diff_bytes": 17394,
                "counts": { "patch_available": 1 },
                "scan": { "degraded": false }
            },
            "selected_candidate": {
                "status": "available",
                "task_id": "task-a",
                "artifact": {
                    "artifact_id": "patch-a",
                    "kind": "patch",
                    "path": "/tmp/patch-a.diff",
                    "metadata": { "changed_files": ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"] }
                },
                "size_bytes": 17394,
                "changed_files": ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs", "f.rs"]
            },
            "promotion_candidates": [{
                "artifact_id": "patch-a",
                "command": null,
                "destination_required": true
            }],
            "next_actions": ["rerun review with `homeboy agent-task review agent-task-11982 --to-worktree <managed-worktree>` to generate executable promotion commands for apply candidates"]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.contains("Outcome: patch produced, not promoted\n"));
        assert!(summary.contains("Patch candidates: 1 non-empty / 0 empty\n"));
        assert!(summary.contains("Candidate state: patch_available\n"));
        assert!(summary.contains("Changed files: 6\n"));
        assert!(summary.contains("Diff bytes: 17394\n"));
        assert!(summary.contains("Next: rerun review with `homeboy agent-task review agent-task-11982 --to-worktree <managed-worktree>` to generate executable promotion commands for apply candidates\n"));
        assert!(!summary.contains("Next: homeboy agent-task promote"));
    }

    #[test]
    fn review_summary_does_not_treat_stale_promotion_candidates_as_patches() {
        let payload = json!({
            "run_id": "homeboy-4345",
            "state": "failed",
            "aggregate_review": {
                "summary": {
                    "apply_candidates": 0,
                    "failed": 1
                },
                "artifact_inventory": [{
                    "task_id": "homeboy-4345",
                    "artifact_id": "empty-patch",
                    "kind": "patch",
                    "path": "/tmp/patch.diff",
                    "size_bytes": 0
                }]
            },
            "promotion_candidates": [{
                "artifact_id": "empty-patch",
                "command": ["homeboy", "agent-task", "promote", "homeboy-4345", "--artifact-id", "empty-patch"]
            }]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.contains("Outcome: failed or partial failure\n"));
        assert!(summary.contains("Patch candidates: 0 non-empty / 0 empty\n"));
        assert!(!summary.contains("patch produced"));
        assert!(!summary.contains("Next: homeboy agent-task promote"));
    }

    #[test]
    fn review_summary_marks_no_op_when_apply_candidates_are_empty_patches() {
        let payload = json!({
            "canonical_candidate": {"schema": "homeboy/agent-task-candidate/v1", "state": "empty", "counts": {"empty": 3}, "diff_bytes": 0, "changed_files": 0, "changed_files_unknown_patches": 0},
            "run_id": "homeboy-4345",
            "state": "succeeded",
            "aggregate_review": {
                "summary": {
                    "apply_candidates": 3,
                    "failed": 0
                },
                "apply_candidates": [
                    { "task_id": "cell-1", "decision": "apply_candidate", "reason": "succeeded with reviewable patch/artifact output", "artifact_ids": ["sample-patch-1"] },
                    { "task_id": "cell-2", "decision": "apply_candidate", "reason": "succeeded with reviewable patch/artifact output", "artifact_ids": ["sample-patch-2"] },
                    { "task_id": "cell-3", "decision": "apply_candidate", "reason": "succeeded with reviewable patch/artifact output", "artifact_ids": ["sample-patch-3"] }
                ],
                "artifact_inventory": [
                    { "task_id": "cell-1", "artifact_id": "sample-patch-1", "kind": "patch", "path": "/tmp/patch-1.diff", "size_bytes": 0 },
                    { "task_id": "cell-2", "artifact_id": "sample-patch-2", "kind": "patch", "path": "/tmp/patch-2.diff", "size_bytes": 0 },
                    { "task_id": "cell-3", "artifact_id": "sample-patch-3", "kind": "patch", "path": "/tmp/patch-3.diff", "size_bytes": 0 }
                ]
            },
            "promotion_candidates": [{
                "artifact_id": "sample-patch-1",
                "command": ["homeboy", "agent-task", "promote", "homeboy-4345", "--artifact-id", "sample-patch-1"]
            }],
            "next_actions": ["inspect task summaries before retrying or reporting"]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.contains("Outcome: no-op: patch artifacts produced but empty\n"));
        assert!(summary.contains("Patch candidates: 0 non-empty / 3 empty\n"));
        assert!(summary.contains("Diff bytes: 0\n"));
        assert!(!summary.contains("Patch: "));
        assert!(!summary.contains("Next: homeboy agent-task promote"));
        assert!(summary.contains("Next: inspect task summaries before retrying or reporting"));
    }

    #[test]
    fn review_summary_treats_unknown_size_patch_as_not_promotable() {
        let payload = json!({
            "canonical_candidate": {"schema": "homeboy/agent-task-candidate/v1", "state": "unknown", "counts": {"unknown": 1}, "diff_bytes": 0, "changed_files": 0, "changed_files_unknown_patches": 0},
            "run_id": "homeboy-4345",
            "state": "succeeded",
            "aggregate_review": {
                "summary": { "apply_candidates": 1, "failed": 0 },
                "apply_candidates": [{
                    "task_id": "homeboy-4345",
                    "decision": "apply_candidate",
                    "reason": "succeeded with reviewable patch/artifact output",
                    "artifact_ids": ["unmeasured-patch"]
                }],
                "artifact_inventory": [{
                    "task_id": "homeboy-4345",
                    "artifact_id": "unmeasured-patch",
                    "kind": "patch",
                    "path": "/tmp/patch.diff"
                }]
            },
            "promotion_candidates": [{
                "artifact_id": "unmeasured-patch",
                "command": ["homeboy", "agent-task", "promote", "homeboy-4345", "--artifact-id", "unmeasured-patch"]
            }]
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();
        assert!(summary.contains("Patch candidates: 0 non-empty / 0 empty / 1 unknown\n"));
        assert!(!summary.contains("Next: homeboy agent-task promote"));
    }

    #[test]
    fn review_summary_surfaces_first_outcome_diagnostic() {
        let payload = json!({
            "run_id": "agent-task-d1622a44",
            "state": "failed",
            "aggregate_review": {
                "summary": { "apply_candidates": 0, "failed": 1 },
                "tasks": [{
                    "task_id": "agent-task-d1622a44",
                    "status": "provider_error",
                    "diagnostics": [{
                        "class": "provider_discovery",
                        "message": "Requested provider \"example-oauth\" is not registered. Registered provider plugins: []"
                    }]
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Review, &payload).unwrap();

        assert!(summary.contains(
            "Diagnostic: Requested provider \"example-oauth\" is not registered. Registered provider plugins: []\n"
        ));
    }

    #[test]
    fn status_summary_reads_control_plane_run() {
        for state in ["queued", "running", "failed", "succeeded"] {
            let payload = json!({
                "schema": "homeboy/control-plane-run/v1",
                "run": "homeboy-4345",
                "state": state,
                "artifacts": []
            });

            let summary =
                render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

            assert!(
                summary.starts_with(&format!("Agent task {state}\nRun: homeboy-4345")),
                "{summary}"
            );
            assert!(summary.contains("Artifacts: 0\n"), "{summary}");
            assert!(
                summary.contains("Next: homeboy runs evidence homeboy-4345\n"),
                "{summary}"
            );
            assert!(!summary.contains("Cook outcome:"), "{summary}");
        }
    }

    #[test]
    fn status_summary_uses_control_plane_action_eligibility() {
        let payload = json!({
            "schema": "homeboy/control-plane-run/v1",
            "run": "unmaterialized-cook",
            "state": "queued",
            "artifacts": [],
            "action_eligibility": {
                "schema": "homeboy/control-plane-action-eligibility/v1",
                "run": "unmaterialized-cook",
                "actions": [{
                    "action": "resume",
                    "availability": "available",
                    "reason": "queued run can re-enter execution",
                    "confirmation": "none",
                    "idempotent": true,
                    "requires_revalidation": true,
                    "result_resource_type": "agent_task_run"
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();
        assert!(summary.contains("Next: homeboy agent-task recover unmaterialized-cook\n"));
        assert!(!summary.contains("homeboy agent-task run unmaterialized-cook"));
    }

    #[test]
    fn queued_runner_proxy_status_recommends_runner_scoped_dispatch() {
        let payload = json!({
            "schema": "homeboy/control-plane-run/v1",
            "run": "exact-run-id",
            "state": "queued",
            "owner": { "id": "runner-7", "kind": "runner" },
            "phase": "retry",
            "action_eligibility": { "actions": [{
                "action": "resume", "availability": "unavailable",
                "reason": "runner-owned transport proxy must be continued by its owning runner"
            }] },
            "artifacts": []
        });
        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();
        assert!(summary.contains("Next: homeboy --runner runner-7 agent-task run exact-run-id\n"));

        let live = json!({
            "schema": "homeboy/control-plane-run/v1", "run": "exact-run-id",
            "state": "running", "owner": { "id": "runner-7", "kind": "runner" },
            "artifacts": []
        });
        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &live).unwrap();
        assert!(summary.contains("Next: homeboy agent-task status exact-run-id --watch\n"));
    }

    #[test]
    fn status_summary_waits_for_a_scheduled_unmaterialized_admission_retry() {
        let payload = json!({
            "schema": "homeboy/control-plane-run/v1",
            "run": "unmaterialized-cook",
            "state": "queued",
            "blocker": {
                "code": "queued",
                "state": "queued",
                "message": "Lab admission is waiting for runner reconciliation",
                "reason": "Lab admission is waiting for runner reconciliation",
                "retry": {
                    "policy": "bounded_exponential",
                    "attempts": 3,
                    "max_attempts": 20,
                    "next_attempt_at": "2099-01-01T00:00:00+00:00",
                    "disposition": "automatic_reconciliation_scheduled"
                }
            },
            "action_eligibility": {
                "actions": [{
                    "action": "resume",
                    "availability": "available",
                    "reason": "resume is legal but explicitly re-arms this admission",
                    "confirmation": "none",
                    "idempotent": true,
                    "requires_revalidation": true,
                    "result_resource_type": "agent_task_run"
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();

        assert!(summary.contains("Retry: automatic reconciliation scheduled"));
        assert!(summary.contains("Next: homeboy agent-task status unmaterialized-cook --watch\n"));
        assert!(!summary.contains("Next: homeboy agent-task recover unmaterialized-cook\n"));
    }

    #[test]
    fn local_pressure_summary_reports_process_owners_thresholds_and_retry() {
        let payload = json!({
            "schema": "homeboy/control-plane-run/v1", "run": "local-cook", "state": "queued",
            "blocker": {"code": "local_pressure", "message": "Local dispatch waiting for host pressure to drop",
                "details": {
                    "pressure": {"load1": 134}, "pressure_thresholds": {"load1": 48},
                    "pressure_holders": [{"user": "chubes", "command": "php", "count": 440, "pids": [101]}],
                    "next_attempt_at": "2026-11-21T00:00:15Z",
                    "queue_policy": "pressure polling; no ordered queue or admission ETA"
                }}
        });
        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();
        for expected in [
            "134",
            "48",
            "chubes",
            "php",
            "440",
            "101",
            "Next attempt: 2026-11-21T00:00:15Z",
            "Queue position / ETA: unknown",
        ] {
            assert!(summary.contains(expected), "{summary}");
        }
    }

    #[test]
    fn pressure_wait_summary_reports_evidence_without_inventing_a_deadline() {
        let payload = json!({
            "schema": "homeboy/control-plane-run/v1", "run": "pressure-cook", "state": "queued",
            "blocker": {
                "message": "pressure",
                "retry": {"disposition": "automatic_reconciliation_scheduled"},
                "details": {
                    "queue_position": 3,
                    "pressure": {"cpu": 9000},
                    "pressure_thresholds": {"cpu": 80},
                    "pressure_holders": [{"run_id": "holder-run"}]
                }
            }
        });
        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();
        assert!(summary.contains("no fixed retry time"));
        assert!(!summary.contains("the recorded retry time"));
        assert!(summary.contains("Queue position: 3"));
        assert!(summary.contains("9000"));
        assert!(summary.contains("Pressure thresholds:"));
        assert!(summary.contains("holder-run"));
    }

    #[test]
    fn status_summary_names_a_resource_guard_remedy_instead_of_resume() {
        let payload = json!({
            "schema": "homeboy/control-plane-run/v1",
            "run": "agent-task-301a2b9a-a63d-446b-a918-e21b2ff6421e-attempt-1-ea6a6751",
            "state": "failed",
            "artifacts": [],
            "blocker": {
                "code": "resource_guard.process_count_limit_exceeded",
                "message": "process_count=296 exceeds limit 256 (top: rustc×32, cc×21)",
                "reason": "process limit 256 is the runner's PROCESS_COUNT; run `homeboy runner set <runner> --json '{\"env\":{\"HOMEBOY_RUNNER_RESOURCE_GUARD_PROCESS_COUNT\":\"384\"}}'` then `homeboy runner service install <runner>`"
            },
            "action_eligibility": {
                "actions": [{
                    "action": "resume",
                    "availability": "available"
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();
        assert!(
            summary.contains("Blocker: resource_guard.process_count_limit_exceeded"),
            "{summary}"
        );
        assert!(
            summary.contains("(process_count=296 exceeds limit 256 (top: rustc×32, cc×21))"),
            "{summary}"
        );
        assert!(
            summary.contains("Next: process limit 256 is the runner's PROCESS_COUNT; run `homeboy runner set <runner> --json"),
            "{summary}"
        );
        assert!(
            summary.contains("then `homeboy runner service install <runner>`"),
            "{summary}"
        );
        assert!(!summary.contains("--runner-env"), "{summary}");
        assert!(
            !summary.contains("Next: homeboy agent-task recover"),
            "{summary}"
        );
    }

    #[test]
    fn status_summary_prioritizes_reconciliation_over_read_only_review() {
        let payload = json!({
            "schema": "homeboy/control-plane-run/v1",
            "run": "stale-run",
            "state": "running",
            "action_eligibility": {
                "actions": [
                    { "action": "review", "availability": "available" },
                    { "action": "reconcile", "availability": "available" }
                ]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();
        assert!(summary.contains("Next: homeboy agent-task reconcile stale-run\n"));
    }

    #[test]
    fn status_summary_projects_control_plane_candidate_gates_and_publication() {
        let payload = json!({
            "schema": "homeboy/control-plane-run/v1",
            "mission": "agent-task-cook",
            "run": "agent-task-cook-attempt-1",
            "state": "candidate_recoverable",
            "candidate": { "state": "apply_ready" },
            "gates": [{ "id": "acceptance", "state": "passed" }],
            "publication": { "state": "blocked" },
            "blocker": { "message": "finalization failed" },
            "artifacts": [{ "id": "patch", "kind": "patch", "uri": "homeboy://agent-task/run/agent-task-cook-attempt-1/artifacts#patch" }],
            "action_eligibility": {
                "schema": "homeboy/control-plane-action-eligibility/v1",
                "run": "agent-task-cook-attempt-1",
                "actions": [{
                    "action": "review",
                    "availability": "available",
                    "reason": "review is a non-mutating read available for every durable run",
                    "confirmation": "none",
                    "idempotent": true,
                    "requires_revalidation": true,
                    "result_resource_type": "agent_task_review"
                }]
            }
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();
        assert!(summary.contains("Mission: agent-task-cook\n"), "{summary}");
        assert!(summary.contains("Candidate: apply_ready\n"), "{summary}");
        assert!(summary.contains("Gates: passed\n"), "{summary}");
        assert!(summary.contains("Publication: blocked\n"), "{summary}");
        assert!(
            summary.contains("Blocker: finalization failed\n"),
            "{summary}"
        );
        assert!(summary.contains("Artifacts: 1\n"), "{summary}");
        assert!(
            summary.contains("Next: homeboy agent-task review agent-task-cook-attempt-1\n"),
            "{summary}"
        );
    }

    #[test]
    fn gate_failure_excerpt_renders_as_status_output_block() {
        let payload = json!({
            "schema": "homeboy/control-plane-run/v1",
            "mission": "agent-task-cook",
            "run": "agent-task-cook-attempt-1",
            "state": "candidate_recoverable",
            "candidate": { "state": "gate_failed" },
            "gates": [{ "id": "test", "state": "failed" }],
            "blocker": {
                "code": "gate_failed",
                "message": "deterministic gate `test` failed",
                "output": "gate: test\ncommand: cargo test\nexit code: 101\noutput:\ntest parser::rejects_empty ... FAILED",
            },
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Status, &payload).unwrap();
        assert!(
            summary.contains("Blocker: deterministic gate `test` failed\nOutput:\n  gate: test\n"),
            "{summary}"
        );
        assert!(summary.contains("  exit code: 101\n"), "{summary}");
        assert!(
            summary.contains("  test parser::rejects_empty ... FAILED\n"),
            "{summary}"
        );
    }

    #[test]
    fn logs_summary_reads_control_plane_event_page() {
        let payload = json!({
            "schema": "homeboy/control-plane-event-page/v1",
            "run": "agent-task-d1622a44",
            "events": [{
                "schema": "homeboy/control-plane-event/v1",
                "event": "agent-task-d1622a44:event:1",
                "sequence": 1,
                "run": "agent-task-d1622a44",
                "kind": "task.state_changed",
                "source": { "component": "agent-task" },
                "data": { "state": "failed", "message": "Embedded agent runtime failed." }
            }],
            "has_more": false
        });

        let summary = render_agent_task_summary(AgentTaskSummaryKind::Logs, &payload).unwrap();
        assert!(summary.starts_with("Agent task logs\nRun: agent-task-d1622a44\nEvents: 1\n"));
        assert!(render_agent_task_summary(
            AgentTaskSummaryKind::Logs,
            &json!({ "run_id": "agent-task-d1622a44", "events": [] })
        )
        .is_none());
    }
}
