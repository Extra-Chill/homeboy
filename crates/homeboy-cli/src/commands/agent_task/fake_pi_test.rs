//! Fake `pi` CLI for agent-task command tests: a local equivalent of the
//! agents crate's private `FakePi` fixture.

pub(crate) struct FakePi {
    _root: tempfile::TempDir,
    command: std::path::PathBuf,
}

impl FakePi {
    pub(crate) fn with_probes(body: &str, models: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let command = root.path().join("pi");
        std::fs::write(&command, format!("#!/bin/sh\ncase \"$1\" in\n--version) printf 'test-pi\\n'; exit 0;;\n--list-models) {models}; exit $?;;\nesac\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&command, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self {
            _root: root,
            command,
        }
    }

    pub(crate) fn install_catalog_config(&self) {
        let mut config = homeboy::core::defaults::load_config();
        config
            .settings
            .extend(self.config().as_object().unwrap().clone());
        homeboy::core::defaults::save_config(&config).unwrap();
    }

    pub(crate) fn config(&self) -> serde_json::Value {
        serde_json::json!({"command": self.command, "command_args": ["--mode", "json"]})
    }

    pub(crate) fn provider(
        &self,
    ) -> homeboy::agents::agent_task_provider::AgentTaskExecutorProvider {
        let mut provider =
            homeboy::agents::agent_task_provider::AgentTaskProviderCatalog::discover()
                .providers
                .remove(0);
        provider
            .provider_defaults
            .insert("pi".into(), self.config());
        provider.cli.profiles.clear();
        provider
    }
}
