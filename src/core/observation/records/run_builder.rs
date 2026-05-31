use std::path::Path;

use crate::core::observation::context::RunContext;

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
        self.record.homeboy_version = Some(env!("CARGO_PKG_VERSION").to_string());
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

    pub fn run_context(mut self, run_context: RunContext) -> Self {
        self.record.run_context = run_context;
        self
    }

    pub fn build(self) -> NewRunRecord {
        self.record
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new() {
        let record = NewRunRecordBuilder::new("test").build();

        assert_eq!(record.kind, "test");
        assert!(record.component_id.is_none());
    }

    #[test]
    fn test_component_id() {
        let record = NewRunRecord::builder("lint")
            .component_id("homeboy")
            .build();

        assert_eq!(record.component_id.as_deref(), Some("homeboy"));
    }

    #[test]
    fn test_command() {
        let record = NewRunRecord::builder("lint")
            .command("homeboy lint homeboy")
            .build();

        assert_eq!(record.command.as_deref(), Some("homeboy lint homeboy"));
    }

    #[test]
    fn test_cwd_path() {
        let record = NewRunRecord::builder("lint")
            .cwd_path(Path::new("/tmp/homeboy"))
            .build();

        assert_eq!(record.cwd.as_deref(), Some("/tmp/homeboy"));
    }

    #[test]
    fn test_optional_cwd_path() {
        let record = NewRunRecord::builder("triage")
            .optional_cwd_path(Some(Path::new("/tmp/homeboy")))
            .build();

        assert_eq!(record.cwd.as_deref(), Some("/tmp/homeboy"));
    }

    #[test]
    fn test_current_homeboy_version() {
        let record = NewRunRecord::builder("lint")
            .current_homeboy_version()
            .build();

        assert_eq!(
            record.homeboy_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn test_homeboy_version() {
        let record = NewRunRecord::builder("lint")
            .homeboy_version("test-version")
            .build();

        assert_eq!(record.homeboy_version.as_deref(), Some("test-version"));
    }

    #[test]
    fn test_git_sha() {
        let record = NewRunRecord::builder("lint")
            .git_sha(Some("abc123".to_string()))
            .build();

        assert_eq!(record.git_sha.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_rig_id() {
        let record = NewRunRecord::builder("bench").rig_id("studio").build();

        assert_eq!(record.rig_id.as_deref(), Some("studio"));
    }

    #[test]
    fn test_optional_rig_id() {
        let record = NewRunRecord::builder("bench")
            .optional_rig_id(Some("studio"))
            .build();

        assert_eq!(record.rig_id.as_deref(), Some("studio"));
    }

    #[test]
    fn test_metadata() {
        let record = NewRunRecord::builder("lint")
            .metadata(serde_json::json!({ "source": "homeboy lint" }))
            .build();

        assert_eq!(record.metadata_json["source"], "homeboy lint");
    }

    #[test]
    fn test_build() {
        let record = NewRunRecord::builder("lint")
            .component_id("homeboy")
            .command("homeboy lint homeboy")
            .cwd_path(Path::new("/tmp/homeboy"))
            .current_homeboy_version()
            .build();

        assert_eq!(record.kind, "lint");
        assert_eq!(record.component_id.as_deref(), Some("homeboy"));
        assert_eq!(record.command.as_deref(), Some("homeboy lint homeboy"));
        assert_eq!(record.cwd.as_deref(), Some("/tmp/homeboy"));
        assert_eq!(
            record.homeboy_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
    }
}
