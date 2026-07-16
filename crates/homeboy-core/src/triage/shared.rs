//! Shared raw GitHub JSON deserialization types and small helpers used across the
//! triage report and landing concerns.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub(super) struct RawNamedNode {
    pub(super) name: Option<String>,
    pub(super) login: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawComment {
    #[serde(default, rename = "createdAt")]
    pub(super) created_at: Option<String>,
    #[serde(default, rename = "updatedAt")]
    pub(super) updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawReview {
    #[serde(default, rename = "submittedAt")]
    pub(super) submitted_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawPr {
    pub(super) number: u64,
    pub(super) title: String,
    pub(super) url: String,
    pub(super) state: String,
    #[serde(default, rename = "isDraft")]
    pub(super) is_draft: bool,
    #[serde(default, rename = "reviewDecision")]
    pub(super) review_decision: Option<String>,
    #[serde(default, rename = "mergeStateStatus")]
    pub(super) merge_state_status: Option<String>,
    #[serde(default, rename = "statusCheckRollup")]
    pub(super) status_check_rollup: Vec<Value>,
    #[serde(default, rename = "baseRefName")]
    pub(super) base_ref_name: Option<String>,
    #[serde(default, rename = "headRefName")]
    pub(super) head_ref_name: Option<String>,
    #[serde(default, rename = "headRepository")]
    pub(super) head_repository: Option<RawPrHeadRepository>,
    #[serde(default, rename = "headRepositoryOwner")]
    pub(super) head_repository_owner: Option<RawNamedNode>,
    #[serde(default, rename = "mergedAt")]
    pub(super) merged_at: Option<String>,
    #[serde(default)]
    pub(super) labels: Vec<RawNamedNode>,
    #[serde(default)]
    pub(super) assignees: Vec<RawNamedNode>,
    #[serde(default)]
    pub(super) author: Option<RawNamedNode>,
    #[serde(default)]
    pub(super) comments: Vec<RawComment>,
    #[serde(default)]
    pub(super) reviews: Vec<RawReview>,
    #[serde(default, rename = "updatedAt")]
    pub(super) updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RawPrHeadRepository {
    #[serde(default, rename = "nameWithOwner")]
    pub(super) name_with_owner: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
}

pub(super) fn latest_comment_at(comments: &[RawComment]) -> Option<String> {
    comments
        .iter()
        .filter_map(|comment| comment.updated_at.as_ref().or(comment.created_at.as_ref()))
        .max()
        .cloned()
}

pub(super) fn latest_review_at(reviews: &[RawReview]) -> Option<String> {
    reviews
        .iter()
        .filter_map(|review| review.submitted_at.as_ref())
        .max()
        .cloned()
}

pub(super) fn is_stale(updated_at: Option<&str>, stale_cutoff: Option<DateTime<Utc>>) -> bool {
    let Some(cutoff) = stale_cutoff else {
        return false;
    };
    let Some(updated_at) = updated_at else {
        return false;
    };
    DateTime::parse_from_rfc3339(updated_at)
        .map(|dt| dt.with_timezone(&Utc) < cutoff)
        .unwrap_or(false)
}

pub(super) fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    })
}

pub(super) fn bool_field(value: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_bool))
}

pub(super) fn pluralize(count: usize, singular: &str, plural: &str) -> String {
    format!("{} {}", count, if count == 1 { singular } else { plural })
}
