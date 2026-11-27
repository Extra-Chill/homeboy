use std::path::Path;

use crate::observation::context::RunContext;

use super::NewRunRecord;

#[derive(Debug, Clone)]
pub struct NewRunRecordBuilder {
    record: NewRunRecord,
}

impl NewRunRecordBuilder {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            record: NewRunRecord {
                kind: kind.into(),
                component_id: None,
                command: None,
                cwd: None,
                homeboy_version: None,
                git_sha: None,
                rig_id: None,
                metadata_json: serde_json::json!({}),
                run_context: RunContext::empty(),
            },
        }
    }

    pub fn component_id(mut self, component_id: impl Into<String>) -> Self {
        self.record.component_id = Some(component_id.into());
        self
    }

    pub fn command(mut self, command: impl Into<String>) -> Self {
        self.record.command = Some(command.into());
        self
    }

    pub fn cwd_path(mut self, path: &Path) -> Self {
        self.record.cwd = Some(path.to_string_lossy().to_string());
        self
    }

    pub fn optional_cwd_path(mut self, path: Option<&Path>) -> Self {
        self.record.cwd = path.map(|path| path.to_string_lossy().to_string());
        self
    }

    pub fn current_homeboy_version(mut self) -> Self {
        self.record.homeboy_version = Some(crate::build_identity::current().version);
        self
    }

    pub fn homeboy_version(mut self, homeboy_version: impl Into<String>) -> Self {
        self.record.homeboy_version = Some(homeboy_version.into());
        self
    }

    pub fn git_sha(mut self, git_sha: Option<String>) -> Self {
        self.record.git_sha = git_sha;
        self
    }

    pub fn rig_id(mut self, rig_id: impl Into<String>) -> Self {
        self.record.rig_id = Some(rig_id.into());
        self
    }

    pub fn optional_rig_id(mut self, rig_id: Option<impl Into<String>>) -> Self {
        self.record.rig_id = rig_id.map(Into::into);
        self
    }

    pub fn metadata(mut self, metadata_json: serde_json::Value) -> Self {
        self.record.metadata_json = metadata_json;
        self
    }

    pub fn build(self) -> NewRunRecord {
        self.record
    }
}
