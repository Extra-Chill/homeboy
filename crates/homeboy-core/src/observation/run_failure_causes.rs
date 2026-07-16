use serde::{Deserialize, Serialize};
use serde_json::Value;

const FAILURE_CAUSE_LIMIT: usize = 4;
const STRUCTURED_ARTIFACT_READ_LIMIT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunFailureCause {
    pub surface: String,
    pub message: String,
    pub source: String,
}

impl RunFailureCause {
    fn priority(&self) -> u8 {
        match self.surface.as_str() {
            "recipe" | "browser" => 0,
            "selected_runtime" => 1,
            "wrapper/parser" => 2,
            _ => 9,
        }
    }
}

/// Promote useful nested failure causes from a serialized run detail.
///
/// Lab/Managed Sandbox runs can bury the actionable error in structured artifact
/// JSON. This scans failed-run metadata, artifact metadata, and small JSON
/// artifact files, then returns a bounded, deduped list ordered by usefulness.
pub fn nested_failure_causes_from_run_detail(run: &Value) -> Vec<RunFailureCause> {
    if !run_failed(run) {
        return Vec::new();
    }

    let mut causes = Vec::new();
    if let Some(metadata) = value_at(run, &["metadata"]) {
        collect_failure_causes(metadata, "metadata", &mut causes);
    }
    if let Some(artifacts) = value_at(run, &["artifacts"]).and_then(Value::as_array) {
        for artifact in artifacts {
            let artifact_id = string_value(artifact, &["id"]).unwrap_or("artifact");
            let artifact_kind = string_value(artifact, &["kind"]).unwrap_or("");
            let source = if artifact_kind.is_empty() {
                format!("artifact {artifact_id}")
            } else {
                format!("artifact {artifact_id} [{artifact_kind}]")
            };
            if let Some(metadata) =
                value_at(artifact, &["metadata_json"]).or_else(|| value_at(artifact, &["metadata"]))
            {
                collect_failure_causes(metadata, &source, &mut causes);
            }
            if let Some(value) = structured_artifact_json(artifact, &source, &mut causes) {
                collect_failure_causes(&value, &source, &mut causes);
            }
        }
    }

    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for cause in causes {
        let key = (
            cause.surface.clone(),
            cause.message.to_ascii_lowercase(),
            cause.source.clone(),
        );
        if seen.insert(key) {
            deduped.push(cause);
        }
    }
    deduped.sort_by_key(RunFailureCause::priority);
    deduped.truncate(FAILURE_CAUSE_LIMIT);
    deduped
}

fn run_failed(run: &Value) -> bool {
    matches!(
        string_value(run, &["status"]),
        Some("fail" | "failed" | "error" | "stale")
    )
}

fn structured_artifact_json(
    artifact: &Value,
    source: &str,
    causes: &mut Vec<RunFailureCause>,
) -> Option<Value> {
    let path = string_value(artifact, &["path"])?;
    if !looks_like_structured_artifact(artifact, path) {
        return None;
    }
    let Ok(metadata) = std::fs::metadata(path) else {
        return None;
    };
    if !metadata.is_file() || metadata.len() > STRUCTURED_ARTIFACT_READ_LIMIT_BYTES {
        return None;
    }
    let Ok(body) = std::fs::read_to_string(path) else {
        return None;
    };
    match serde_json::from_str::<Value>(&body) {
        Ok(value) => Some(value),
        Err(err) => {
            causes.push(RunFailureCause {
                surface: "wrapper/parser".to_string(),
                message: format!("could not parse structured artifact JSON: {err}"),
                source: source.to_string(),
            });
            None
        }
    }
}

fn looks_like_structured_artifact(artifact: &Value, path: &str) -> bool {
    if path.ends_with(".json") {
        return true;
    }
    matches!(
        string_value(artifact, &["mime"]),
        Some("application/json" | "text/json")
    )
}

fn collect_failure_causes(value: &Value, source: &str, out: &mut Vec<RunFailureCause>) {
    collect_failure_causes_at(value, source, "", out);
}

fn collect_failure_causes_at(
    value: &Value,
    source: &str,
    context: &str,
    out: &mut Vec<RunFailureCause>,
) {
    match value {
        Value::Object(map) => {
            let node_context = object_context(value, context);
            if object_indicates_failure(value) {
                if let Some(message) = object_failure_message(value) {
                    out.push(RunFailureCause {
                        surface: classify_failure_surface(&node_context, &message),
                        message,
                        source: source.to_string(),
                    });
                }
            }
            if let Some(Value::Array(diagnostics)) = map.get("diagnostics") {
                for diagnostic in diagnostics {
                    if let Some(message) = object_failure_message(diagnostic) {
                        let diagnostic_context = object_context(diagnostic, &node_context);
                        out.push(RunFailureCause {
                            surface: classify_failure_surface(&diagnostic_context, &message),
                            message,
                            source: source.to_string(),
                        });
                    }
                }
            }
            for (key, nested) in map {
                let next_context = append_context(&node_context, key);
                collect_failure_causes_at(nested, source, &next_context, out);
            }
        }
        Value::Array(items) => {
            for nested in items {
                collect_failure_causes_at(nested, source, context, out);
            }
        }
        _ => {}
    }
}

fn object_context(value: &Value, base: &str) -> String {
    let mut context = base.to_string();
    for key in [
        "class",
        "kind",
        "code",
        "type",
        "surface",
        "phase",
        "component",
    ] {
        if let Some(value) = string_value(value, &[key]) {
            context = append_context(&context, value);
        }
    }
    context
}

fn append_context(base: &str, value: &str) -> String {
    if base.is_empty() {
        value.to_string()
    } else {
        format!("{base} {value}")
    }
}

fn object_indicates_failure(value: &Value) -> bool {
    value.get("success").and_then(Value::as_bool) == Some(false)
        || string_value(value, &["status"]).is_some_and(failure_word)
        || string_value(value, &["state"]).is_some_and(failure_word)
        || value.get("error").is_some()
        || value.get("failure").is_some()
}

fn failure_word(value: &str) -> bool {
    matches!(value, "fail" | "failed" | "error" | "errored" | "blocked")
}

fn object_failure_message(value: &Value) -> Option<String> {
    for path in [
        &["message"][..],
        &["diagnostic"][..],
        &["summary"][..],
        &["reason"][..],
        &["error", "message"][..],
        &["failure", "message"][..],
    ] {
        if let Some(message) = string_value(value, path) {
            if useful_failure_message(message) {
                return Some(message.to_string());
            }
        }
    }
    value
        .get("error")
        .and_then(Value::as_str)
        .filter(|message| useful_failure_message(message))
        .map(str::to_string)
}

fn useful_failure_message(message: &str) -> bool {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return false;
    }
    !matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "failed" | "failure" | "error" | "false"
    )
}

fn classify_failure_surface(context: &str, message: &str) -> String {
    let haystack = format!("{context} {message}").to_ascii_lowercase();
    if haystack.contains("browser")
        || haystack.contains("playwright")
        || haystack.contains("page.")
        || haystack.contains("console")
        || haystack.contains("network")
    {
        "browser".to_string()
    } else if haystack.contains("recipe")
        || haystack.contains("schema")
        || haystack.contains("validation")
        || haystack.contains("required step")
    {
        "recipe".to_string()
    } else if haystack.contains("parse")
        || haystack.contains("parser")
        || haystack.contains("wrapper")
        || haystack.contains("structured output")
        || haystack.contains("invalid json")
    {
        "wrapper/parser".to_string()
    } else if haystack.contains("sandbox") || haystack.contains("selected_runtime") {
        "selected_runtime".to_string()
    } else {
        "nested".to_string()
    }
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn string_value<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    value_at(value, path)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}
