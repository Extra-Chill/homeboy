//! Compact human-readable summary for `homeboy runs dossier`.

use serde_json::Value;

use super::summary_json::{string_value, usize_value, value_at};

pub(crate) fn render_runs_dossier_summary(payload: &Value) -> Option<String> {
    if payload.get("variant").and_then(Value::as_str)? != "dossier" {
        return None;
    }
    let dossier = value_at(payload, &["payload"])?;
    Some(render_dossier(dossier))
}

fn render_dossier(dossier: &Value) -> String {
    let run_id = string_value(dossier, &["run_id"]).unwrap_or("<unknown>");
    let kind = string_value(dossier, &["run", "kind"]).unwrap_or("run");
    let status = string_value(dossier, &["status", "status"]).unwrap_or("unknown");
    let mut lines = vec![
        format!("Run Dossier {run_id} ({kind})"),
        format!("Status: {status}"),
    ];

    if let Some(category) = string_value(dossier, &["status", "category"]) {
        lines.push(format!("Category: {category}"));
    }
    if let Some(reason) = string_value(dossier, &["status", "stale_reason"]) {
        lines.push(format!("Stale reason: {reason}"));
    }
    if let Some(error) = string_value(dossier, &["failure", "error"]) {
        lines.push(format!("Error: {error}"));
    }
    if let Some(gates) = value_at(dossier, &["failure", "gate_failures"]).and_then(Value::as_array)
    {
        if !gates.is_empty() {
            lines.push("Gate failures:".to_string());
            for gate in gates.iter().filter_map(Value::as_str) {
                lines.push(format!("  {gate}"));
            }
        }
    }

    lines.extend(ref_lines(dossier));
    lines.extend(env_lines(dossier));
    lines.extend(artifact_lines(dossier));
    lines.extend(command_lines(dossier, "Inspection", "inspection_commands"));
    lines.extend(command_lines(dossier, "Repair", "repair_commands"));
    lines.extend(command_lines(dossier, "Next", "next_commands"));
    lines.push(format!("Full output: homeboy runs dossier {run_id} --json"));

    let mut output = lines.join("\n");
    output.push('\n');
    output
}

fn ref_lines(dossier: &Value) -> Vec<String> {
    let mut lines = vec!["Refs:".to_string()];
    if let Some(run_ref) = string_value(dossier, &["run_ref"]) {
        lines.push(format!("  run: {run_ref}"));
    }
    for (label, path) in [
        ("job", &["refs", "job_ref"][..].as_ref()),
        ("handoff", &["refs", "handoff_ref"][..].as_ref()),
        ("result", &["refs", "result_ref"][..].as_ref()),
    ] {
        if let Some(value) = string_value(dossier, path) {
            lines.push(format!("  {label}: {value}"));
        }
    }
    lines
}

fn env_lines(dossier: &Value) -> Vec<String> {
    let Some(env) = value_at(dossier, &["env"]) else {
        return Vec::new();
    };
    let available = env
        .get("available")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !available {
        return vec!["Env provenance: not recorded".to_string()];
    }
    let keys = usize_value(env, &["key_count"]).unwrap_or(0);
    let secret = usize_value(env, &["secret_key_count"]).unwrap_or(0);
    let public = usize_value(env, &["public_key_count"]).unwrap_or(0);
    let shadowed = usize_value(env, &["shadowed_key_count"]).unwrap_or(0);
    let mut lines = vec![format!(
        "Env provenance: {keys} keys ({secret} secret, {public} public, {shadowed} shadowed)"
    )];
    if let Some(command) = string_value(env, &["command"]) {
        lines.push(format!("  inspect: {command}"));
    }
    lines
}

fn artifact_lines(dossier: &Value) -> Vec<String> {
    let count = usize_value(dossier, &["artifacts", "count"]).unwrap_or(0);
    let reviewer_visible =
        usize_value(dossier, &["artifacts", "reviewer_visible_count"]).unwrap_or(0);
    let fetchable = usize_value(dossier, &["artifacts", "fetchable_count"]).unwrap_or(0);
    let missing = usize_value(dossier, &["artifacts", "missing_count"]).unwrap_or(0);
    let mut lines = vec![format!(
        "Artifacts: {count} recorded, {reviewer_visible} reviewer-visible, {fetchable} fetchable, {missing} missing"
    )];
    if let Some(artifacts) =
        value_at(dossier, &["artifacts", "artifacts"]).and_then(Value::as_array)
    {
        for artifact in artifacts.iter().take(8) {
            let id = string_value(artifact, &["artifact_id"]).unwrap_or("artifact");
            let kind = string_value(artifact, &["kind"]).unwrap_or("");
            let hint = string_value(artifact, &["visibility_hint"]).unwrap_or("unknown visibility");
            lines.push(format!("  {id} [{kind}]: {hint}"));
            if let Some(target) = string_value(artifact, &["target"]) {
                lines.push(format!("    target: {target}"));
            }
        }
    }
    lines
}

fn command_lines(dossier: &Value, title: &str, key: &str) -> Vec<String> {
    let Some(commands) = value_at(dossier, &[key]).and_then(Value::as_array) else {
        return Vec::new();
    };
    if commands.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![format!("{title} commands:")];
    for command in commands {
        if let Some(value) = string_value(command, &["command"]) {
            lines.push(format!("  {value}"));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn renders_actionable_dossier_summary() {
        let payload = json!({
            "variant": "dossier",
            "payload": {
                "run_id": "run-1",
                "run_ref": "homeboy://run/run-1",
                "run": { "kind": "bench" },
                "status": { "status": "fail", "category": "gate_failure" },
                "failure": { "error": "boom", "gate_failures": ["budget exceeded"] },
                "refs": { "job_ref": "job-1" },
                "env": { "available": true, "key_count": 2, "secret_key_count": 1, "public_key_count": 1, "shadowed_key_count": 1, "command": "homeboy runs env run-1" },
                "artifacts": {
                    "count": 1,
                    "reviewer_visible_count": 0,
                    "fetchable_count": 1,
                    "missing_count": 0,
                    "artifacts": [{ "artifact_id": "a1", "kind": "report", "visibility_hint": "operator-local; fetch before sharing with reviewers", "target": "homeboy runs artifact get run-1 a1 -o <path>" }]
                },
                "inspection_commands": [{ "command": "homeboy runs evidence run-1" }],
                "repair_commands": [],
                "next_commands": [{ "command": "homeboy runs export --run run-1 --output <dir>" }]
            }
        });

        let summary = render_runs_dossier_summary(&payload).expect("summary");
        assert!(summary.contains("Run Dossier run-1 (bench)\n"));
        assert!(summary.contains("Category: gate_failure\n"));
        assert!(summary.contains("job: job-1\n"));
        assert!(summary.contains("Env provenance: 2 keys (1 secret, 1 public, 1 shadowed)\n"));
        assert!(
            summary.contains("Artifacts: 1 recorded, 0 reviewer-visible, 1 fetchable, 0 missing\n")
        );
        assert!(summary.contains("Full output: homeboy runs dossier run-1 --json\n"));
    }
}
