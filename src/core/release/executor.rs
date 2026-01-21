use crate::component;
use crate::core::local_files::FileSystem;
use crate::error::{Error, Result};
use crate::module::{self, ModuleManifest};
use crate::engine::pipeline::{PipelineRunStatus, PipelineStep, PipelineStepExecutor, PipelineStepResult};
use crate::utils::validation;
use crate::{changelog, version};

use super::types::{ReleaseContext, ReleaseStepType};
use super::utils::extract_latest_notes;

pub(crate) struct ReleaseStepExecutor {
    component_id: String,
    modules: Vec<ModuleManifest>,
    pub(crate) context: std::sync::Mutex<ReleaseContext>,
}

impl ReleaseStepExecutor {
    pub fn new(component_id: String, modules: Vec<ModuleManifest>) -> Self {
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
        let step_type = ReleaseStepType::from_str(&step.step_type);
        match step_type {
            ReleaseStepType::Version => self.run_version(step),
            ReleaseStepType::GitCommit => self.run_git_commit(step),
            ReleaseStepType::GitTag => self.run_git_tag(step),
            ReleaseStepType::GitPush => self.run_git_push(step),
            ReleaseStepType::Package => self.run_package(step),
            ReleaseStepType::Publish(target) => self.run_publish(step, &target),
            ReleaseStepType::Cleanup => self.run_cleanup(step),
        }
    }

    fn run_version(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
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

    fn run_git_tag(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let tag_name = self.get_release_tag(step)?;
        let component = component::load(&self.component_id)?;

        if crate::git::tag_exists_locally(&component.local_path, &tag_name).unwrap_or(false) {
            let tag_commit = crate::git::get_tag_commit(&component.local_path, &tag_name)?;
            let head_commit = crate::git::get_head_commit(&component.local_path)?;

            if tag_commit == head_commit {
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

            return Err(Error::validation_invalid_argument(
                "tag",
                format!(
                    "Tag '{}' exists but points to different commit",
                    tag_name
                ),
                Some(format!(
                    "Tag points to {}, HEAD is {}",
                    &tag_commit[..8.min(tag_commit.len())],
                    &head_commit[..8.min(head_commit.len())]
                )),
                Some(vec![
                    format!("Delete stale tag: git tag -d {}", tag_name),
                    format!("Then retry: homeboy release {} <bump>", self.component_id),
                ]),
            ));
        }

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

    fn run_package(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let module = self
            .modules
            .iter()
            .find(|m| m.actions.iter().any(|a| a.id == "release.package"))
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "release.package",
                    "No module provides release.package action",
                    None,
                    Some(vec![
                        "Add a module with release.package action to the component".to_string(),
                        "For Rust projects, add: \"modules\": { \"rust\": {} }".to_string(),
                    ]),
                )
            })?;

        let payload = self.build_release_payload(step)?;
        let response = module::execute_action(&module.id, "release.package", None, None, Some(&payload))?;

        self.store_artifacts_from_output(&response)?;

        let data = serde_json::json!({
            "module": module.id,
            "action": "release.package",
            "response": response
        });

        Ok(self.step_result(
            step,
            PipelineRunStatus::Success,
            Some(data),
            None,
            Vec::new(),
        ))
    }

    fn store_artifacts_from_output(&self, response: &serde_json::Value) -> Result<()> {
        let stdout = response
            .get("stdout")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let artifacts: Vec<super::types::ReleaseArtifact> =
            serde_json::from_str(stdout).map_err(|e| {
                Error::internal_json(
                    e.to_string(),
                    Some(format!("Failed to parse package artifacts: {}", stdout)),
                )
            })?;

        let mut context = self.context.lock().map_err(|_| {
            Error::internal_unexpected("Failed to lock release context".to_string())
        })?;

        context.artifacts.extend(artifacts);
        Ok(())
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

    /// Execute a publish step by calling the target module's release.publish action.
    fn run_publish(&self, step: &PipelineStep, target: &str) -> Result<PipelineStepResult> {
        let module = self
            .modules
            .iter()
            .find(|m| m.id == target)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "release.publish",
                    format!("No module '{}' found for publish target", target),
                    None,
                    Some(vec![format!(
                        "Add module to component config: \"modules\": {{ \"{}\": {{}} }}",
                        target
                    )]),
                )
            })?;

        let action_id = "release.publish";
        let has_action = module.actions.iter().any(|a| a.id == action_id);
        if !has_action {
            return Err(Error::validation_invalid_argument(
                "release.publish",
                format!("Module '{}' does not provide action '{}'", target, action_id),
                None,
                None,
            ));
        }

        let payload = self.build_release_payload(step)?;
        let response = module::execute_action(&module.id, action_id, None, None, Some(&payload))?;
        let module_data = serde_json::to_value(&response).map_err(|e| {
            Error::internal_json(e.to_string(), Some("module action output".to_string()))
        })?;

        let data = serde_json::json!({
            "target": target,
            "module": module.id,
            "action": action_id,
            "response": module_data
        });

        Ok(self.step_result(
            step,
            PipelineRunStatus::Success,
            Some(data),
            None,
            Vec::new(),
        ))
    }

    fn run_cleanup(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        let component = component::load(&self.component_id)?;
        let distrib_path = format!("{}/target/distrib", component.local_path);

        let mut removed = false;
        if std::path::Path::new(&distrib_path).exists() {
            std::fs::remove_dir_all(&distrib_path).map_err(|e| {
                Error::other(format!("Failed to clean up {}: {}", distrib_path, e))
            })?;
            removed = true;
        }

        let data = serde_json::json!({
            "action": "cleanup",
            "path": distrib_path,
            "removed": removed
        });

        Ok(self.step_result(
            step,
            PipelineRunStatus::Success,
            Some(data),
            None,
            Vec::new(),
        ))
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

    pub(crate) fn build_release_payload(&self, step: &PipelineStep) -> Result<serde_json::Value> {
        let component = component::load(&self.component_id)?;
        let context = self.context.lock().map_err(|_| {
            Error::internal_unexpected("Failed to lock release context".to_string())
        })?;

        let version = context.version.clone().ok_or_else(|| {
            Error::validation_invalid_argument(
                "version",
                "Version context not set for release step",
                Some(format!("Step '{}' requires version context", step.id)),
                Some(vec![
                    "Ensure version step runs before this step".to_string(),
                ]),
            )
        })?;

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

    fn get_release_tag(&self, step: &PipelineStep) -> Result<String> {
        if let Some(name) = step.config.get("name").and_then(|v| v.as_str()) {
            return Ok(name.to_string());
        }
        if let Some(name) = step.config.get("versionTag").and_then(|v| v.as_str()) {
            return Ok(name.to_string());
        }

        let context = self.context.lock().map_err(|_| {
            Error::internal_unexpected("Failed to lock release context".to_string())
        })?;

        if let Some(tag) = context.tag.as_ref() {
            return Ok(tag.clone());
        }
        if let Some(version) = context.version.as_ref() {
            return Ok(format!("v{}", version));
        }

        Err(Error::validation_invalid_argument(
            "tag",
            "Cannot determine release tag - version context not set",
            None,
            Some(vec![
                "Ensure version step runs before git.tag step".to_string(),
                "Or specify tag explicitly in step config: { \"name\": \"v1.2.3\" }".to_string(),
            ]),
        ))
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
}

impl PipelineStepExecutor for ReleaseStepExecutor {
    fn execute_step(&self, step: &PipelineStep) -> Result<PipelineStepResult> {
        self.execute_core_step(step)
    }
}
