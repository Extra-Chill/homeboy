use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildIdentity {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_dirty: Option<bool>,
    pub display: String,
}

pub fn build_identity() -> BuildIdentity {
    identity_from_parts(
        env!("HOMEBOY_PRODUCT_VERSION"),
        option_env!("HOMEBOY_PRODUCT_GIT_COMMIT"),
        option_env!("HOMEBOY_PRODUCT_GIT_DIRTY"),
    )
}

pub const fn product_version() -> &'static str {
    env!("HOMEBOY_PRODUCT_VERSION")
}

fn identity_from_parts(version: &str, commit: Option<&str>, dirty: Option<&str>) -> BuildIdentity {
    let git_commit = commit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let git_dirty = dirty.and_then(|value| match value.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    });
    let display = match git_commit.as_deref() {
        Some(commit) if git_dirty == Some(true) => format!("homeboy {version}+{commit}-dirty"),
        Some(commit) => format!("homeboy {version}+{commit}"),
        None => format!("homeboy {version}"),
    };

    BuildIdentity {
        version: version.to_string(),
        git_commit,
        git_dirty,
        display,
    }
}

/// Homeboy-owned product literals that core code needs for config, paths, and
/// backward-compatible environment contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductIdentity {
    pub name: &'static str,
    pub binary_name: &'static str,
    pub config_filename: &'static str,
    pub config_dirname: &'static str,
    pub data_dirname: &'static str,
    pub env_prefix: &'static str,
    pub artifact_prefix: &'static str,
    pub run_dir_prefix: &'static str,
}

pub const PRODUCT_IDENTITY: ProductIdentity = ProductIdentity {
    name: "Homeboy",
    binary_name: "homeboy",
    config_filename: "homeboy.json",
    config_dirname: "homeboy",
    data_dirname: "homeboy",
    env_prefix: "HOMEBOY_",
    artifact_prefix: ".homeboy-",
    run_dir_prefix: "homeboy-run",
};

impl ProductIdentity {
    pub fn env_var(self, suffix: &str) -> String {
        format!("{}{}", self.env_prefix, suffix)
    }

    pub fn config_file(self, base: PathBuf) -> PathBuf {
        base.join(self.config_filename)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_source_and_archive_builds() {
        assert_eq!(
            identity_from_parts("0.286.8", Some("19a41cd5102d"), Some("true")).display,
            "homeboy 0.286.8+19a41cd5102d-dirty"
        );
        assert_eq!(
            identity_from_parts("0.286.8", None, None).display,
            "homeboy 0.286.8"
        );
    }
}
