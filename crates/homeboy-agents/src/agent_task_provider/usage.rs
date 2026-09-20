use serde_json::Value;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};

use crate::agent_task::{AgentTaskUsage, AGENT_TASK_USAGE_METADATA_KEY};

/// Extract OpenCode's provider-reported usage from JSONL runtime captures.
/// Event identity prevents duplicated stdout records from charging twice.
pub(crate) fn usage_from_runtime_files(paths: &[std::path::PathBuf]) -> Option<AgentTaskUsage> {
    let mut seen = BTreeSet::new();
    let mut usage = AgentTaskUsage {
        source: "opencode-jsonl".to_string(),
        ..Default::default()
    };
    let mut found = false;
    for path in paths {
        let Ok(file) = File::open(path) else { continue };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(event) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if event.get("type").and_then(Value::as_str) != Some("step_finish") {
                continue;
            }
            let Some(part) = event.get("part").and_then(Value::as_object) else {
                continue;
            };
            let identity = part
                .get("id")
                .or_else(|| part.get("messageID"))
                .map(ToString::to_string)
                .unwrap_or_else(|| line.clone());
            if !seen.insert(identity) {
                continue;
            }
            let Some(tokens) = part.get("tokens").and_then(Value::as_object) else {
                continue;
            };
            found = true;
            add(&mut usage.input_tokens, number(tokens, "input"));
            add(&mut usage.output_tokens, number(tokens, "output"));
            add(&mut usage.reasoning_tokens, number(tokens, "reasoning"));
            if let Some(cache) = tokens.get("cache").and_then(Value::as_object) {
                add(&mut usage.cache_read_tokens, number(cache, "read"));
                add(&mut usage.cache_write_tokens, number(cache, "write"));
            }
            add(&mut usage.total_tokens, number(tokens, "total"));
            if let Some(cost) = part
                .get("cost")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite())
            {
                usage.cost_usd = Some(usage.cost_usd.unwrap_or(0.0) + cost);
            }
            usage.provider = usage.provider.take().or_else(|| string(part, "providerID"));
            usage.model = usage.model.take().or_else(|| string(part, "modelID"));
        }
    }
    if !found {
        return None;
    }
    if usage.total_tokens.is_none() {
        usage.total_tokens = usage
            .input_tokens
            .zip(usage.output_tokens)
            .map(|(i, o)| i.saturating_add(o));
    }
    Some(usage)
}

fn number(value: &serde_json::Map<String, Value>, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

fn add(total: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *total = Some(total.unwrap_or(0).saturating_add(value));
    }
}

fn string(value: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
}

pub(crate) fn merge_usage_metadata(metadata: &mut Value, usage: AgentTaskUsage) {
    if !metadata.is_object() {
        *metadata = serde_json::json!({});
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            AGENT_TASK_USAGE_METADATA_KEY.to_string(),
            serde_json::to_value(usage).expect("usage serializes"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn extracts_and_deduplicates_opencode_step_usage() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("provider-runtime-stdout.log");
        let event = r#"{"type":"step_finish","part":{"id":"part-1","providerID":"openai","modelID":"gpt-5","cost":0.0125,"tokens":{"input":100,"output":20,"reasoning":4,"cache":{"read":30,"write":2}}}}"#;
        fs::write(&path, format!("{event}\n{event}\n")).expect("fixture");
        let usage = usage_from_runtime_files(&[path]).expect("usage");
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(20));
        assert_eq!(usage.total_tokens, Some(120));
        assert_eq!(usage.cache_read_tokens, Some(30));
        assert_eq!(usage.reasoning_tokens, Some(4));
        assert_eq!(usage.cost_usd, Some(0.0125));
        assert_eq!(usage.provider.as_deref(), Some("openai"));
    }
}
