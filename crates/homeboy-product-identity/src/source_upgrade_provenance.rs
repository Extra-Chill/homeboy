pub const GIT_COMMIT_ENV: &str = "HOMEBOY_SOURCE_UPGRADE_GIT_COMMIT";
pub const GIT_DIRTY_ENV: &str = "HOMEBOY_SOURCE_UPGRADE_GIT_DIRTY";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceUpgradeProvenance<'a> {
    pub git_commit: &'a str,
    pub git_dirty: bool,
}

pub fn parse_source_upgrade_provenance<'a>(
    git_commit: Option<&'a str>,
    git_dirty: Option<&str>,
) -> Result<Option<SourceUpgradeProvenance<'a>>, String> {
    match (git_commit, git_dirty) {
        (None, None) => Ok(None),
        (Some(commit), Some(dirty)) => {
            let commit = commit.trim();
            if commit.len() != 12
                || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err("HOMEBOY_SOURCE_UPGRADE_GIT_COMMIT must be a 12-character hexadecimal commit".to_string());
            }
            let git_dirty = match dirty.trim() {
                "true" => true,
                "false" => false,
                _ => return Err("HOMEBOY_SOURCE_UPGRADE_GIT_DIRTY must be `true` or `false`".to_string()),
            };
            Ok(Some(SourceUpgradeProvenance {
                git_commit: commit,
                git_dirty,
            }))
        }
        _ => Err("source-upgrade provenance requires both HOMEBOY_SOURCE_UPGRADE_GIT_COMMIT and HOMEBOY_SOURCE_UPGRADE_GIT_DIRTY".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_complete_provenance() {
        assert_eq!(
            parse_source_upgrade_provenance(Some("a30dc8a37b8d"), Some("true")).unwrap(),
            Some(SourceUpgradeProvenance {
                git_commit: "a30dc8a37b8d",
                git_dirty: true,
            })
        );
    }

    #[test]
    fn rejects_malformed_or_partial_provenance() {
        assert!(parse_source_upgrade_provenance(Some("synthetic"), Some("false")).is_err());
        assert!(parse_source_upgrade_provenance(Some("a30dc8a37b8d"), None).is_err());
        assert!(parse_source_upgrade_provenance(Some("a30dc8a37b8d"), Some("dirty")).is_err());
    }
}
