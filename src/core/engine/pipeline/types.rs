//! types — extracted from pipeline.rs.

use std::collections::{HashMap, VecDeque};
use serde::{Deserialize, Serialize};
use crate::error::{Error, Result};
use std::sync::Arc;


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStep {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub config: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelinePlan {
    pub steps: Vec<PipelinePlanStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelinePlanStep {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub config: HashMap<String, serde_json::Value>,
    pub status: PipelineStepStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStepStatus {
    Ready,
    Missing,
    Disabled,
}

pub trait PipelineCapabilityResolver: Send + Sync {
    fn is_supported(&self, step_type: &str) -> bool;
    fn missing(&self, step_type: &str) -> Vec<String>;
}

pub trait PipelineStepExecutor: Send + Sync {
    fn execute_step(&self, step: &PipelineStep) -> Result<PipelineStepResult>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRunPlan {
    pub steps: Vec<PipelineStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineStepResult {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: String,
    pub status: PipelineRunStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<crate::error::Hint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRunResult {
    pub steps: Vec<PipelineStepResult>,
    pub status: PipelineRunStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<PipelineRunSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRunSummary {
    pub total_steps: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub missing: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub success_summary: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PipelineRunStatus {
    Success,
    PartialSuccess,
    Failed,
    Skipped,
    Missing,
}
