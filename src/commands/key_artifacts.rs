use serde_json::Value;

use super::summary_json::string_value;

const KEY_ARTIFACT_MARKERS: &[&str] = &[
    "admin_page_coverage",
    "route_inventory",
    "coverage",
    "case_artifact",
    "failing_case",
    "failure_case",
    "fuzz_case",
    "key_case",
    "minimal_repro",
    "raw_result",
    "repro_case",
];

pub(crate) fn key_artifact_lines(
    artifacts: &[Value],
    run_id: Option<&str>,
    include_type_guard: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    for artifact in artifacts
        .iter()
        .filter(|artifact| is_key_artifact(artifact))
    {
        let scenario = string_value(artifact, &["scenario_id"]).unwrap_or("global");
        let name = artifact_name(artifact);
        let mut line = format!("  {scenario}/{name}");
        if let Some(locator) = artifact_locator(artifact) {
            line.push_str(": ");
            line.push_str(locator);
        }
        lines.push(line);
        if let (Some(run_id), Some(artifact_id)) = (run_id, artifact_get_id(artifact)) {
            if !include_type_guard || string_value(artifact, &["type"]) == Some("file") {
                lines.push(format!(
                    "    get: homeboy runs artifact get {run_id} {artifact_id} -o <path>"
                ));
            }
        }
    }
    if !lines.is_empty() {
        lines.insert(0, "Key artifacts:".to_string());
    }
    lines
}

pub(crate) fn artifact_locator(artifact: &Value) -> Option<&str> {
    for key in [
        "path",
        "local_url",
        "viewer_url",
        "preview_url",
        "public_url",
        "url",
    ] {
        // Artifacts record absent locators as empty strings (e.g. a URL-only
        // artifact still carries `"path": ""`). Skip blanks so the fallthrough
        // reaches the populated locator key instead of rendering an empty one.
        if let Some(value) = string_value(artifact, &[key]).filter(|value| !value.is_empty()) {
            return Some(value);
        }
    }
    None
}

fn is_key_artifact(artifact: &Value) -> bool {
    [
        string_value(artifact, &["name"]),
        string_value(artifact, &["kind"]),
        string_value(artifact, &["id"]),
        string_value(artifact, &["artifact_id"]),
        string_value(artifact, &["observation_artifact_id"]),
    ]
    .into_iter()
    .flatten()
    .any(|value| {
        let normalized = value.to_ascii_lowercase().replace('-', "_");
        KEY_ARTIFACT_MARKERS.contains(&normalized.as_str())
    })
}

fn artifact_name(artifact: &Value) -> &str {
    string_value(artifact, &["name"])
        .or_else(|| string_value(artifact, &["id"]))
        .or_else(|| string_value(artifact, &["artifact_id"]))
        .or_else(|| string_value(artifact, &["kind"]))
        .unwrap_or("artifact")
}

fn artifact_get_id(artifact: &Value) -> Option<&str> {
    string_value(artifact, &["observation_artifact_id"])
        .or_else(|| string_value(artifact, &["id"]))
        .or_else(|| string_value(artifact, &["artifact_id"]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn highlights_generic_key_artifacts_with_get_commands() {
        let artifacts = vec![
            json!({
                "scenario_id": "admin",
                "name": "coverage",
                "observation_artifact_id": "artifact-1",
                "path": "/tmp/coverage.json"
            }),
            json!({
                "scenario_id": "routes",
                "name": "route-inventory",
                "id": "artifact-2",
                "viewer_url": "https://example.test/routes"
            }),
            json!({ "scenario_id": "other", "name": "transcript", "path": "/tmp/log.txt" }),
        ];

        let lines = key_artifact_lines(&artifacts, Some("run-1"), false).join("\n");

        assert!(lines.starts_with("Key artifacts:\n"));
        assert!(lines.contains("  admin/coverage: /tmp/coverage.json\n"));
        assert!(lines.contains("    get: homeboy runs artifact get run-1 artifact-1 -o <path>\n"));
        assert!(lines.contains("  routes/route-inventory: https://example.test/routes\n"));
        assert!(lines.contains("    get: homeboy runs artifact get run-1 artifact-2 -o <path>"));
        assert!(!lines.contains("transcript"));
    }

    #[test]
    fn can_require_file_type_for_runs_artifacts() {
        let artifacts = vec![
            json!({ "id": "coverage", "type": "url", "url": "https://example.test" }),
            json!({ "id": "raw_result", "type": "file", "path": "/tmp/raw.json" }),
        ];

        let lines = key_artifact_lines(&artifacts, Some("run-1"), true).join("\n");

        assert!(lines.contains("  global/coverage: https://example.test\n"));
        assert!(!lines.contains("homeboy runs artifact get run-1 coverage"));
        assert!(lines.contains("homeboy runs artifact get run-1 raw_result -o <path>"));
    }
}
