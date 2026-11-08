//! Bounded, exact-head merge gate. GitHub enforces the final SHA comparison atomically.
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use super::gh_client::GhClient;
use crate::error::{Error, Result};

#[derive(Debug, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum MergeHold {
    HeadMoved,
    ChecksFailed { names: Vec<String> },
    ChecksMissing,
    Conflicting,
    Timeout,
}

#[derive(Debug, Serialize)]
pub struct MergeWhenGreenOutput {
    pub merged: bool,
    pub number: u64,
    pub expected_head_sha: String,
    pub hold: Option<MergeHold>,
}

/// Merge only a checked, unchanged head, using a repository-enabled method.
pub fn pr_merge_when_green(
    repo: &str,
    number: u64,
    sha: &str,
    wait: Duration,
) -> Result<MergeWhenGreenOutput> {
    if sha.len() != 40 || !sha.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::validation_invalid_argument(
            "sha",
            "expected a full 40-character commit SHA",
            None,
            None,
        ));
    }
    let gh = GhClient::from_repo_arg(repo)?;
    let repo = gh.repo_path()?;
    let sha = sha.to_ascii_lowercase();
    let deadline = Instant::now() + wait.min(Duration::from_secs(3600));
    let mut result = MergeWhenGreenOutput {
        merged: false,
        number,
        expected_head_sha: sha.clone(),
        hold: None,
    };
    let hold = loop {
        let args = [
            "pr",
            "view",
            &number.to_string(),
            "--repo",
            repo,
            "--json",
            "headRefOid,mergeable,statusCheckRollup,isDraft,state",
        ];
        let output = match gh.output_until(&args, deadline) {
            Ok(output) => output,
            Err(_) if Instant::now() >= deadline => break MergeHold::Timeout,
            Err(error) => return Err(error),
        };
        if !output.status.success() {
            return Err(Error::internal_io(
                String::from_utf8_lossy(&output.stderr).into_owned(),
                None,
            ));
        }
        let view: Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| Error::internal_json(e.to_string(), None))?;
        if view["headRefOid"].as_str() != Some(&sha) {
            break MergeHold::HeadMoved;
        }
        if view["mergeable"] == "CONFLICTING" {
            break MergeHold::Conflicting;
        }
        let checks = view["statusCheckRollup"].as_array();
        let Some(checks) = checks.filter(|checks| !checks.is_empty()) else {
            break MergeHold::ChecksMissing;
        };
        let mut pending = false;
        let mut failed = Vec::new();
        for check in checks {
            // CheckRun and legacy StatusContext have different shapes.
            let status = check["status"].as_str();
            let outcome = if check.get("state").is_some() {
                check["state"].as_str()
            } else if status == Some("COMPLETED") {
                check["conclusion"].as_str()
            } else {
                pending = true;
                continue;
            };
            match outcome {
                Some("SUCCESS" | "NEUTRAL" | "SKIPPED") => {}
                Some("PENDING" | "EXPECTED") => pending = true,
                _ => failed.push(
                    check["name"]
                        .as_str()
                        .or_else(|| check["context"].as_str())
                        .unwrap_or("unnamed")
                        .to_string(),
                ),
            }
        }
        if !failed.is_empty() {
            break MergeHold::ChecksFailed { names: failed };
        }
        if !pending
            && view["mergeable"] == "MERGEABLE"
            && view["isDraft"] == false
            && view["state"] == "OPEN"
        {
            let policy: Value = match gh.api_json_until(&format!("repos/{repo}"), deadline) {
                Ok(policy) => policy,
                Err(_) if Instant::now() >= deadline => break MergeHold::Timeout,
                Err(error) => return Err(error),
            };
            let method = ["squash", "merge", "rebase"]
                .into_iter()
                .find(|method| {
                    let key = if *method == "merge" {
                        "allow_merge_commit".to_string()
                    } else {
                        format!("allow_{method}_merge")
                    };
                    policy[key] == true
                })
                .ok_or_else(|| Error::internal_io("Repository permits no merge method", None))?;
            // REST's sha field is an atomic compare-and-merge, not a client-side check.
            let endpoint = format!("repos/{repo}/pulls/{number}/merge");
            let sha_arg = format!("sha={sha}");
            let method_arg = format!("merge_method={method}");
            let output = match gh.output_until(
                &[
                    "api",
                    &endpoint,
                    "--method",
                    "PUT",
                    "-f",
                    &sha_arg,
                    "-f",
                    &method_arg,
                ],
                deadline,
            ) {
                Ok(output) => output,
                Err(_) if Instant::now() >= deadline => break MergeHold::Timeout,
                Err(error) => return Err(error),
            };
            if output.status.success() {
                let merged: Value = serde_json::from_slice(&output.stdout)
                    .map_err(|e| Error::internal_json(e.to_string(), None))?;
                if merged["merged"] == true {
                    result.merged = true;
                    return Ok(result);
                }
            }
            let message = String::from_utf8_lossy(&output.stderr);
            if message.contains("409") {
                break MergeHold::HeadMoved;
            }
            return Err(Error::internal_io(
                format!(
                    "GitHub refused merge: {} {}",
                    message,
                    String::from_utf8_lossy(&output.stdout)
                ),
                None,
            ));
        }
        if Instant::now() >= deadline {
            break MergeHold::Timeout;
        }
        std::thread::sleep(
            Duration::from_secs(2).min(deadline.saturating_duration_since(Instant::now())),
        );
    };
    result.hold = Some(hold);
    Ok(result)
}
