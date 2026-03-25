mod component_set_flags;
mod helpers;
mod projects;
mod types;

pub use component_set_flags::*;
pub use helpers::*;
pub use projects::*;
pub use types::*;

use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

use homeboy::component::{self, Component};
use homeboy::project::{self, Project};
use homeboy::EntityCrudOutput;

use super::{CmdResult, DynamicSetArgs};

impl ComponentSetFlags {
    fn has_any(&self) -> bool {
        self.local_path.is_some()
            || self.remote_path.is_some()
            || self.build_artifact.is_some()
            || self.extract_command.is_some()
            || self.changelog_target.is_some()
    }

    /// Insert non-None fields into a JSON object.
    fn apply_to(&self, obj: &mut serde_json::Map<String, serde_json::Value>) {
        if let Some(ref v) = self.local_path {
            obj.insert("local_path".to_string(), serde_json::json!(v));
        }
        if let Some(ref v) = self.remote_path {
            obj.insert("remote_path".to_string(), serde_json::json!(v));
        }
        if let Some(ref v) = self.build_artifact {
            obj.insert("build_artifact".to_string(), serde_json::json!(v));
        }
        if let Some(ref v) = self.extract_command {
            obj.insert("extract_command".to_string(), serde_json::json!(v));
        }
        if let Some(ref v) = self.changelog_target {
            obj.insert("changelog_target".to_string(), serde_json::json!(v));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_component_set_flags_has_any_all_none() {
        let flags = ComponentSetFlags {
            local_path: None,
            remote_path: None,
            build_artifact: None,
            extract_command: None,
            changelog_target: None,
        };
        assert!(!flags.has_any());
    }

    #[test]
    fn test_component_set_flags_has_any_single_field() {
        let flags = ComponentSetFlags {
            local_path: Some("/foo".to_string()),
            remote_path: None,
            build_artifact: None,
            extract_command: None,
            changelog_target: None,
        };
        assert!(flags.has_any());
    }

    #[test]
    fn test_component_set_flags_apply_to_inserts_fields() {
        let flags = ComponentSetFlags {
            local_path: Some("/new/path".to_string()),
            remote_path: None,
            build_artifact: None,
            extract_command: Some("unzip -o artifact.zip".to_string()),
            changelog_target: Some("CHANGELOG.md".to_string()),
        };

        let mut obj = serde_json::Map::new();
        flags.apply_to(&mut obj);

        assert_eq!(obj.len(), 3);
        assert_eq!(obj["local_path"], serde_json::json!("/new/path"));
        assert_eq!(
            obj["extract_command"],
            serde_json::json!("unzip -o artifact.zip")
        );
        assert_eq!(obj["changelog_target"], serde_json::json!("CHANGELOG.md"));
        assert!(!obj.contains_key("remote_path"));
    }

    #[test]
    fn test_component_set_flags_apply_to_overrides_existing() {
        let flags = ComponentSetFlags {
            local_path: Some("/override".to_string()),
            remote_path: None,
            build_artifact: None,
            extract_command: None,
            changelog_target: None,
        };

        let mut obj = serde_json::Map::new();
        obj.insert("local_path".to_string(), serde_json::json!("/original"));
        obj.insert("remote_path".to_string(), serde_json::json!("/keep-this"));

        flags.apply_to(&mut obj);

        assert_eq!(obj["local_path"], serde_json::json!("/override"));
        assert_eq!(obj["remote_path"], serde_json::json!("/keep-this"));
    }
}
