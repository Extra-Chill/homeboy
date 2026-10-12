//! Shared helpers for the `runs` subcommand family.
//!
//! Hosts duration parsing (used by `--since` flags) and JSONPath field
//! selection (used by `show`/`evidence` `--field`).

use std::time::Duration;

use homeboy::core::observation::{ArtifactRecord, ObservationStore, RunRecord};
use homeboy::core::Error;
use homeboy::rig::RigRunArtifactIndex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Serialize, Deserialize, Clone)]
pub struct RunSummary {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub component_id: Option<String>,
    pub rig_id: Option<String>,
    pub git_sha: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_note: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_index: Option<RigRunArtifactIndex>,
    /// Cook attempts and lab-hydration children nested under the Cook that owns
    /// them. Empty for ordinary runs, and omitted from the wire document then.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_runs: Vec<RunSummary>,
}

pub(crate) fn run_summaries_with_artifact_indexes(
    store: &ObservationStore,
    run_records: Vec<RunRecord>,
) -> homeboy::core::Result<Vec<RunSummary>> {
    let rig_run_ids = run_records
        .iter()
        .filter(|run| run.kind == "rig" && run.rig_id.is_some())
        .map(|run| run.id.clone())
        .collect::<Vec<_>>();
    let mut artifacts_by_run = store.list_artifacts_for_runs(&rig_run_ids)?;
    // Resolved once for the whole listing, from the store being listed. The
    // ambient form resolved per row and guarded first to keep that affordable;
    // asking the store once removes the per-row cost and the disagreement
    // between the listing's home and the store's (#7505).
    let artifact_root = store.artifact_root()?;
    Ok(run_records
        .into_iter()
        .map(|run| {
            let artifacts = artifacts_by_run.remove(&run.id).unwrap_or_default();
            run_summary_with_artifact_index(&artifact_root, run, &artifacts)
        })
        .collect())
}

fn run_summary_with_artifact_index(
    artifact_root: &std::path::Path,
    run: RunRecord,
    artifacts: &[ArtifactRecord],
) -> RunSummary {
    let artifact_index =
        homeboy::rig::artifact_index_for_run_with_artifacts(artifact_root, &run, artifacts);
    RunSummary {
        artifact_index,
        ..super::run_summary(run)
    }
}

/// Convert a `--since <duration>` flag value into an RFC-3339 timestamp.
///
/// Accepts the same forms as elsewhere in the runs surface (s, m, h, d).
/// Returned timestamp is `now - duration` so the caller can compare with
/// `started_at >= threshold` semantics.
pub(crate) fn since_threshold(raw: &str) -> homeboy::core::Result<String> {
    let duration = parse_duration(raw)?;
    let chrono_duration = chrono::Duration::from_std(duration).map_err(|e| {
        Error::validation_invalid_argument("since", e.to_string(), Some(raw.to_string()), None)
    })?;
    Ok((chrono::Utc::now() - chrono_duration).to_rfc3339())
}

/// Parse a duration string for the `--since` family of flags.
///
/// The unit table lives in `commands::utils::watch` so every duration-taking
/// flag in the CLI accepts the same units; this only names the field the error
/// is attributed to.
pub(crate) fn parse_duration(raw: &str) -> homeboy::core::Result<Duration> {
    crate::commands::utils::watch::parse_duration("since", raw)
}

/// Compile a JSONPath expression. Returns a structured validation error on
/// invalid syntax instead of panicking. Schema-blind: the engine doesn't know
/// what the JSON looks like, only how to walk it.
pub(crate) fn compile_jsonpath(expr: &str) -> homeboy::core::Result<serde_json_path::JsonPath> {
    serde_json_path::JsonPath::parse(expr).map_err(|e| {
        Error::validation_invalid_argument(
            "jsonpath",
            format!("invalid JSONPath expression: {e}"),
            Some(expr.to_string()),
            None,
        )
    })
}

/// Apply a compiled JSONPath to a JSON value and return all matched nodes,
/// each as an owned `Value` clone.
pub(crate) fn eval_jsonpath(path: &serde_json_path::JsonPath, value: &Value) -> Vec<Value> {
    path.query(value).all().into_iter().cloned().collect()
}

/// Project one or more JSONPath selectors over `root`, returning a
/// `(selector, value)` pair per expression. Each value collapses the matched
/// nodes: `null` for no match, the single match, or an array for many. Backs
/// the `-q`/`--field` selectors on `runs show` and `runs artifact get` so a
/// caller extracts only the fields it needs instead of the whole structure.
pub(crate) fn select_fields(
    root: &Value,
    exprs: &[String],
) -> homeboy::core::Result<Vec<(String, Value)>> {
    let mut selected = Vec::with_capacity(exprs.len());
    for expr in exprs {
        let trimmed = expr.trim();
        if trimmed.is_empty() {
            return Err(Error::validation_invalid_argument(
                "field",
                "--field/-q selector must not be empty",
                None,
                None,
            ));
        }
        let path = compile_jsonpath(trimmed)?;
        let matches = eval_jsonpath(&path, root);
        let value = match matches.len() {
            0 => Value::Null,
            1 => matches.into_iter().next().unwrap(),
            _ => Value::Array(matches),
        };
        selected.push((trimmed.to_string(), value));
    }
    Ok(selected)
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
pub struct SkippedArtifactRow {
    pub run_id: String,
    pub artifact_id: String,
    pub artifact_kind: String,
    pub artifact_type: String,
    pub path: String,
    pub reason: String,
}

/// Shared test fixture: a run whose owner PID is unreachable, so it reads as
/// dead/abandoned. Used by both `tests` and `bundle_import_tests`.
#[cfg(test)]
pub(super) fn dead_owned_run(id: &str) -> RunRecord {
    RunRecord {
        id: id.to_string(),
        kind: "bench".to_string(),
        component_id: Some("homeboy".to_string()),
        started_at: "2026-05-02T16:46:46Z".to_string(),
        finished_at: None,
        status: "running".to_string(),
        command: Some("homeboy bench".to_string()),
        cwd: Some("/tmp/homeboy-fixture".to_string()),
        homeboy_version: Some("test-version".to_string()),
        git_sha: Some("abc123".to_string()),
        rig_id: Some("studio".to_string()),
        metadata_json: serde_json::json!({ "homeboy_run_owner": { "pid": u32::MAX } }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_duration_accepts_supported_units() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("3d").unwrap(), Duration::from_secs(259_200));
    }

    #[test]
    fn parse_duration_rejects_zero_and_unknown_units() {
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("10x").is_err());
        assert!(parse_duration("h").is_err());
    }

    #[test]
    fn select_fields_collapses_matches_per_selector() {
        let root = serde_json::json!({
            "status": "pass",
            "metadata": { "run_dir": "/tmp/run" },
            "tags": ["a", "b"]
        });
        let selected = select_fields(
            &root,
            &[
                "$.status".to_string(),
                "$.metadata.run_dir".to_string(),
                "$.tags[*]".to_string(),
                "$.missing".to_string(),
            ],
        )
        .expect("select");
        assert_eq!(
            selected[0],
            ("$.status".to_string(), Value::String("pass".into()))
        );
        assert_eq!(
            selected[1],
            (
                "$.metadata.run_dir".to_string(),
                Value::String("/tmp/run".into())
            )
        );
        assert_eq!(selected[2].1, serde_json::json!(["a", "b"]));
        assert_eq!(selected[3].1, Value::Null);
    }

    #[test]
    fn select_fields_rejects_empty_and_invalid_selectors() {
        let root = serde_json::json!({ "a": 1 });
        assert!(select_fields(&root, &["   ".to_string()]).is_err());
        assert!(select_fields(&root, &["$[".to_string()]).is_err());
    }

    #[test]
    fn eval_jsonpath_matches_root_property() {
        let path = compile_jsonpath("$.color").expect("compile");
        let value = serde_json::json!({ "color": "red" });
        let matches = eval_jsonpath(&path, &value);
        assert_eq!(matches, vec![Value::String("red".into())]);
    }

    #[test]
    fn eval_jsonpath_matches_array_wildcard() {
        let path = compile_jsonpath("$.runs[*].status").expect("compile");
        let value = serde_json::json!({
            "runs": [
                { "status": "pass" },
                { "status": "fail" },
                { "status": "pass" }
            ]
        });
        let matches = eval_jsonpath(&path, &value);
        assert_eq!(matches.len(), 3);
    }
}
