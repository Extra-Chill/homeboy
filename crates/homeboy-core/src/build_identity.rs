use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildIdentity {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_dirty: Option<bool>,
    pub display: String,
}

pub fn current() -> BuildIdentity {
    identity_from_parts(
        env!("CARGO_PKG_VERSION"),
        option_env!("HOMEBOY_BUILD_GIT_COMMIT"),
        option_env!("HOMEBOY_BUILD_GIT_DIRTY"),
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_clean_git_identity() {
        let identity = identity_from_parts("0.228.13", Some("19a41cd5102d"), Some("false"));

        assert_eq!(identity.version, "0.228.13");
        assert_eq!(identity.git_commit.as_deref(), Some("19a41cd5102d"));
        assert_eq!(identity.git_dirty, Some(false));
        assert_eq!(identity.display, "homeboy 0.228.13+19a41cd5102d");
    }

    #[test]
    fn formats_dirty_git_identity() {
        let identity = identity_from_parts("0.228.13", Some("f7569a5e"), Some("true"));

        assert_eq!(identity.display, "homeboy 0.228.13+f7569a5e-dirty");
    }

    #[test]
    fn preserves_release_clean_identity_without_git_metadata() {
        let identity = identity_from_parts("0.228.13", None, None);

        assert_eq!(identity.git_commit, None);
        assert_eq!(identity.git_dirty, None);
        assert_eq!(identity.display, "homeboy 0.228.13");
    }
}
