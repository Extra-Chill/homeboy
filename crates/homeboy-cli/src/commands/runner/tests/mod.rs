use std::collections::HashMap;

use homeboy::core::runners::{Runner, RunnerKind};
use homeboy::core::server::{RunnerPolicy, RunnerSettings};

mod exec;
mod redaction;
mod status;

/// Shared fixture: a local runner carrying one sensitive and one public env var.
pub(super) fn runner_with_env(id: &str) -> Runner {
    Runner {
        id: id.to_string(),
        kind: RunnerKind::Local,
        server_id: None,
        workspace_root: None,
        settings: RunnerSettings::default(),
        env: HashMap::from([
            ("OPENCODE_API_KEY".to_string(), "secret-token".to_string()),
            (
                "HOMEBOY_PUBLIC_ARTIFACT_BASE_URL".to_string(),
                "https://artifacts.example.test".to_string(),
            ),
        ]),
        secret_env: HashMap::new(),
        resources: HashMap::new(),
        policy: RunnerPolicy::default(),
    }
}
