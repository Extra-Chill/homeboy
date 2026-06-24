use super::*;

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
            || self.cwd.as_deref() == Some("git_checkout")
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
