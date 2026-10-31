//! Shared fake of Pi's CLI, not the retired extension executor protocol.
use super::*;

pub(crate) struct FakePi {
    pub root: tempfile::TempDir,
    pub command: PathBuf,
}

impl FakePi {
    /// `body` runs only for inference. It may emit events, sleep, write evidence,
    /// print diagnostics, or fail. Probe bodies are independently configurable.
    pub fn new(body: &str) -> Self {
        Self::with_probes(body, "printf 'test-pi\\n'", "printf 'test model\\n'")
    }

    pub fn with_probes(body: &str, version: &str, models: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let command = root.path().join("pi");
        std::fs::write(&command, format!("#!/bin/sh\ncase \"$1\" in\n--version) {version}; exit $?;;\n--list-models) {models}; exit $?;;\nesac\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self { root, command }
    }

    pub fn success() -> Self {
        Self::new(&Self::events(include_str!("fixtures/pi-success.jsonl")))
    }

    pub fn events(stream: &str) -> String {
        format!("cat <<'HOMEBOY_PI_EVENTS'\n{stream}\nHOMEBOY_PI_EVENTS")
    }

    pub fn answer(text: &str) -> String {
        Self::events(&json!({"type":"message_end", "message":{"role":"assistant", "stopReason":"stop", "content":[{"type":"text","text":text}]}}).to_string())
    }

    pub fn config(&self) -> Value {
        json!({"command": self.command, "command_args": ["--mode", "json"]})
    }

    pub fn provider(&self) -> AgentTaskExecutorProvider {
        let mut provider = discovery::discover_agent_task_executor_providers().remove(0);
        provider
            .provider_defaults
            .insert("pi".into(), self.config());
        provider
    }

    pub fn request(&self, task: &str) -> AgentTaskRequest {
        serde_json::from_value(json!({
            "schema": AGENT_TASK_REQUEST_SCHEMA, "task_id":task,
            "executor":{"backend":"pi", "model":"test/model", "config":self.config()},
            "instructions":"Run the task", "limits":{"timeout_ms":10000}
        }))
        .unwrap()
    }
}
