//! release_step_type — extracted from types.rs.

use super::super::*;
use crate::engine::pipeline::{self, PipelinePlanStep, PipelineRunResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Internal step types for the release pipeline.
/// These are used internally - the core flow is non-configurable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseStepType {
    Version,
    GitCommit,
    GitTag,
    GitPush,
    Package,
    Publish(String),
    Cleanup,
    PostRelease,
}
