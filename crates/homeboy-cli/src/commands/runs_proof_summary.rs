//! Compact human-readable summary for `homeboy runs proof`.
//!
//! `runs proof` already returns a small signal-only payload. This renderer
//! presents it as ~10 lines of `key: value` text so an agent verifying a run
//! reads the verdict and declared proof/scorecard signals without parsing
//! JSON. The full payload remains available via `runs proof <id> --json`.

use serde_json::Value;

use super::summary_json::{string_value, value_at};

/// Render a compact summary for a serialized `RunsOutput` value. Returns `None`
/// for any variant other than `proof`.
pub(crate) fn render_runs_proof_summary(payload: &Value) -> Option<String> {
    if payload.get("variant").and_then(Value::as_str)? != "proof" {
        return None;
    }
    let proof = value_at(payload, &["payload"])?;
    Some(render(proof))
}

fn render(proof: &Value) -> String {
    let run_id = string_value(proof, &["run_id"]).unwrap_or("<unknown>");
    let kind = string_value(proof, &["kind"]).unwrap_or("run");
    let status = string_value(proof, &["status"]).unwrap_or("unknown");
    let verdict = match proof.get("passed") {
        Some(Value::Bool(true)) => "PASS",
        Some(Value::Bool(false)) => "FAIL",
        _ => "PENDING",
    };

    let mut lines = vec![
        format!("Proof {run_id} ({kind})"),
        format!("Verdict: {verdict} (status: {status})"),
    ];

    if let Some(code) = proof.get("exit_code").and_then(Value::as_i64) {
        lines.push(format!("Exit code: {code}"));
    }
    if let Some(error) = string_value(proof, &["error"]) {
        lines.push(format!("Error: {error}"));
    }

    let gate_failures = proof
        .get("gate_failures")
        .and_then(Value::as_array)
        .filter(|gates| !gates.is_empty());
    if let Some(gates) = gate_failures {
        lines.push(format!("Gate failures ({}):", gates.len()));
        for gate in gates.iter().filter_map(Value::as_str) {
            lines.push(format!("  {gate}"));
        }
    }

    match proof.get("signals").and_then(Value::as_object) {
        Some(signals) if !signals.is_empty() => {
            lines.push(format!("Signals ({}):", signals.len()));
            for (key, value) in signals {
                lines.push(format!("  {key}: {}", scalar_display(value)));
            }
        }
        _ => lines.push("Signals: none declared".to_string()),
    }

    lines.push(format!("Full output: homeboy runs proof {run_id} --json"));

    let mut output = lines.join("\n");
    output.push('\n');
    output
}

fn scalar_display(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn non_proof_variant_returns_none() {
        let payload = json!({ "variant": "show", "payload": {} });
        assert!(render_runs_proof_summary(&payload).is_none());
    }

    #[test]
    fn proof_summary_surfaces_verdict_gates_and_signals() {
        let payload = json!({
            "variant": "proof",
            "payload": {
                "command": "runs.proof",
                "run_id": "bench-run-42",
                "kind": "bench",
                "status": "fail",
                "passed": false,
                "exit_code": 2,
                "error": "gate exceeded",
                "gate_failures": ["p95_ms exceeded"],
                "signals": {
                    "cold.p95_ms": 42.0,
                    "proof.opfs_resume": false,
                    "observation_status": "failed"
                }
            }
        });

        let summary = render_runs_proof_summary(&payload).expect("summary");

        assert!(summary.starts_with("Proof bench-run-42 (bench)\n"));
        assert!(summary.contains("Verdict: FAIL (status: fail)\n"));
        assert!(summary.contains("Exit code: 2\n"));
        assert!(summary.contains("Error: gate exceeded\n"));
        assert!(summary.contains("Gate failures (1):\n"));
        assert!(summary.contains("  p95_ms exceeded\n"));
        assert!(summary.contains("Signals (3):\n"));
        assert!(summary.contains("  cold.p95_ms: 42.0\n"));
        assert!(summary.contains("  proof.opfs_resume: false\n"));
        assert!(summary.contains("  observation_status: failed\n"));
        assert!(summary.contains("Full output: homeboy runs proof bench-run-42 --json\n"));
        // Compact: no raw JSON braces.
        assert!(!summary.contains("{\n"));
    }

    #[test]
    fn proof_summary_reports_pending_and_no_signals() {
        let payload = json!({
            "variant": "proof",
            "payload": {
                "run_id": "run-1",
                "kind": "bench",
                "status": "running",
                "passed": null,
                "signals": {}
            }
        });

        let summary = render_runs_proof_summary(&payload).expect("summary");
        assert!(summary.contains("Verdict: PENDING (status: running)\n"));
        assert!(summary.contains("Signals: none declared\n"));
    }
}
