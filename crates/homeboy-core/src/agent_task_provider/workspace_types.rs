use super::*;
use std::fmt;
use std::str::FromStr;

pub const WORKSPACE_CWD_MODE_GIT_CHECKOUT: &str = "git_checkout";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceCwdMode {
    GitCheckout,
}

impl WorkspaceCwdMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GitCheckout => WORKSPACE_CWD_MODE_GIT_CHECKOUT,
        }
    }
}

impl fmt::Display for WorkspaceCwdMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for WorkspaceCwdMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            WORKSPACE_CWD_MODE_GIT_CHECKOUT => Ok(Self::GitCheckout),
            _ => Err(()),
        }
    }
}

pub const WORKSPACE_WRITE_SCOPE_PATCH: &str = "patch";
pub const WORKSPACE_WRITE_SCOPE_WORKSPACE: &str = "workspace";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceWriteScope {
    Patch,
    Workspace,
}

impl WorkspaceWriteScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Patch => WORKSPACE_WRITE_SCOPE_PATCH,
            Self::Workspace => WORKSPACE_WRITE_SCOPE_WORKSPACE,
        }
    }
}

impl fmt::Display for WorkspaceWriteScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for WorkspaceWriteScope {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            WORKSPACE_WRITE_SCOPE_PATCH => Ok(Self::Patch),
            WORKSPACE_WRITE_SCOPE_WORKSPACE => Ok(Self::Workspace),
            _ => Err(()),
        }
    }
}

pub const WORKSPACE_MATERIALIZATION_MODE_GIT: &str = "git";
pub const WORKSPACE_MATERIALIZATION_MODE_SNAPSHOT: &str = "snapshot";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceMaterializationMode {
    Git,
    Snapshot,
}

impl WorkspaceMaterializationMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Git => WORKSPACE_MATERIALIZATION_MODE_GIT,
            Self::Snapshot => WORKSPACE_MATERIALIZATION_MODE_SNAPSHOT,
        }
    }
}

impl fmt::Display for WorkspaceMaterializationMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for WorkspaceMaterializationMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            WORKSPACE_MATERIALIZATION_MODE_GIT => Ok(Self::Git),
            WORKSPACE_MATERIALIZATION_MODE_SNAPSHOT => Ok(Self::Snapshot),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AgentTaskProviderWorkspaceMaterialization {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_git: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<WorkspaceMaterializationSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<WorkspaceMountSpec>,
    #[serde(default, skip_serializing_if = "AgentTaskRuntimeApplyBack::is_empty")]
    pub apply_back: AgentTaskRuntimeApplyBack,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl AgentTaskProviderWorkspaceMaterialization {
    pub fn requires_cwd_git_checkout(&self) -> bool {
        self.apply_back.requires_git_checkout == Some(true)
            || self.requires_git == Some(true)
            || self.cwd_mode() == Some(WorkspaceCwdMode::GitCheckout)
    }

    pub fn cwd_mode(&self) -> Option<WorkspaceCwdMode> {
        parse_optional_contract_value(self.cwd.as_deref())
    }

    pub fn write_scope(&self) -> Option<WorkspaceWriteScope> {
        parse_optional_contract_value(self.write_scope.as_deref())
    }

    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        if let Some(spec) = &self.spec {
            errors.extend(
                spec.validation_errors()
                    .into_iter()
                    .map(|error| format!("spec.{error}")),
            );
        }
        for (index, mount) in self.mounts.iter().enumerate() {
            errors.extend(
                mount
                    .validation_errors()
                    .into_iter()
                    .map(|error| format!("mounts[{index}].{error}")),
            );
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkspaceMaterializationSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<WorkspaceMountSpec>,
    #[serde(
        default = "default_metadata",
        skip_serializing_if = "is_empty_metadata"
    )]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl WorkspaceMaterializationSpec {
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let errors = self.validation_errors();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn validation_errors(&self) -> Vec<String> {
        let mut errors = workspace_mount_like_validation_errors(
            self.handle.as_deref(),
            self.repo.as_deref(),
            self.host_path.as_deref(),
            self.target_path.as_deref(),
            self.mode.as_deref(),
            self.materialization.as_deref(),
        );
        for (index, mount) in self.mounts.iter().enumerate() {
            errors.extend(
                mount
                    .validation_errors()
                    .into_iter()
                    .map(|error| format!("mounts[{index}].{error}")),
            );
        }
        errors
    }

    pub fn mode(&self) -> Option<WorkspaceMaterializationMode> {
        parse_optional_contract_value(self.mode.as_deref())
    }

    pub fn materialization(&self) -> Option<WorkspaceMaterializationMode> {
        parse_optional_contract_value(self.materialization.as_deref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkspaceMountSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialization: Option<String>,
    #[serde(
        default = "default_metadata",
        skip_serializing_if = "is_empty_metadata"
    )]
    pub metadata: Value,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

impl WorkspaceMountSpec {
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let errors = self.validation_errors();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn validation_errors(&self) -> Vec<String> {
        workspace_mount_like_validation_errors(
            self.handle.as_deref(),
            self.repo.as_deref(),
            self.host_path.as_deref(),
            self.target_path.as_deref(),
            self.mode.as_deref(),
            self.materialization.as_deref(),
        )
    }

    pub fn mode(&self) -> Option<WorkspaceMaterializationMode> {
        parse_optional_contract_value(self.mode.as_deref())
    }

    pub fn materialization(&self) -> Option<WorkspaceMaterializationMode> {
        parse_optional_contract_value(self.materialization.as_deref())
    }
}

fn parse_optional_contract_value<T: FromStr>(value: Option<&str>) -> Option<T> {
    value.and_then(|value| T::from_str(value).ok())
}

fn workspace_mount_like_validation_errors(
    handle: Option<&str>,
    repo: Option<&str>,
    host_path: Option<&str>,
    target_path: Option<&str>,
    mode: Option<&str>,
    materialization: Option<&str>,
) -> Vec<String> {
    let mut errors = Vec::new();
    validate_non_blank_optional("handle", handle, &mut errors);
    validate_non_blank_optional("repo", repo, &mut errors);
    validate_non_blank_optional("host_path", host_path, &mut errors);
    validate_non_blank_optional("target_path", target_path, &mut errors);
    validate_non_blank_optional("mode", mode, &mut errors);
    validate_non_blank_optional("materialization", materialization, &mut errors);
    if host_path.is_some() && target_path.is_none() {
        errors.push("target_path is required when host_path is set".to_string());
    }
    errors
}

fn validate_non_blank_optional(field: &str, value: Option<&str>, errors: &mut Vec<String>) {
    if value.is_some_and(|value| value.trim().is_empty()) {
        errors.push(format!("{field} must not be blank"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_materialization_uses_typed_cwd_contract_for_git_checkout() {
        let materialization = AgentTaskProviderWorkspaceMaterialization {
            cwd: Some(WorkspaceCwdMode::GitCheckout.to_string()),
            ..AgentTaskProviderWorkspaceMaterialization::default()
        };

        assert_eq!(
            materialization.cwd_mode(),
            Some(WorkspaceCwdMode::GitCheckout)
        );
        assert!(materialization.requires_cwd_git_checkout());
    }

    #[test]
    fn workspace_materialization_keeps_unknown_strings_non_breaking() {
        let materialization = AgentTaskProviderWorkspaceMaterialization {
            cwd: Some("provider_owned_workspace".to_string()),
            write_scope: Some("provider_owned_scope".to_string()),
            ..AgentTaskProviderWorkspaceMaterialization::default()
        };

        assert_eq!(materialization.cwd_mode(), None);
        assert_eq!(materialization.write_scope(), None);
        assert!(!materialization.requires_cwd_git_checkout());
        assert!(materialization.validate().is_ok());
    }

    #[test]
    fn workspace_mount_modes_parse_known_contract_values() {
        let spec = WorkspaceMaterializationSpec {
            mode: Some(WorkspaceMaterializationMode::Git.to_string()),
            materialization: Some(WorkspaceMaterializationMode::Snapshot.to_string()),
            ..WorkspaceMaterializationSpec::default()
        };
        let mount = WorkspaceMountSpec {
            mode: Some(WorkspaceMaterializationMode::Snapshot.to_string()),
            materialization: Some(WorkspaceMaterializationMode::Git.to_string()),
            ..WorkspaceMountSpec::default()
        };

        assert_eq!(spec.mode(), Some(WorkspaceMaterializationMode::Git));
        assert_eq!(
            spec.materialization(),
            Some(WorkspaceMaterializationMode::Snapshot)
        );
        assert_eq!(mount.mode(), Some(WorkspaceMaterializationMode::Snapshot));
        assert_eq!(
            mount.materialization(),
            Some(WorkspaceMaterializationMode::Git)
        );
    }
}
