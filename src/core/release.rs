use serde::{Deserialize, Deserializer, Serialize, Serializer};

use std::collections::HashMap;

/// Type of release pipeline step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseStepType {
    Build,
    Changelog,
    Version,
    GitCommit,
    GitTag,
    GitPush,
    Changes,
    ModuleRun,
    ModuleAction(String),
}

impl ReleaseStepType {
    pub fn as_str(&self) -> &str {
        match self {
            ReleaseStepType::Build => "build",
            ReleaseStepType::Changelog => "changelog",
            ReleaseStepType::Version => "version",
            ReleaseStepType::GitCommit => "git.commit",
            ReleaseStepType::GitTag => "git.tag",
            ReleaseStepType::GitPush => "git.push",
            ReleaseStepType::Changes => "changes",
            ReleaseStepType::ModuleRun => "module.run",
            ReleaseStepType::ModuleAction(s) => s.as_str(),
        }
    }

    pub fn is_core_step(&self) -> bool {
        matches!(
            self,
            ReleaseStepType::Build
                | ReleaseStepType::Changelog
                | ReleaseStepType::Version
                | ReleaseStepType::GitCommit
                | ReleaseStepType::GitTag
                | ReleaseStepType::GitPush
                | ReleaseStepType::Changes
        )
    }
}

impl From<&str> for ReleaseStepType {
    fn from(s: &str) -> Self {
        match s {
            "build" => ReleaseStepType::Build,
            "changelog" => ReleaseStepType::Changelog,
            "version" => ReleaseStepType::Version,
            "git.commit" => ReleaseStepType::GitCommit,
            "git.tag" => ReleaseStepType::GitTag,
            "git.push" => ReleaseStepType::GitPush,
            "changes" => ReleaseStepType::Changes,
            "module.run" => ReleaseStepType::ModuleRun,
            other => ReleaseStepType::ModuleAction(other.to_string()),
        }
    }
}

impl From<String> for ReleaseStepType {
    fn from(s: String) -> Self {
        ReleaseStepType::from(s.as_str())
    }
}

impl Serialize for ReleaseStepType {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ReleaseStepType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(ReleaseStepType::from(s))
    }
}

use crate::component::{self, Component};
use crate::core::local_files::FileSystem;
use crate::error::{Error, Result};
use crate::module::{self, ModuleManifest};
use crate::pipeline::{
    self, PipelineCapabilityResolver, PipelinePlanStep, PipelineRunResult, PipelineRunStatus,
    PipelineStep, PipelineStepExecutor, PipelineStepResult,
};
use crate::utils::validation;
use crate::{changelog, version};

fn parse_module_inputs(values: &[serde_json::Value]) -> Result<Vec<(String, String)>> {
    let mut inputs = Vec::new();
    for value in values {
        let entry = validation::require(
            value.as_object(),
            "release.steps",
            "module.run inputs must be objects with 'id' and 'value'",
        )?;
        let id = validation::require(
            entry.get("id").and_then(|v| v.as_str()),
            "release.steps",
            "module.run inputs require 'id'",
        )?;
        let value = validation::require(
            entry.get("value").and_then(|v| v.as_str()),
            "release.steps",
            "module.run inputs require 'value'",
        )?;
        inputs.push((id.to_string(), value.to_string()));
    }

    Ok(inputs)
}

fn parse_module_args(values: &[serde_json::Value]) -> Result<Vec<String>> {
    let mut args = Vec::new();
    for value in values {
        let arg = validation::require(
            value.as_str(),
            "release.steps",
            "module.run args must be strings",
        )?;
        args.push(arg.to_string());
    }
    Ok(args)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

pub struct ReleaseConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<ReleaseStep>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub settings: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct ReleaseStep {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: ReleaseStepType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub config: HashMap<String, serde_json::Value>,
}

impl From<ReleaseStep> for PipelineStep {
    fn from(step: ReleaseStep) -> Self {
        PipelineStep {
            id: step.id,
            step_type: step.step_type.as_str().to_string(),
            label: step.label,
            needs: step.needs,
            config: step.config,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct ReleasePlan {
    pub component_id: String,
    pub enabled: bool,
    pub steps: Vec<ReleasePlanStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct ReleaseRun {
    pub component_id: String,
    pub enabled: bool,
    pub result: PipelineRunResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct ReleaseArtifact {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]

struct ReleaseContext {
    version: Option<String>,
    tag: Option<String>,
    notes: Option<String>,
    artifacts: Vec<ReleaseArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct ReleasePlanStep {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub config: HashMap<String, serde_json::Value>,
    pub status: ReleasePlanStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

impl From<PipelinePlanStep> for ReleasePlanStep {
    fn from(step: PipelinePlanStep) -> Self {
        let status = match step.status {
            pipeline::PipelineStepStatus::Ready => ReleasePlanStatus::Ready,
            pipeline::PipelineStepStatus::Missing => ReleasePlanStatus::Missing,
            pipeline::PipelineStepStatus::Disabled => ReleasePlanStatus::Disabled,
        };

        Self {
            id: step.id,
            step_type: step.step_type,
            label: step.label,
            needs: step.needs,
            config: step.config,
            status,
            missing: step.missing,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleasePlanStatus {
    Ready,
    Missing,
    Disabled,
}

struct ReleaseCapabilityResolver {
    modules: Vec<ModuleManifest>,
}

impl ReleaseCapabilityResolver {
    fn new(modules: Vec<ModuleManifest>) -> Self {
        Self { modules }
    }
}

impl PipelineCapabilityResolver for ReleaseCapabilityResolver {
    fn is_supported(&self, step_type: &str) -> bool {
        let st = ReleaseStepType::from(step_type);
        st == ReleaseStepType::ModuleRun
            || st.is_core_step()
            || self.supports_module_action(step_type)
    }

    fn missing(&self, step_type: &str) -> Vec<String> {
        if ReleaseStepType::from(step_type) == ReleaseStepType::ModuleRun {
            return Vec::new();
        }
        let action_id = format!("release.{}", step_type);
        vec![format!("Missing action '{}'", action_id)]
    }
}

impl ReleaseCapabilityResolver {
    fn supports_module_action(&self, step_type: &str) -> bool {
        let action_id = format!("release.{}", step_type);
        self.modules
            .iter()
            .any(|module| module.actions.iter().any(|action| action.id == action_id))
    }
}

struct ReleaseStepExecutor {
    component_id: String,
    modules: Vec<ModuleManifest>,
    context: std::sync::Mutex<ReleaseContext>,
}

impl ReleaseStepExecutor {
    fn new(component_id: String, modules: Vec<ModuleManifest>) -> Self {
        Self {
            component_id,
            modules,
            context: std::sync::Mutex::new(ReleaseContext::default()),
        }
    }

    fn step_result(
        &self,
        step: &PipelineStep,
        status: PipelineRunStatus,
        data: Option<serde_json::Value>,
        error: Option<String>,
        hints: Vec<crate::error::Hint>,
    ) -> PipelineStepResult {
        PipelineStepResult {
            id: step.id.clone(),
            step_type: step.step_type.clone(),
            status,
            missing: Vec::new(),
            warnings: Vec::new(),
            hints,
            data,
            error,
        }
    }

    fn execute_core_step(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let step_type = ReleaseStepType::from(step.step_type.as_str());
        match step_type {
            ReleaseStepType::Build => self.run_build(step),
            ReleaseStepType::Changes => self.run_changes(step),
            ReleaseStepType::Version => self.run_version(step),
            ReleaseStepType::GitCommit => self.run_git_commit(step),
            ReleaseStepType::GitTag => self.run_git_tag(step),
            ReleaseStepType::GitPush => self.run_git_push(step),
            _ => Err(Error::validation_invalid_argument(
                "release.steps",
                format!("Unsupported core step '{}'", step.step_type),
                None,
                None,
            )),
        }
    }

    fn run_build(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let (output, exit_code) = crate::build::run(&self.component_id)?;
        let data = serde_json::to_value(output)
            .map_err(|e| Error::internal_json(e.to_string(), Some("build output".to_string())))?;
        let status = if exit_code == 0 {
            PipelineRunStatus::Success
        } else {
            PipelineRunStatus::Failed
        };
        Ok(self.step_result(step, status, Some(data), None, Vec::new()))
    }

    fn run_changes(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let include_diff = step
            .config
            .get("includeDiff")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let output = crate::git::changes(Some(&self.component_id), None, include_diff)?;
        let data = serde_json::to_value(output)
            .map_err(|e| Error::internal_json(e.to_string(), Some("changes output".to_string())))?;
        Ok(self.step_result(
            step,
            PipelineRunStatus::Success,
            Some(data),
            None,
            Vec::new(),
        ))
    }

    fn run_version(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let mode = step
            .config
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("bump");

        match mode {
            "validate" => self.run_version_validate(step),
            _ => self.run_version_bump(step),
        }
    }

    fn run_version_bump(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let bump_type = step
            .config
            .get("bump")
            .and_then(|v| v.as_str())
            .unwrap_or("patch");
        let result = version::bump_version(Some(&self.component_id), bump_type)?;
        let data = serde_json::to_value(&result)
            .map_err(|e| Error::internal_json(e.to_string(), Some("version output".to_string())))?;
        self.store_version_context(&result.new_version)?;
        Ok(self.step_result(
            step,
            PipelineRunStatus::Success,
            Some(data),
            None,
            Vec::new(),
        ))
    }

    fn run_version_validate(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let info = version::read_version(Some(&self.component_id))?;
        let data = serde_json::to_value(&info)
            .map_err(|e| Error::internal_json(e.to_string(), Some("version output".to_string())))?;
        self.store_version_context(&info.version)?;
        Ok(self.step_result(
            step,
            PipelineRunStatus::Success,
            Some(data),
            None,
            Vec::new(),
        ))
    }

    fn run_git_tag(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let tag = step
            .config
            .get("name")
            .and_then(|v| v.as_str())
            .map(|v| v.to_string())
            .or_else(|| {
                step.config
                    .get("versionTag")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string())
            });

        let tag_name = match tag {
            Some(name) => name,
            None => self.default_tag()?,
        };

        let component = component::load(&self.component_id)?;

        // Check if tag already exists locally (idempotent behavior for version bump workflow)
        if crate::git::tag_exists_locally(&component.local_path, &tag_name).unwrap_or(false) {
            let tag_commit = crate::git::get_tag_commit(&component.local_path, &tag_name)?;
            let head_commit = crate::git::get_head_commit(&component.local_path)?;

            if tag_commit == head_commit {
                // Tag exists and points to HEAD - success (idempotent)
                self.store_tag_context(&tag_name)?;
                return Ok(self.step_result(
                    step,
                    PipelineRunStatus::Success,
                    Some(serde_json::json!({
                        "action": "tag",
                        "component_id": self.component_id,
                        "tag": tag_name,
                        "skipped": true,
                        "reason": "tag already exists and points to HEAD"
                    })),
                    None,
                    Vec::new(),
                ));
            }

            // Tag exists but points to different commit - auto-fix by deleting and recreating
            eprintln!(
                "[release] Auto-fixing: Tag '{}' points to {} but HEAD is {}. Recreating tag at HEAD...",
                tag_name,
                &tag_commit[..8.min(tag_commit.len())],
                &head_commit[..8.min(head_commit.len())]
            );

            // Delete the old tag
            let delete_output = crate::git::execute_git_for_release(
                &component.local_path,
                &["tag", "-d", &tag_name],
            )
            .map_err(|e| Error::other(e.to_string()))?;

            if !delete_output.status.success() {
                return Ok(self.step_result(
                    step,
                    PipelineRunStatus::Failed,
                    None,
                    Some(format!(
                        "Failed to delete orphaned tag '{}': {}",
                        tag_name,
                        String::from_utf8_lossy(&delete_output.stderr)
                    )),
                    Vec::new(),
                ));
            }

            eprintln!("[release] Tag '{}' will be recreated at HEAD", tag_name);

            // Fall through to create the tag (existing code below handles this)
        }

        // Tag doesn't exist - create it
        let message = step
            .config
            .get("message")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("Release {}", tag_name));

        let output = crate::git::tag(Some(&self.component_id), Some(&tag_name), Some(&message))?;
        let data = serde_json::to_value(&output)
            .map_err(|e| Error::internal_json(e.to_string(), Some("git tag output".to_string())))?;

        if !output.success {
            let mut hints = Vec::new();

            if output.stderr.contains("already exists") {
                let local_exists = crate::git::tag_exists_locally(&component.local_path, &tag_name)
                    .unwrap_or(false);
                let remote_exists =
                    crate::git::tag_exists_on_remote(&component.local_path, &tag_name)
                        .unwrap_or(false);

                if local_exists && !remote_exists {
                    hints.push(crate::error::Hint {
                        message: format!(
                            "Tag '{}' exists locally but not on remote. Push it with: git push origin {}",
                            tag_name, tag_name
                        ),
                    });
                } else if local_exists && remote_exists {
                    hints.push(crate::error::Hint {
                        message: format!(
                            "Tag '{}' already exists locally and on remote. Delete local tag first: git tag -d {}",
                            tag_name, tag_name
                        ),
                    });
                }
            }

            return Ok(self.step_result(
                step,
                PipelineRunStatus::Failed,
                Some(data),
                Some(output.stderr),
                hints,
            ));
        }

        self.store_tag_context(&tag_name)?;
        Ok(self.step_result(
            step,
            PipelineRunStatus::Success,
            Some(data),
            None,
            Vec::new(),
        ))
    }

    fn run_git_push(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let tags = step
            .config
            .get("tags")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let output = crate::git::push(Some(&self.component_id), tags)?;
        let data = serde_json::to_value(output).map_err(|e| {
            Error::internal_json(e.to_string(), Some("git push output".to_string()))
        })?;
        Ok(self.step_result(
            step,
            PipelineRunStatus::Success,
            Some(data),
            None,
            Vec::new(),
        ))
    }

    fn run_git_commit(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let status_output = crate::git::status(Some(&self.component_id))?;
        let is_clean = status_output.stdout.trim().is_empty();

        if is_clean {
            let data = serde_json::json!({
                "skipped": true,
                "reason": "working tree is clean, nothing to commit"
            });
            return Ok(self.step_result(
                step,
                PipelineRunStatus::Success,
                Some(data),
                None,
                Vec::new(),
            ));
        }

        let should_amend = self.should_amend_release_commit()?;

        let message = step
            .config
            .get("message")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.default_commit_message());

        let options = crate::git::CommitOptions {
            staged_only: false,
            files: None,
            exclude: None,
            amend: should_amend,
        };

        let output = crate::git::commit(Some(&self.component_id), Some(&message), options)?;
        let mut data = serde_json::to_value(&output).map_err(|e| {
            Error::internal_json(e.to_string(), Some("git commit output".to_string()))
        })?;

        if should_amend {
            data["amended"] = serde_json::json!(true);
        }

        let status = if output.success {
            PipelineRunStatus::Success
        } else {
            PipelineRunStatus::Failed
        };

        Ok(self.step_result(step, status, Some(data), None, Vec::new()))
    }

    fn default_commit_message(&self) -> String {
        let context = self.context.lock().ok();
        let version = context
            .as_ref()
            .and_then(|c| c.version.as_ref())
            .map(|v| v.as_str())
            .unwrap_or("unknown");
        format!("release: v{}", version)
    }

    fn should_amend_release_commit(&self) -> Result<bool> {
        let component = component::load(&self.component_id)?;

        let log_output = crate::git::execute_git_for_release(
            &component.local_path,
            &["log", "-1", "--format=%s"],
        )
        .map_err(|e| Error::other(e.to_string()))?;
        if !log_output.status.success() {
            return Ok(false);
        }
        let last_message = String::from_utf8_lossy(&log_output.stdout)
            .trim()
            .to_string();

        if !last_message.starts_with("release: v") {
            return Ok(false);
        }

        let status_output =
            crate::git::execute_git_for_release(&component.local_path, &["status", "-sb"])
                .map_err(|e| Error::other(e.to_string()))?;
        if !status_output.status.success() {
            return Ok(false);
        }
        let status_str = String::from_utf8_lossy(&status_output.stdout);
        let is_ahead = status_str.contains("[ahead");

        Ok(is_ahead)
    }

    fn build_release_payload(&self, step: &PipelineStep) -> Result<serde_json::Value> {
        let component = component::load(&self.component_id)?;
        let context = self.context.lock().map_err(|_| {
            Error::internal_unexpected("Failed to lock release context".to_string())
        })?;
        let version = context.version.clone().unwrap_or_default();
        let tag = context
            .tag
            .clone()
            .unwrap_or_else(|| format!("v{}", version));
        let notes = context.notes.clone().unwrap_or_default();
        let artifacts = context.artifacts.clone();

        let release_payload = serde_json::json!({
            "release": {
                "version": version,
                "tag": tag,
                "notes": notes,
                "component_id": self.component_id,
                "local_path": component.local_path,
                "artifacts": artifacts
            }
        });

        let mut payload = release_payload;
        if !step.config.is_empty() {
            let config_value = serde_json::to_value(&step.config).map_err(|e| {
                Error::internal_json(e.to_string(), Some("release step config".to_string()))
            })?;
            payload["config"] = config_value;
        }

        Ok(payload)
    }

    fn store_version_context(&self, version_value: &str) -> Result<()> {
        let mut context = self.context.lock().map_err(|_| {
            Error::internal_unexpected("Failed to lock release context".to_string())
        })?;
        context.version = Some(version_value.to_string());
        context.tag = Some(format!("v{}", version_value));
        context.notes = Some(self.load_release_notes()?);
        Ok(())
    }

    fn store_tag_context(&self, tag_value: &str) -> Result<()> {
        let mut context = self.context.lock().map_err(|_| {
            Error::internal_unexpected("Failed to lock release context".to_string())
        })?;
        context.tag = Some(tag_value.to_string());
        Ok(())
    }

    fn default_tag(&self) -> Result<String> {
        let context = self.context.lock().map_err(|_| {
            Error::internal_unexpected("Failed to lock release context".to_string())
        })?;
        if let Some(tag) = context.tag.as_ref() {
            return Ok(tag.clone());
        }
        if let Some(version) = context.version.as_ref() {
            return Ok(format!("v{}", version));
        }
        let info = version::read_version(Some(&self.component_id))?;
        Ok(format!("v{}", info.version))
    }

    fn load_release_notes(&self) -> Result<String> {
        let component = component::load(&self.component_id)?;
        let changelog_path = changelog::resolve_changelog_path(&component)?;
        let changelog_content = crate::core::local_files::local().read(&changelog_path)?;
        let notes = validation::require(
            extract_latest_notes(&changelog_content),
            "changelog",
            "No finalized changelog entries found for release notes",
        )?;
        Ok(notes)
    }

    fn update_artifacts_from_step(
        &self,
        step: &PipelineStep,
        response: &serde_json::Value,
    ) -> Result<()> {
        if !matches!(ReleaseStepType::from(step.step_type.as_str()), ReleaseStepType::ModuleAction(ref s) if s == "package") {
            return Ok(());
        }

        let artifacts_value = match response.get("artifacts") {
            Some(value) => Some(value.clone()),
            None => response
                .get("stdout")
                .and_then(|value| value.as_str())
                .and_then(|stdout| serde_json::from_str::<serde_json::Value>(stdout).ok()),
        };
        let Some(artifacts_value) = artifacts_value else {
            return Ok(());
        };

        let artifacts = parse_release_artifacts(&artifacts_value)?;
        if artifacts.is_empty() {
            return Ok(());
        }

        let mut context = self.context.lock().map_err(|_| {
            Error::internal_unexpected("Failed to lock release context".to_string())
        })?;
        context.artifacts = artifacts;
        Ok(())
    }

    fn run_module_action(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let action_id = format!("release.{}", step.step_type);
        let modules = resolve_module_actions(&self.modules, &action_id)?;
        let payload = self.build_release_payload(step)?;

        let mut results = Vec::new();
        for module in &modules {
            let response =
                module::execute_action(&module.id, &action_id, None, None, Some(&payload))?;
            let module_data = serde_json::to_value(&response).map_err(|e| {
                Error::internal_json(e.to_string(), Some("module action output".to_string()))
            })?;
            self.update_artifacts_from_step(step, &module_data)?;
            results.push(serde_json::json!({
                "module": module.id,
                "response": module_data
            }));
        }

        let data = serde_json::json!({
            "action": action_id,
            "results": results
        });

        Ok(self.step_result(
            step,
            PipelineRunStatus::Success,
            Some(data),
            None,
            Vec::new(),
        ))
    }

    fn run_module_runtime(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let module_id = validation::require(
            step.config.get("module").and_then(|v| v.as_str()),
            "release.steps",
            "module.run requires config.module",
        )?;

        let inputs = step
            .config
            .get("inputs")
            .and_then(|v| v.as_array())
            .map(|values| parse_module_inputs(values))
            .unwrap_or_else(|| Ok(Vec::new()))?;
        let args = step
            .config
            .get("args")
            .and_then(|v| v.as_array())
            .map(|values| parse_module_args(values))
            .unwrap_or_else(|| Ok(Vec::new()))?;

        let payload = self.build_release_payload(step)?;
        let working_dir = payload
            .get("release")
            .and_then(|r| r.get("local_path"))
            .and_then(|p| p.as_str());

        let outcome = module::run_module_runtime(
            module_id,
            None,
            None,
            inputs,
            args,
            Some(&payload),
            working_dir,
        )?;

        let data = serde_json::json!({
            "module": module_id,
            "stdout": outcome.result.stdout,
            "stderr": outcome.result.stderr,
            "exitCode": outcome.result.exit_code,
            "success": outcome.result.success,
            "payload": payload
        });

        let status = if outcome.result.success {
            PipelineRunStatus::Success
        } else {
            PipelineRunStatus::Failed
        };

        Ok(self.step_result(step, status, Some(data), None, Vec::new()))
    }
}

impl PipelineStepExecutor for ReleaseStepExecutor {
    fn execute_step(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let step_type = ReleaseStepType::from(step.step_type.as_str());

        if step_type.is_core_step() {
            return self.execute_core_step(step);
        }

        if step_type == ReleaseStepType::ModuleRun {
            return self.run_module_runtime(step);
        }

        self.run_module_action(step)
    }
}

fn resolve_modules(component: &Component, module_id: Option<&str>) -> Result<Vec<ModuleManifest>> {
    if module_id.is_some() {
        return Err(Error::validation_invalid_argument(
            "module",
            "Module selection is configured via component.modules; --module is not supported",
            None,
            None,
        ));
    }

    let mut modules = Vec::new();
    if let Some(configured) = component.modules.as_ref() {
        let mut module_ids: Vec<String> = configured.keys().cloned().collect();
        module_ids.sort();
        let suggestions = module::available_module_ids();
        for module_id in module_ids {
            let manifest = module::load_module(&module_id).map_err(|_| {
                Error::module_not_found(module_id.to_string(), suggestions.clone())
            })?;
            modules.push(manifest);
        }
    }

    Ok(modules)
}

fn resolve_module_actions(
    modules: &[ModuleManifest],
    action_id: &str,
) -> Result<Vec<ModuleManifest>> {
    let matches: Vec<ModuleManifest> = modules
        .iter()
        .filter(|module| module.actions.iter().any(|action| action.id == action_id))
        .cloned()
        .collect();

    if matches.is_empty() {
        return Err(Error::validation_invalid_argument(
            "release.steps",
            format!("No module provides action '{}'", action_id),
            None,
            None,
        ));
    }

    Ok(matches)
}

fn extract_latest_notes(content: &str) -> Option<String> {
    let mut in_section = false;
    let mut buffer = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            if in_section {
                break;
            }
            if extract_version_from_heading(trimmed).is_some() {
                in_section = true;
                continue;
            }
        }

        if in_section {
            buffer.push(line);
        }
    }

    let notes = buffer.join("\n").trim().to_string();
    if notes.is_empty() {
        None
    } else {
        Some(notes)
    }
}

fn extract_version_from_heading(label: &str) -> Option<String> {
    let semver_pattern = regex::Regex::new(r"\[?(\d+\.\d+\.\d+)\]?").ok()?;
    semver_pattern
        .captures(label)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

fn parse_release_artifacts(value: &serde_json::Value) -> Result<Vec<ReleaseArtifact>> {
    let mut artifacts = Vec::new();
    let items = match value {
        serde_json::Value::Array(arr) => arr.clone(),
        serde_json::Value::Object(_) => vec![value.clone()],
        _ => Vec::new(),
    };

    for item in items {
        let artifact = match item {
            serde_json::Value::String(path) => ReleaseArtifact {
                path,
                artifact_type: None,
                platform: None,
            },
            serde_json::Value::Object(map) => {
                let path = validation::require(
                    map.get("path").and_then(|v| v.as_str()),
                    "release.artifacts",
                    "Artifact is missing 'path'",
                )?
                .to_string();
                let artifact_type = map
                    .get("type")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                let platform = map
                    .get("platform")
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string());
                ReleaseArtifact {
                    path,
                    artifact_type,
                    platform,
                }
            }
            _ => {
                return Err(Error::validation_invalid_argument(
                    "release.artifacts",
                    "Artifact entry is invalid",
                    None,
                    None,
                ))
            }
        };
        artifacts.push(artifact);
    }

    Ok(artifacts)
}

pub fn resolve_component_release(component: &Component) -> Option<ReleaseConfig> {
    component.release.clone()
}

fn validate_plan_prerequisites(component: &Component) -> Vec<String> {
    use crate::core::local_files::FileSystem;
    let mut warnings = Vec::new();

    // Check changelog status
    match changelog::resolve_changelog_path(component) {
        Ok(changelog_path) => {
            if let Ok(content) = crate::core::local_files::local().read(&changelog_path) {
                let settings = changelog::resolve_effective_settings(Some(component));
                if let Ok(Some(status)) =
                    changelog::check_next_section_content(&content, &settings.next_section_aliases)
                {
                    match status.as_str() {
                        "empty" => {
                            warnings.push(
                                "No unreleased changelog entries. Run `homeboy changelog add` first."
                                    .to_string(),
                            );
                        }
                        "subsection_headers_only" => {
                            warnings.push(
                                "Changelog has subsection headers but no items. Add entries with `homeboy changelog add`."
                                    .to_string(),
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
        Err(_) => {
            warnings.push("No changelog configured for this component.".to_string());
        }
    }

    warnings
}

pub fn plan(component_id: &str, module_id: Option<&str>) -> Result<ReleasePlan> {
    let component = component::load(component_id)?;
    let modules = resolve_modules(&component, module_id)?;
    let resolver = ReleaseCapabilityResolver::new(modules.clone());
    let release = resolve_component_release(&component).ok_or_else(|| {
        Error::validation_invalid_argument(
            "release",
            "Release configuration is missing",
            Some(component_id.to_string()),
            None,
        )
        .with_hint(format!(
            "Use 'homeboy component set {} --json' to add a release block",
            component_id
        ))
        .with_hint("See 'homeboy docs commands/release' for examples")
    })?;

    let enabled = release.enabled.unwrap_or(true);

    let (release_steps, commit_auto_inserted) = auto_insert_commit_step(release.steps);
    let pipeline_steps: Vec<PipelineStep> = release_steps
        .iter()
        .cloned()
        .map(PipelineStep::from)
        .collect();
    let pipeline_plan = pipeline::plan(&pipeline_steps, &resolver, enabled, "release.steps")?;
    let steps: Vec<ReleasePlanStep> = pipeline_plan
        .steps
        .into_iter()
        .map(ReleasePlanStep::from)
        .collect();

    // Validate plan prerequisites and merge warnings
    let mut warnings = pipeline_plan.warnings;
    warnings.extend(validate_plan_prerequisites(&component));

    let mut hints = build_plan_hints(component_id, &steps, &modules);
    if commit_auto_inserted {
        hints.insert(
            0,
            "git.commit step auto-inserted before git.tag".to_string(),
        );
    }
    hints.push(format!(
        "Review changes first with: homeboy changes {}",
        component_id
    ));

    Ok(ReleasePlan {
        component_id: component_id.to_string(),
        enabled,
        steps,
        warnings,
        hints,
    })
}

pub fn run(component_id: &str, module_id: Option<&str>) -> Result<ReleaseRun> {
    let component = component::load(component_id)?;
    let modules = resolve_modules(&component, module_id)?;
    let resolver = ReleaseCapabilityResolver::new(modules.clone());
    let release = resolve_component_release(&component).ok_or_else(|| {
        Error::validation_invalid_argument(
            "release",
            "Release configuration is missing",
            Some(component_id.to_string()),
            None,
        )
        .with_hint(format!(
            "Use 'homeboy component set {} --json' to add a release block",
            component_id
        ))
        .with_hint("See 'homeboy docs commands/release' for examples")
    })?;

    let enabled = release.enabled.unwrap_or(true);

    let (release_steps, _commit_auto_inserted) = auto_insert_commit_step(release.steps);

    validate_preflight(&component, &release_steps)?;

    let executor = ReleaseStepExecutor::new(component_id.to_string(), modules.clone());

    let pipeline_steps: Vec<PipelineStep> =
        release_steps.into_iter().map(PipelineStep::from).collect();

    let run_result = pipeline::run(
        &pipeline_steps,
        std::sync::Arc::new(executor),
        std::sync::Arc::new(resolver),
        enabled,
        "release.steps",
    )?;

    Ok(ReleaseRun {
        component_id: component_id.to_string(),
        enabled,
        result: run_result,
    })
}

fn validate_preflight(component: &Component, steps: &[ReleaseStep]) -> Result<()> {
    let uncommitted = crate::git::get_uncommitted_changes(&component.local_path)?;
    let has_commit_step = steps.iter().any(|s| s.step_type == ReleaseStepType::GitCommit);

    if uncommitted.has_changes && !has_commit_step {
        return Err(Error::validation_invalid_argument(
            "working_tree",
            "Working tree has uncommitted changes",
            None,
            None,
        )
        .with_hint(
            "Commit your changes first with `git commit` or ensure a `git.commit` step \
             is in your release pipeline (auto-inserted when git.tag is present).",
        ));
    }

    // Validate changelog has no unreleased section with content
    if let Ok(changelog_path) = crate::changelog::resolve_changelog_path(component) {
        let changelog_content = crate::core::local_files::local().read(&changelog_path);
        if let Ok(content) = changelog_content {
            let settings = crate::changelog::resolve_effective_settings(Some(component));
            if let Some(status) = crate::changelog::check_next_section_content(
                &content,
                &settings.next_section_aliases,
            )? {
                match status.as_str() {
                    "empty" => {
                        // Empty unreleased section is fine - no content to release
                    }
                    _ => {
                        // Has unreleased content - should be finalized before release
                        return Err(Error::validation_invalid_argument(
                            "changelog",
                            "Changelog has unreleased section with content. Finalize changelog before releasing.",
                            None,
                            Some(vec![
                                "Run `homeboy version bump <component>` to finalize and increment version".to_string(),
                                "Or run `homeboy changelog add <component> -m \"...\"` to add more items".to_string(),
                            ]),
                        ));
                    }
                }
            }
        }
    }

    Ok(())
}

fn auto_insert_commit_step(steps: Vec<ReleaseStep>) -> (Vec<ReleaseStep>, bool) {
    let has_tag = steps.iter().any(|s| s.step_type == ReleaseStepType::GitTag);
    let has_commit = steps.iter().any(|s| s.step_type == ReleaseStepType::GitCommit);

    if !has_tag || has_commit {
        return (steps, false);
    }

    let mut result = Vec::with_capacity(steps.len() + 1);
    let mut inserted = false;

    for step in steps {
        if step.step_type == ReleaseStepType::GitTag && !inserted {
            let commit_step = ReleaseStep {
                id: "git.commit".to_string(),
                step_type: ReleaseStepType::GitCommit,
                label: Some("Commit release changes".to_string()),
                needs: step.needs.clone(),
                config: HashMap::new(),
            };
            result.push(commit_step);
            inserted = true;

            let mut tag_step = step;
            tag_step.needs = vec!["git.commit".to_string()];
            result.push(tag_step);
        } else {
            result.push(step);
        }
    }

    (result, inserted)
}

fn build_plan_hints(
    component_id: &str,
    steps: &[ReleasePlanStep],
    modules: &[ModuleManifest],
) -> Vec<String> {
    let mut hints = Vec::new();
    if steps.is_empty() {
        hints.push("Release plan has no steps".to_string());
    }

    if steps
        .iter()
        .any(|step| matches!(step.status, ReleasePlanStatus::Missing))
    {
        if modules.is_empty() {
            hints.push("Configure component modules to resolve release actions".to_string());
        } else {
            let module_names: Vec<String> =
                modules.iter().map(|module| module.id.clone()).collect();
            hints.push(format!(
                "Release actions are resolved from modules: {}",
                module_names.join(", ")
            ));
        }
    }

    if !hints.is_empty() {
        hints.push(format!(
            "Update release config with: homeboy component set {} --json",
            component_id
        ));
    }

    hints
}

// ============================================================================
// Unified Release Command (cargo-release pattern)
// ============================================================================

/// Options for the unified release command.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseOptions {
    pub bump_type: String,
    pub dry_run: bool,
    pub local: bool,
    pub publish: bool,
    pub no_tag: bool,
    pub no_push: bool,
}

/// Determine if the component has publish targets configured.
fn has_publish_targets(component: &Component) -> bool {
    if let Some(release) = &component.release {
        release.steps.iter().any(|step| {
            matches!(
                step.step_type,
                ReleaseStepType::GitPush | ReleaseStepType::ModuleAction(_) | ReleaseStepType::ModuleRun
            )
        })
    } else {
        false
    }
}

/// Plan a unified release (version bump + git operations + optional publish).
pub fn plan_unified(component_id: &str, options: &ReleaseOptions) -> Result<ReleasePlan> {
    let component = component::load(component_id)?;

    // Validate changelog has content to release
    let changelog_path = changelog::resolve_changelog_path(&component)?;
    let changelog_content = crate::core::local_files::local().read(&changelog_path)?;
    let settings = changelog::resolve_effective_settings(Some(&component));

    if let Some(status) =
        changelog::check_next_section_content(&changelog_content, &settings.next_section_aliases)?
    {
        match status.as_str() {
            "empty" => {
                return Err(Error::validation_invalid_argument(
                    "changelog",
                    "Changelog has no unreleased entries",
                    None,
                    Some(vec![
                        "Add changelog entries: homeboy changelog add <component> -m \"...\"".to_string(),
                    ]),
                ));
            }
            "subsection_headers_only" => {
                return Err(Error::validation_invalid_argument(
                    "changelog",
                    "Changelog has subsection headers but no items",
                    None,
                    Some(vec![
                        "Add changelog entries: homeboy changelog add <component> -m \"...\"".to_string(),
                    ]),
                ));
            }
            _ => {}
        }
    }

    // Preview version bump
    let version_info = version::read_version(Some(component_id))?;
    let new_version = version::increment_version(&version_info.version, &options.bump_type)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "version",
                format!("Invalid version format: {}", version_info.version),
                None,
                None,
            )
        })?;

    // Validate changelog for bump (dry-run validation)
    version::validate_changelog_for_bump(&component, &version_info.version, &new_version)?;

    // Determine effective behavior based on flags and config
    let has_publish = has_publish_targets(&component);
    let will_push = !options.local && !options.no_push && (options.publish || has_publish);
    let will_publish = !options.local && options.publish;

    // Build plan steps
    let mut steps = Vec::new();
    let warnings = Vec::new();
    let mut hints = Vec::new();

    // Step 1: Version bump (implicit, always first)
    steps.push(ReleasePlanStep {
        id: "version".to_string(),
        step_type: "version".to_string(),
        label: Some(format!(
            "Bump version {} → {} ({})",
            version_info.version, new_version, options.bump_type
        )),
        needs: vec![],
        config: {
            let mut config = std::collections::HashMap::new();
            config.insert(
                "bump".to_string(),
                serde_json::Value::String(options.bump_type.clone()),
            );
            config.insert(
                "from".to_string(),
                serde_json::Value::String(version_info.version.clone()),
            );
            config.insert(
                "to".to_string(),
                serde_json::Value::String(new_version.clone()),
            );
            config
        },
        status: ReleasePlanStatus::Ready,
        missing: vec![],
    });

    // Step 2: Git commit (always after version bump)
    steps.push(ReleasePlanStep {
        id: "git.commit".to_string(),
        step_type: "git.commit".to_string(),
        label: Some(format!("Commit release: v{}", new_version)),
        needs: vec!["version".to_string()],
        config: std::collections::HashMap::new(),
        status: ReleasePlanStatus::Ready,
        missing: vec![],
    });

    // Step 3: Git tag (unless --no-tag)
    if !options.no_tag {
        steps.push(ReleasePlanStep {
            id: "git.tag".to_string(),
            step_type: "git.tag".to_string(),
            label: Some(format!("Tag v{}", new_version)),
            needs: vec!["git.commit".to_string()],
            config: {
                let mut config = std::collections::HashMap::new();
                config.insert(
                    "name".to_string(),
                    serde_json::Value::String(format!("v{}", new_version)),
                );
                config
            },
            status: ReleasePlanStatus::Ready,
            missing: vec![],
        });
    }

    // Step 4: Git push (unless --local or --no-push)
    if will_push {
        let needs = if options.no_tag {
            vec!["git.commit".to_string()]
        } else {
            vec!["git.tag".to_string()]
        };
        steps.push(ReleasePlanStep {
            id: "git.push".to_string(),
            step_type: "git.push".to_string(),
            label: Some("Push to remote".to_string()),
            needs,
            config: {
                let mut config = std::collections::HashMap::new();
                config.insert("tags".to_string(), serde_json::Value::Bool(!options.no_tag));
                config
            },
            status: ReleasePlanStatus::Ready,
            missing: vec![],
        });
    }

    // Step 5+: Publish steps from component config (if --publish or has publish targets and not --local)
    if will_publish {
        if let Some(release) = &component.release {
            for step in &release.steps {
                if matches!(
                    step.step_type,
                    ReleaseStepType::ModuleAction(_) | ReleaseStepType::ModuleRun
                ) {
                    let needs = if will_push {
                        vec!["git.push".to_string()]
                    } else if !options.no_tag {
                        vec!["git.tag".to_string()]
                    } else {
                        vec!["git.commit".to_string()]
                    };
                    steps.push(ReleasePlanStep {
                        id: step.id.clone(),
                        step_type: step.step_type.as_str().to_string(),
                        label: step.label.clone(),
                        needs,
                        config: step.config.clone(),
                        status: ReleasePlanStatus::Ready,
                        missing: vec![],
                    });
                }
            }
        }
    }

    // Add hints based on configuration
    if options.local {
        hints.push("Local-only release: skipping push and publish steps".to_string());
    } else if options.no_push {
        hints.push("Skipping push (--no-push)".to_string());
    }

    if options.no_tag {
        hints.push("Skipping tag creation (--no-tag)".to_string());
    }

    if !will_publish && has_publish {
        hints.push(format!(
            "Component has publish targets. Use --publish to run: homeboy release {} {} --publish",
            component_id, options.bump_type
        ));
    }

    if options.dry_run {
        hints.push("Dry run: no changes will be made".to_string());
    }

    Ok(ReleasePlan {
        component_id: component_id.to_string(),
        enabled: true,
        steps,
        warnings,
        hints,
    })
}

/// Run a unified release (version bump + git operations + optional publish).
pub fn run_unified(component_id: &str, options: &ReleaseOptions) -> Result<ReleaseRun> {
    let component = component::load(component_id)?;

    // Execute pre-bump commands
    if !component.pre_version_bump_commands.is_empty() {
        version::run_pre_bump_commands(&component.pre_version_bump_commands, &component.local_path)?;
    }

    // Auto-stage changelog changes before clean-tree check
    if let Some(ref changelog_target) = component.changelog_target {
        let uncommitted = crate::git::get_uncommitted_changes(&component.local_path)?;
        if uncommitted.has_changes {
            let all_uncommitted: Vec<&str> = uncommitted
                .staged
                .iter()
                .chain(uncommitted.unstaged.iter())
                .map(|s| s.as_str())
                .collect();

            let only_changelog = !all_uncommitted.is_empty()
                && all_uncommitted
                    .iter()
                    .all(|f| *f == changelog_target || f.ends_with(changelog_target));

            if only_changelog {
                eprintln!(
                    "[release] Auto-staging changelog changes: {}",
                    changelog_target
                );
                crate::git::stage_files(&component.local_path, &[changelog_target.as_str()])?;
            }
        }
    }

    // Require clean working tree
    let uncommitted = crate::git::get_uncommitted_changes(&component.local_path)?;
    if uncommitted.has_changes {
        let mut details = vec![];
        if !uncommitted.staged.is_empty() {
            details.push(format!("Staged: {}", uncommitted.staged.join(", ")));
        }
        if !uncommitted.unstaged.is_empty() {
            details.push(format!("Unstaged: {}", uncommitted.unstaged.join(", ")));
        }
        if !uncommitted.untracked.is_empty() {
            details.push(format!("Untracked: {}", uncommitted.untracked.join(", ")));
        }
        return Err(Error::validation_invalid_argument(
            "workingTree",
            "Working tree has uncommitted changes",
            Some(details.join("\n")),
            Some(vec![
                "Release only commits version targets and changelog.".to_string(),
                "1. Document changes: homeboy changelog add <component> -m \"...\"".to_string(),
                "2. Commit everything: git add -A && git commit -m \"<description>\"".to_string(),
                "3. Run release: homeboy release <component> <level>".to_string(),
            ]),
        ));
    }

    // Step 1: Version bump with changelog finalization
    eprintln!("[release] Bumping version ({})...", options.bump_type);
    let bump_result = version::bump_version(Some(component_id), &options.bump_type)?;
    let new_version = bump_result.new_version.clone();

    // Collect files to commit
    let mut files_to_stage: Vec<String> = bump_result
        .targets
        .iter()
        .map(|t| t.full_path.clone())
        .collect();
    if !bump_result.changelog_path.is_empty() {
        files_to_stage.push(bump_result.changelog_path.clone());
    }

    // Step 2: Git commit
    eprintln!("[release] Committing release: v{}...", new_version);
    let commit_message = format!("release: v{}", new_version);
    let commit_options = crate::git::CommitOptions {
        staged_only: false,
        files: Some(files_to_stage),
        exclude: None,
        amend: false,
    };
    let commit_output = crate::git::commit(Some(component_id), Some(&commit_message), commit_options)?;
    if !commit_output.success {
        return Err(Error::other(format!(
            "Git commit failed: {}",
            commit_output.stderr
        )));
    }

    // Step 3: Git tag (unless --no-tag)
    if !options.no_tag {
        let tag_name = format!("v{}", new_version);
        let tag_message = format!("Release {}", tag_name);
        eprintln!("[release] Tagging {}...", tag_name);
        let tag_output = crate::git::tag(Some(component_id), Some(&tag_name), Some(&tag_message))?;
        if !tag_output.success {
            return Err(Error::other(format!(
                "Git tag failed: {}",
                tag_output.stderr
            )));
        }
    }

    // Determine effective behavior
    let has_publish = has_publish_targets(&component);
    let will_push = !options.local && !options.no_push && (options.publish || has_publish);
    let will_publish = !options.local && options.publish;

    // Step 4: Git push (unless --local or --no-push)
    if will_push {
        eprintln!("[release] Pushing to remote...");
        let push_output = crate::git::push(Some(component_id), !options.no_tag)?;
        if !push_output.success {
            return Err(Error::other(format!(
                "Git push failed: {}",
                push_output.stderr
            )));
        }
    }

    // Step 5+: Run publish steps from component config (if --publish)
    let mut publish_results = Vec::new();
    if will_publish {
        let modules = resolve_modules(&component, None)?;
        if let Some(release) = &component.release {
            for step in &release.steps {
                if matches!(
                    step.step_type,
                    ReleaseStepType::ModuleAction(_) | ReleaseStepType::ModuleRun
                ) {
                    eprintln!("[release] Running publish step: {}...", step.id);
                    let executor =
                        ReleaseStepExecutor::new(component_id.to_string(), modules.clone());

                    // Store version context for module execution
                    {
                        let mut context = executor.context.lock().map_err(|_| {
                            Error::internal_unexpected("Failed to lock release context".to_string())
                        })?;
                        context.version = Some(new_version.clone());
                        context.tag = Some(format!("v{}", new_version));
                    }

                    let pipeline_step = PipelineStep::from(step.clone());
                    let result = executor.execute_step(&pipeline_step)?;
                    publish_results.push(result);
                }
            }
        }
    }

    // Build result
    let mut run_steps = vec![
        PipelineStepResult {
            id: "version".to_string(),
            step_type: "version".to_string(),
            status: PipelineRunStatus::Success,
            missing: vec![],
            warnings: vec![],
            hints: vec![],
            data: Some(serde_json::json!({
                "old_version": bump_result.old_version,
                "new_version": bump_result.new_version,
                "changelog_finalized": bump_result.changelog_finalized,
            })),
            error: None,
        },
        PipelineStepResult {
            id: "git.commit".to_string(),
            step_type: "git.commit".to_string(),
            status: PipelineRunStatus::Success,
            missing: vec![],
            warnings: vec![],
            hints: vec![],
            data: Some(serde_json::json!({
                "message": commit_message,
            })),
            error: None,
        },
    ];

    if !options.no_tag {
        run_steps.push(PipelineStepResult {
            id: "git.tag".to_string(),
            step_type: "git.tag".to_string(),
            status: PipelineRunStatus::Success,
            missing: vec![],
            warnings: vec![],
            hints: vec![],
            data: Some(serde_json::json!({
                "tag": format!("v{}", new_version),
            })),
            error: None,
        });
    }

    if will_push {
        run_steps.push(PipelineStepResult {
            id: "git.push".to_string(),
            step_type: "git.push".to_string(),
            status: PipelineRunStatus::Success,
            missing: vec![],
            warnings: vec![],
            hints: vec![],
            data: None,
            error: None,
        });
    }

    run_steps.extend(publish_results);

    let overall_status = if run_steps.iter().all(|s| s.status == PipelineRunStatus::Success) {
        PipelineRunStatus::Success
    } else {
        PipelineRunStatus::Failed
    };

    // Log completion message
    if overall_status == PipelineRunStatus::Success {
        eprintln!("[release] Released v{}", new_version);
        if !will_push {
            eprintln!(
                "[release] Push with: git push origin v{} && git push",
                new_version
            );
        }
    }

    Ok(ReleaseRun {
        component_id: component_id.to_string(),
        enabled: true,
        result: PipelineRunResult {
            status: overall_status,
            steps: run_steps,
            warnings: Vec::new(),
            summary: None,
        },
    })
}
