//! Compact human-readable summary for `homeboy runs show`.
//!
//! `runs show` returns a `RunDetail` that embeds full run metadata and the
//! complete artifact list. For bench runs in particular, the useful evidence
//! — shared-state files, WP Codebox artifact bundles, scenario-specific
//! artifacts — is buried in a large JSON payload (#3260).
//!
//! This module renders a compact summary from the serialized `RunsOutput`
//! value, surfacing run identity, status, and (prominently) each artifact's
//! locator plus a concise `homeboy runs artifact get ...` command to inspect
//! it. The full JSON remains available via `runs show <id> --json` and is
//! always written to `--output <file>` unchanged.

use serde_json::Value;

use super::summary_json::{string_value, value_at};

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

fn render_run_detail(run: &Value) -> String {
    let run_id = string_value(run, &["id"]).unwrap_or("<unknown>");
    let kind = string_value(run, &["kind"]).unwrap_or("run");
    let status = string_value(run, &["status"]).unwrap_or("unknown");

    let mut lines = vec![
        format!("Run {run_id} ({kind})"),
        format!("Status: {status}"),
    ];

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

    lines.extend(artifact_lines(run, run_id));
    lines.push(format!("Full output: homeboy runs show {run_id} --json"));

    finish(lines)
}

/// Surface every recorded artifact with its best on-disk / network locator
/// and a concise command to fetch it (#3260). Local file paths are shown
/// directly; otherwise the public/viewer URL is shown.
fn artifact_lines(run: &Value, run_id: &str) -> Vec<String> {
    let Some(artifacts) = value_at(run, &["artifacts"]).and_then(Value::as_array) else {
        return Vec::new();
    };
    if artifacts.is_empty() {
        return vec!["Artifacts: none recorded".to_string()];
    }

    let mut lines = vec![format!("Artifacts ({}):", artifacts.len())];
    for artifact in artifacts {
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
    lines
}

fn artifact_locator(artifact: &Value) -> Option<String> {
    if string_value(artifact, &["type"]) == Some("file") {
        if let Some(path) = string_value(artifact, &["path"]) {
            return Some(path.to_string());
        }
    }
    for key in ["viewer_url", "public_url", "url", "path"] {
        if let Some(value) = string_value(artifact, &[key]) {
            return Some(value.to_string());
        }
    }
    None
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

    #[test]
    fn non_show_variant_returns_none() {
        let payload = json!({ "variant": "list", "payload": { "runs": [] } });
        assert!(render_runs_show_summary(&payload).is_none());
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

        assert!(summary.starts_with("Run bench-run-42 (bench)\nStatus: pass\n"));
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
        assert!(summary.contains("Full output: homeboy runs show bench-run-42 --json\n"));
        // URL artifacts are not fetchable via `runs artifact get`.
        assert!(!summary.contains("get: homeboy runs artifact get bench-run-42 admin_url"));
        // Compact: no raw JSON braces.
        assert!(!summary.contains("{\n"));
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
        assert!(summary.contains("Full output: homeboy runs show run-1 --json\n"));
    }
}
