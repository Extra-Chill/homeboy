//! Stable operator projection of retained deterministic gate failures.
use serde_json::{json, Value};

/// Project the first rejecting gate, including native Lab execution evidence.
/// Successful reports (including accepted inherited failures) are not failures.
pub fn gate_failure_projection(promotion: &Value) -> Option<Value> {
    let gates = promotion.get("deterministic_gates")?.as_array()?;
    let (index, gate) = gates.iter().enumerate().find(|(_, gate)| {
        matches!(
            gate["status"].as_str(),
            Some("failed" | "failure" | "unavailable" | "deferred")
        )
    })?;
    let text = |path: &str| gate.pointer(path).and_then(Value::as_str);
    let unavailable = matches!(text("/status"), Some("unavailable" | "deferred"));
    // A gate whose environment failed before a verdict (a Homeboy setup error,
    // a missing tool, disk exhaustion) is infrastructure, not a candidate
    // failure: its next step is a re-gate, not another provider attempt.
    let environment = text("/failure_evidence/classification") == Some("environment");
    let termination = if text("/termination") == Some("cancelled") {
        "cancelled"
    } else if unavailable {
        "unavailable"
    } else {
        text("/termination")
            .or_else(|| text("/lab_receipt/termination"))
            .unwrap_or("completed")
    };
    let reason = if unavailable || (environment && termination == "completed") {
        "gate_environment_unavailable"
    } else if matches!(termination, "timed_out" | "no_progress" | "cancelled") {
        termination
    } else if text("/baseline_comparison/result") == Some("baseline_red")
        || promotion["reason_code"] == "baseline_red"
    {
        "baseline_red"
    } else {
        "gate_failed"
    };
    let revealed = text("/visibility") != Some("private")
        || text("/reveal_policy").unwrap_or("full_evidence") == "full_evidence";
    let command = revealed
        .then(|| {
            text("/failure_evidence/command")
                .map(str::to_owned)
                .or_else(|| {
                    gate.get("command").and_then(|command| match command {
                        Value::String(command) => Some(command.clone()),
                        Value::Array(parts) => Some(
                            parts
                                .iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(" "),
                        ),
                        _ => None,
                    })
                })
                .map(|command| homeboy_core::redaction::redact_string(&command))
        })
        .flatten();
    let output_tail = revealed.then(|| {
        let stream = |name: &str| {
            [
                format!("/{name}"),
                format!("/lab_receipt/{name}"),
                format!("/failure_evidence/{name}_tail"),
            ]
            .iter()
            .find_map(|path| text(path).filter(|s| !s.is_empty()))
            .unwrap_or("")
        };
        let combined = [stream("stdout"), stream("stderr")]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let combined = if combined.is_empty() && unavailable {
            text("/lab_receipt/diagnostic/details/diagnostic/details/stderr")
                .or_else(|| text("/lab_receipt/diagnostic/details/stderr"))
                .or_else(|| text("/lab_receipt/diagnostic/message"))
                .unwrap_or("")
        } else {
            &combined
        };
        let lines = combined.lines().collect::<Vec<_>>();
        homeboy_core::redaction::redact_string(&lines[lines.len().saturating_sub(40)..].join("\n"))
    });
    let label = text("/label").or_else(|| text("/id")).unwrap_or("verify");
    Some(json!({
        "gate": format!("{label}#{}", index + 1),
        "command": command,
        "exit_code": gate.get("exit_code").or_else(|| gate.pointer("/lab_receipt/exit_code")).or_else(|| gate.pointer("/failure_evidence/exit_code")),
        "termination": termination,
        "reason": reason,
        "output_tail": output_tail,
        "artifact": gate.pointer("/lab_receipt/artifact_ref").or_else(|| gate.get("artifact_ref")),
    }))
}

/// A classification alone determines the next gate recovery action. The run
/// identity only supplies the command's target, never the choice of action.
pub fn gate_failure_next_action(reason: &str, run: &str) -> (String, String) {
    let regate = matches!(
        reason,
        "timed_out" | "no_progress" | "gate_environment_unavailable" | "baseline_red"
    );
    if regate {
        (format!("homeboy agent-task recover {run} --regate"),
         "Re-run retained gates; add --gate-timeout-seconds / --gate-no-progress-timeout-seconds (0 disables it) to this regate to change the supervision budget.".into())
    } else {
        (
            format!("homeboy agent-task recover {run}"),
            "Start a new provider attempt with the retained gate feedback.".into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A gate whose setup failed before a verdict projects as infrastructure
    /// with a re-gate next step and keeps the setup error in its output.
    #[test]
    fn environment_gate_failure_projects_as_unavailable_infrastructure() {
        let setup_error = "review test cannot wait for a Lab runner";
        let report = json!({"deterministic_gates": [{
            "id": "gate-2", "status": "failed", "exit_code": 2,
            "command": "homeboy review test homeboy",
            "failure_evidence": {
                "classification": "environment",
                "stdout_tail": format!("{{\"success\":false,\"diagnostics\":{{\"message\":\"{setup_error}\"}}}}"),
            }
        }]});
        let failure = gate_failure_projection(&report).expect("failed gate");
        assert_eq!(failure["reason"], "gate_environment_unavailable");
        assert!(failure["output_tail"]
            .as_str()
            .unwrap()
            .contains(setup_error));
        let (command, _) = gate_failure_next_action("gate_environment_unavailable", "run");
        assert_eq!(command, "homeboy agent-task recover run --regate");

        let mut candidate = report.clone();
        candidate["deterministic_gates"][0]["failure_evidence"]["classification"] =
            json!("candidate_code");
        let failure = gate_failure_projection(&candidate).expect("failed gate");
        assert_eq!(failure["reason"], "gate_failed");
    }
}
