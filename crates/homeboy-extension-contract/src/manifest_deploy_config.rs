use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeployArchiveInstallPolicy {
    pub path_pattern: String,
    #[serde(default = "default_staging_path")]
    pub staging_path: String,
    #[serde(default)]
    pub root_must_match_target_basename: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_header: Option<DeployRequiredHeader>,
    #[serde(default)]
    pub skip_permissions_fix: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "DeployRequiredHeaderConfig")]
pub struct DeployRequiredHeader {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_glob: Option<String>,
    pub contains: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeployRequiredHeaderConfig {
    pub file: Option<String>,
    pub file_glob: Option<String>,
    pub contains: String,
}

impl TryFrom<DeployRequiredHeaderConfig> for DeployRequiredHeader {
    type Error = String;

    fn try_from(config: DeployRequiredHeaderConfig) -> Result<Self, Self::Error> {
        if config.file.is_some() == config.file_glob.is_some() {
            return Err(
                "deploy.archive_install.required_header must declare exactly one of file or file_glob"
                    .to_string(),
            );
        }

        Ok(Self {
            file: config.file,
            file_glob: config.file_glob,
            contains: config.contains,
        })
    }
}

fn default_staging_path() -> String {
    "/tmp/homeboy-staging".to_string()
}
