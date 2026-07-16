use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TriagePullRequestSignals {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviews_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_comment_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_review_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NewTriageItemRecord {
    pub run_id: String,
    pub provider: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub item_type: String,
    pub number: u64,
    pub state: String,
    pub title: String,
    pub url: String,
    #[serde(flatten)]
    pub signals: TriagePullRequestSignals,
    pub updated_at: Option<String>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TriageItemRecord {
    pub id: String,
    pub run_id: String,
    pub provider: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub item_type: String,
    pub number: u64,
    pub state: String,
    pub title: String,
    pub url: String,
    #[serde(flatten)]
    pub signals: TriagePullRequestSignals,
    pub updated_at: Option<String>,
    pub metadata_json: serde_json::Value,
    pub observed_at: String,
}
