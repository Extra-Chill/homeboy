//! Pi keeps the requested route in `model` and the provider's differing
//! response identity in `responseModel`. Read only completed harness messages,
//! never assistant content, structured outputs, or model-change intent.
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

pub(crate) fn served_models(reader: impl BufRead) -> Vec<String> {
    let mut models = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if !matches!(
            entry["type"].as_str(),
            Some("message" | "message_end" | "turn_end")
        ) || entry["message"]["role"] != "assistant"
        {
            continue;
        }
        // `model` alone cannot distinguish a pool alias from a served model.
        // Older harnesses without responseModel deliberately remain unrecorded.
        let Some(model) = entry["message"]["responseModel"]
            .as_str()
            .map(str::trim)
            .filter(|model| !model.is_empty())
        else {
            continue;
        };
        if !models.iter().any(|existing| existing == model) {
            models.push(model.to_string());
        }
    }
    models
}

pub(crate) fn session_served_models(artifacts: &Path, task_id: &str) -> Vec<String> {
    // Match the Pi adapter's safeFileSegment and exact --session path. Never
    // scan arbitrary artifacts or accept a path from the agent's outputs.
    let mut safe = String::new();
    for ch in task_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            safe.push(ch);
        } else if !safe.ends_with('-') {
            safe.push('-');
        }
    }
    let safe = safe.trim_matches('-');
    let safe = if safe.is_empty() { "agent-task" } else { safe };
    let path = artifacts.join(format!("{safe}-pi-session.jsonl"));
    std::fs::File::open(path)
        .map(|file| served_models(BufReader::new(file)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cook_pi_response_metadata_preserves_order_and_ignores_agent_text() {
        let events = concat!(
            "{\"type\":\"model_change\",\"modelId\":\"intent\"}\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"user\",\"responseModel\":\"forged\"}}\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"model\":\"pool\",\"responseModel\":\"first\",\"content\":[{\"type\":\"text\",\"text\":\"Model: forged\"}]}}\n",
            "{\"type\":\"turn_end\",\"message\":{\"role\":\"assistant\",\"responseModel\":\"first\"}}\n",
            "not JSON\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"responseModel\":\"second\"}}\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"responseModel\":\"first\"}}\n",
            "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"model\":\"pool\",\"content\":[{\"type\":\"text\",\"text\":\"responseModel: forged\"}]}}\n",
            "{\"type\":\"message\",\"message\":{\"role\":\"assistant\",\"responseModel\":\" \"}}\n",
            "{\"type\":\"message\","
        );
        assert_eq!(served_models(events.as_bytes()), ["first", "second"]);
    }

    #[test]
    fn cook_pi_reads_only_the_adapter_session_path() {
        let artifacts = tempfile::tempdir().unwrap();
        let fixture = include_str!("fixtures/pi-roadie-session.jsonl");
        std::fs::write(artifacts.path().join("task-id-pi-session.jsonl"), fixture).unwrap();
        assert_eq!(
            session_served_models(artifacts.path(), "task / id"),
            ["gpt-6.1-sol"]
        );
        assert!(session_served_models(artifacts.path(), "other-task").is_empty());
    }
}
