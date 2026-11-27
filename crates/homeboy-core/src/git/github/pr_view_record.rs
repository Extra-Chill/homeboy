//! The one `gh pr view` fetch contract: a single `--json` field superset, one
//! raw record, and one parser.
//!
//! `github/pulls.rs`, `git/pr_land.rs`, and `git/pr_refresh.rs` share
//! [`PR_VIEW_JSON_FIELDS`], [`pr_view_args`], and [`parse_pr_view`]. This
//! module is deliberately a faithful wire projection only: consumers keep
//! their own projection semantics on top of [`PrViewRecord`] (the pr-land
//! auto-merge gate keeps its rollup summarizer, the view/readiness summary
//! keeps its CI classifier), and this module takes no side in those choices.

use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};

/// The union of every field the `gh pr view` consumers request.
pub const PR_VIEW_JSON_FIELDS: &str = "author,body,baseRefName,headRefName,headRepository,headRefOid,isDraft,mergedAt,mergeStateStatus,number,reviewDecision,state,statusCheckRollup,title,url";

/// Build the `gh` argument list for one `pr view` invocation.
///
/// `repo` is `Some` for component-scoped callers (`-R owner/repo`); callers
/// that may target a foreign-repo PR by URL pass `None` and let `gh` resolve
/// the repository from the ref and the current directory.
pub fn pr_view_args(pr_ref: &str, repo: Option<&str>) -> Vec<String> {
    let mut args = vec!["pr".to_string(), "view".to_string(), pr_ref.to_string()];
    if let Some(repo) = repo {
        args.push("-R".to_string());
        args.push(repo.to_string());
    }
    args.push("--json".to_string());
    args.push(PR_VIEW_JSON_FIELDS.to_string());
    args
}

/// Raw `gh pr view --json PR_VIEW_JSON_FIELDS` payload.
///
/// Every field defaults, so a minimal payload still parses. `headRepository` keeps both
/// `nameWithOwner` and `name` because consumers disagree on the fallback, and
/// `statusCheckRollup` stays untyped because each consumer classifies it
/// differently.
#[derive(Debug, Clone, Deserialize)]
pub struct PrViewRecord {
    #[serde(default, deserialize_with = "null_as_default")]
    pub number: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    pub state: String,
    #[serde(default, deserialize_with = "null_as_default", rename = "isDraft")]
    pub is_draft: bool,
    #[serde(default)]
    pub author: Option<PrViewAuthor>,
    #[serde(default, deserialize_with = "null_as_default", rename = "baseRefName")]
    pub base_ref_name: String,
    #[serde(default, deserialize_with = "null_as_default", rename = "headRefName")]
    pub head_ref_name: String,
    #[serde(default, rename = "headRepository")]
    pub head_repository: Option<PrViewHeadRepository>,
    #[serde(default, rename = "headRefOid")]
    pub head_ref_oid: Option<String>,
    #[serde(default, rename = "mergedAt")]
    pub merged_at: Option<String>,
    #[serde(default, rename = "reviewDecision")]
    pub review_decision: Option<String>,
    #[serde(default, rename = "mergeStateStatus")]
    pub merge_state_status: Option<String>,
    #[serde(
        default,
        deserialize_with = "null_as_default",
        rename = "statusCheckRollup"
    )]
    pub status_check_rollup: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrViewAuthor {
    #[serde(default)]
    pub login: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrViewHeadRepository {
    #[serde(default, rename = "nameWithOwner")]
    pub name_with_owner: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// Treat an explicit JSON `null` like an absent field.
///
/// `#[serde(default)]` alone only covers a missing key; `gh` can also emit
/// explicit `null`s, which the non-`Option` fields read as empty.
fn null_as_default<'de, D, T>(deserializer: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Parse `gh pr view` JSON — the only place that payload is decoded.
pub fn parse_pr_view(raw: &str) -> Result<PrViewRecord> {
    serde_json::from_str(raw.trim()).map_err(|error| {
        Error::internal_json(
            format!("failed to parse gh pr view JSON: {error}"),
            Some(raw.trim().to_string()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_full_superset_payload() {
        let raw = r#"{
            "author": {"login": "homeboy-ci[bot]"},
            "baseRefName": "main",
            "headRefName": "ci/autofix/homeboy/main",
            "headRepository": {"nameWithOwner": "Extra-Chill/homeboy", "name": "homeboy"},
            "headRefOid": "abc123def456",
            "isDraft": true,
            "mergedAt": "2026-01-02T03:04:05Z",
            "mergeStateStatus": "CLEAN",
            "number": 42,
            "reviewDecision": "APPROVED",
            "state": "OPEN",
            "statusCheckRollup": [
                {"name": "CI", "status": "COMPLETED", "conclusion": "SUCCESS"}
            ],
            "title": "feat: shared pr view record",
            "url": "https://github.com/Extra-Chill/homeboy/pull/42"
        }"#;
        let record = parse_pr_view(raw).unwrap();
        assert_eq!(record.number, 42);
        assert_eq!(record.url, "https://github.com/Extra-Chill/homeboy/pull/42");
        assert_eq!(record.title.as_deref(), Some("feat: shared pr view record"));
        assert_eq!(record.state, "OPEN");
        assert!(record.is_draft);
        assert_eq!(
            record.author.as_ref().and_then(|a| a.login.as_deref()),
            Some("homeboy-ci[bot]")
        );
        assert_eq!(record.base_ref_name, "main");
        assert_eq!(record.head_ref_name, "ci/autofix/homeboy/main");
        let head_repository = record.head_repository.as_ref().unwrap();
        assert_eq!(
            head_repository.name_with_owner.as_deref(),
            Some("Extra-Chill/homeboy")
        );
        assert_eq!(head_repository.name.as_deref(), Some("homeboy"));
        assert_eq!(record.head_ref_oid.as_deref(), Some("abc123def456"));
        assert_eq!(record.merged_at.as_deref(), Some("2026-01-02T03:04:05Z"));
        assert_eq!(record.review_decision.as_deref(), Some("APPROVED"));
        assert_eq!(record.merge_state_status.as_deref(), Some("CLEAN"));
        assert_eq!(record.status_check_rollup.len(), 1);
    }

    /// `gh` can omit fields or emit explicit `null`s; both read as empty
    /// rather than rejecting the payload.
    #[test]
    fn missing_and_null_fields_parse_as_defaults() {
        for raw in [
            r#"{"number": 7}"#,
            r#"{
                "number": 7, "url": null, "state": null, "isDraft": null,
                "baseRefName": null, "headRefName": null, "statusCheckRollup": null,
                "title": null, "author": null, "headRepository": null,
                "headRefOid": null, "mergedAt": null, "reviewDecision": null,
                "mergeStateStatus": null
            }"#,
        ] {
            let record = parse_pr_view(raw).expect("sparse payload parses");
            assert_eq!(record.number, 7);
            assert_eq!(record.state, "");
            assert!(!record.is_draft);
            assert_eq!(record.base_ref_name, "");
            assert!(record.status_check_rollup.is_empty());
            assert!(record.head_repository.is_none());
            assert!(record.merge_state_status.is_none());
        }
    }

    #[test]
    fn pr_view_args_requests_the_shared_superset() {
        assert_eq!(
            pr_view_args("42", Some("Extra-Chill/homeboy")),
            [
                "pr",
                "view",
                "42",
                "-R",
                "Extra-Chill/homeboy",
                "--json",
                PR_VIEW_JSON_FIELDS,
            ]
        );
        assert_eq!(
            pr_view_args("https://github.com/Extra-Chill/homeboy/pull/9", None),
            [
                "pr",
                "view",
                "https://github.com/Extra-Chill/homeboy/pull/9",
                "--json",
                PR_VIEW_JSON_FIELDS,
            ]
        );
    }
}
