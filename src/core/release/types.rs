use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;

use crate::pipeline::{self, PipelinePlanStep, PipelineRunResult, PipelineStep};

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
pub(crate) struct ReleaseContext {
    pub version: Option<String>,
    pub tag: Option<String>,
    pub notes: Option<String>,
    pub artifacts: Vec<ReleaseArtifact>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReleaseOptions {
    pub bump_type: String,
    pub dry_run: bool,
    pub no_tag: bool,
    pub no_push: bool,
    pub no_commit: bool,
    pub commit_message: Option<String>,
}
