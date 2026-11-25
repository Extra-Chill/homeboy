//! Required-check verification for CI-deferred Cooks.
use crate::git::GhClient;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CookCiEvidence {
    pub status: String,
    pub head_sha: String,
    pub checks: Vec<Value>,
    pub feedback: Vec<String>,
}

/// The pull request facts that decide CI classification before any check is
/// read: GitHub never runs `pull_request` workflows for a conflicting PR, and a
/// merged or closed PR has no CI left to wait for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CookPrState {
    /// GitHub's `state`: `open` or `closed`.
    pub state: String,
    pub merged: bool,
    /// `None` while GitHub is still computing mergeability.
    pub mergeable: Option<bool>,
    /// GitHub's `mergeable_state` (`clean`, `dirty`, `blocked`, `unknown`, ...).
    pub mergeable_state: String,
    pub base_ref: String,
    pub head_sha: String,
}

impl CookPrState {
    fn conflicting(&self) -> bool {
        self.mergeable == Some(false) || self.mergeable_state.eq_ignore_ascii_case("dirty")
    }
}

/// Seam for tests and alternative GitHub transports. Checks must belong to the
/// returned head SHA and include only required checks (all checks if unavailable).
pub trait CookCiSource {
    fn pull(&self, pr_url: &str) -> Result<CookPrState>;
    fn checks(&self, pr_url: &str) -> Result<(String, Vec<Value>)>;
}

/// `(host, owner, repo, number)` of a GitHub pull request URL.
fn parse_pr_url(pr_url: &str) -> Result<(String, String, String, String)> {
    let parts: Vec<_> = pr_url
        .strip_prefix("https://")
        .unwrap_or("")
        .trim_end_matches('/')
        .split('/')
        .collect();
    if parts.len() != 5 || parts[3] != "pull" || parts[4].parse::<u64>().is_err() {
        return Err(Error::internal_unexpected("expected GitHub PR URL"));
    }
    Ok((
        parts[0].to_owned(),
        parts[1].to_owned(),
        parts[2].to_owned(),
        parts[4].to_owned(),
    ))
}

pub struct GithubCookCi;
impl CookCiSource for GithubCookCi {
    fn pull(&self, pr_url: &str) -> Result<CookPrState> {
        let (host, owner, repo, number) = parse_pr_url(pr_url)?;
        let client = GhClient::from_repo_arg(&format!("{host}/{owner}/{repo}"))?;
        let pr: Value = client.api_json(&format!("repos/{owner}/{repo}/pulls/{number}"))?;
        let text = |value: &Value| value.as_str().unwrap_or("").to_owned();
        Ok(CookPrState {
            state: text(&pr["state"]),
            merged: pr["merged"].as_bool().unwrap_or(false) || !pr["merged_at"].is_null(),
            mergeable: pr["mergeable"].as_bool(),
            mergeable_state: text(&pr["mergeable_state"]),
            base_ref: text(&pr["base"]["ref"]),
            head_sha: text(&pr["head"]["sha"]),
        })
    }

    fn checks(&self, pr_url: &str) -> Result<(String, Vec<Value>)> {
        let (host, owner, repo_name, number) = parse_pr_url(pr_url)?;
        let client = GhClient::from_repo_arg(&format!("{host}/{owner}/{repo_name}"))?;
        let head = || -> Result<String> {
            let raw = client.run(&[
                "pr".into(),
                "view".into(),
                pr_url.into(),
                "--json".into(),
                "headRefOid".into(),
            ])?;
            let value: Value = serde_json::from_str(&raw)
                .map_err(|e| Error::internal_unexpected(e.to_string()))?;
            value["headRefOid"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| Error::internal_unexpected("PR has no head SHA"))
        };
        let sha = head()?;
        let repo = format!("repos/{owner}/{repo_name}");
        let pr: Value = client.api_json(&format!("{repo}/pulls/{number}"))?;
        let base = pr["base"]["ref"]
            .as_str()
            .ok_or_else(|| Error::internal_unexpected("PR missing base"))?;
        let base = base
            .replace('%', "%25")
            .replace('/', "%2F")
            .replace('#', "%23");
        let mut required = std::collections::BTreeSet::new();
        if let Ok(protection) = client.api_json::<Value>(&format!(
            "{repo}/branches/{base}/protection/required_status_checks"
        )) {
            if let Some(contexts) = protection["contexts"].as_array() {
                required.extend(contexts.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            if let Some(checks) = protection["checks"].as_array() {
                required.extend(
                    checks
                        .iter()
                        .filter_map(|v| v["context"].as_str())
                        .map(str::to_owned),
                );
            }
        }
        if let Ok(rules) = client.api_json::<Vec<Value>>(&format!("{repo}/rules/branches/{base}")) {
            for rule in rules {
                if let Some(checks) = rule["parameters"]["required_status_checks"].as_array() {
                    required.extend(
                        checks
                            .iter()
                            .filter_map(|v| v["context"].as_str())
                            .map(str::to_owned),
                    );
                }
            }
        }
        let mut checks = Vec::new();
        for page in 1.. {
            let result: Value = client.api_json(&format!(
                "{repo}/commits/{sha}/check-runs?filter=latest&per_page=100&page={page}"
            ))?;
            let runs = result["check_runs"]
                .as_array()
                .ok_or_else(|| Error::internal_unexpected("missing check runs"))?;
            for run in runs {
                checks.push(serde_json::json!({"name":run["name"], "state": if run["status"] == "completed" { &run["conclusion"] } else { &run["status"] }, "link":run["html_url"]}));
            }
            if runs.len() < 100 {
                break;
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for page in 1.. {
            let statuses: Vec<Value> = client.api_json(&format!(
                "{repo}/commits/{sha}/statuses?per_page=100&page={page}"
            ))?;
            for status in &statuses {
                if seen.insert(status["context"].as_str().unwrap_or("").to_owned()) {
                    checks.push(serde_json::json!({"name":status["context"], "state":status["state"], "link":status["target_url"]}));
                }
            }
            if statuses.len() < 100 {
                break;
            }
        }
        if !required.is_empty() {
            checks.retain(|c| {
                c["name"]
                    .as_str()
                    .is_some_and(|name| required.contains(name))
            });
            for name in required {
                if !checks.iter().any(|c| c["name"] == name) {
                    checks.push(serde_json::json!({"name":name,"state":"pending"}));
                }
            }
        }
        if head()? != sha {
            return Err(Error::internal_unexpected(
                "PR head changed while polling CI; poll again",
            ));
        }
        Ok((sha, checks))
    }
}

/// Classify a CI-deferred Cook's PR. PR state wins over checks: a merged PR is
/// verified, a closed unmerged PR is terminal (`pr_closed`), and a conflicting
/// PR fails the CI gate because GitHub will not run CI until it is resolved.
pub fn poll_cook_ci(source: &dyn CookCiSource, pr_url: &str) -> Result<CookCiEvidence> {
    let pr = source.pull(pr_url)?;
    let settled = |status: &str, feedback: Vec<String>| CookCiEvidence {
        status: status.into(),
        head_sha: pr.head_sha.clone(),
        checks: Vec::new(),
        feedback,
    };
    if pr.merged {
        return Ok(settled("verified", Vec::new()));
    }
    if pr.state.eq_ignore_ascii_case("closed") {
        return Ok(settled(
            "pr_closed",
            vec![format!("{pr_url} was closed without merging")],
        ));
    }
    if pr.conflicting() {
        let base = if pr.base_ref.is_empty() {
            "the base branch"
        } else {
            pr.base_ref.as_str()
        };
        return Ok(settled(
            "ci_gate_failed",
            vec![format!(
                "{pr_url} has a merge conflict with {base}; GitHub will not run pull_request CI until the conflict is resolved"
            )],
        ));
    }
    let (head_sha, checks) = source.checks(pr_url)?;
    let mut failed = Vec::new();
    let mut infra = false;
    let mut pending = checks.is_empty();
    for check in &checks {
        let state = check["state"].as_str().unwrap_or("").to_ascii_uppercase();
        match state.as_str() {
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => {}
            "CANCELLED" | "TIMED_OUT" | "STARTUP_FAILURE" => infra = true,
            "FAILURE" | "ERROR" | "ACTION_REQUIRED" => failed.push(format!(
                "{}: {}",
                check["name"].as_str().unwrap_or("unknown"),
                check["link"].as_str().unwrap_or("")
            )),
            _ => pending = true,
        }
    }
    let status = if !failed.is_empty() {
        "ci_gate_failed"
    } else if infra {
        "ci_infrastructure"
    } else if pending {
        "awaiting_ci"
    } else {
        "verified"
    };
    Ok(CookCiEvidence {
        status: status.into(),
        head_sha,
        checks,
        feedback: failed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fake(Vec<Value>);
    impl CookCiSource for Fake {
        fn pull(&self, _: &str) -> Result<CookPrState> {
            Ok(open_pr())
        }
        fn checks(&self, _: &str) -> Result<(String, Vec<Value>)> {
            Ok(("head".into(), self.0.clone()))
        }
    }
    fn open_pr() -> CookPrState {
        CookPrState {
            state: "open".into(),
            mergeable: Some(true),
            mergeable_state: "clean".into(),
            base_ref: "main".into(),
            head_sha: "head".into(),
            ..CookPrState::default()
        }
    }
    /// A PR whose state alone must settle classification; reading checks fails.
    struct PrOnly(CookPrState);
    impl CookCiSource for PrOnly {
        fn pull(&self, _: &str) -> Result<CookPrState> {
            Ok(self.0.clone())
        }
        fn checks(&self, _: &str) -> Result<(String, Vec<Value>)> {
            Err(Error::internal_unexpected("checks must not be read"))
        }
    }
    fn poll(state: &str) -> CookCiEvidence {
        poll_cook_ci(
            &Fake(vec![
                serde_json::json!({"name":"candidate", "state":state,"link":"https://ci/log"}),
            ]),
            "pr",
        )
        .unwrap()
    }
    #[test]
    fn cook_ci_check_states_map_to_evidence_status() {
        for (state, status, feedback) in [
            ("SUCCESS", "verified", vec![]),
            (
                "FAILURE",
                "ci_gate_failed",
                vec!["candidate: https://ci/log".to_string()],
            ),
            ("CANCELLED", "ci_infrastructure", vec![]),
            ("TIMED_OUT", "ci_infrastructure", vec![]),
            ("IN_PROGRESS", "awaiting_ci", vec![]),
        ] {
            let evidence = poll(state);
            assert_eq!(evidence.status, status, "{state}");
            assert_eq!(evidence.feedback, feedback, "{state}");
        }
        assert_eq!(
            poll_cook_ci(&Fake(vec![]), "pr").unwrap().status,
            "awaiting_ci"
        );
    }
    #[test]
    fn cook_ci_conflicting_pr_fails_and_names_base() {
        for (mergeable, state) in [(Some(false), "dirty"), (None, "dirty")] {
            let evidence = poll_cook_ci(
                &PrOnly(CookPrState {
                    mergeable,
                    mergeable_state: state.into(),
                    ..open_pr()
                }),
                "https://github.com/o/r/pull/1",
            )
            .unwrap();
            assert_eq!(evidence.status, "ci_gate_failed");
            assert_eq!(evidence.feedback.len(), 1);
            assert!(evidence.feedback[0].contains("merge conflict with main"));
            assert!(evidence.feedback[0].contains("will not run pull_request CI"));
        }
    }
    #[test]
    fn cook_ci_merged_pr_is_verified() {
        let evidence = poll_cook_ci(
            &PrOnly(CookPrState {
                state: "closed".into(),
                merged: true,
                mergeable: None,
                ..open_pr()
            }),
            "pr",
        )
        .unwrap();
        assert_eq!(evidence.status, "verified");
        assert_eq!(evidence.head_sha, "head");
        assert!(evidence.feedback.is_empty());
    }
    #[test]
    fn cook_ci_closed_unmerged_pr_is_terminal() {
        let evidence = poll_cook_ci(
            &PrOnly(CookPrState {
                state: "closed".into(),
                ..open_pr()
            }),
            "pr",
        )
        .unwrap();
        assert_eq!(evidence.status, "pr_closed");
        assert_eq!(evidence.feedback, ["pr was closed without merging"]);
    }
    #[test]
    fn cook_ci_unknown_mergeability_still_reads_checks() {
        let source = Fake(vec![
            serde_json::json!({"name":"candidate","state":"SUCCESS"}),
        ]);
        struct Computing(Fake);
        impl CookCiSource for Computing {
            fn pull(&self, _: &str) -> Result<CookPrState> {
                Ok(CookPrState {
                    mergeable: None,
                    mergeable_state: "unknown".into(),
                    ..open_pr()
                })
            }
            fn checks(&self, url: &str) -> Result<(String, Vec<Value>)> {
                self.0.checks(url)
            }
        }
        assert_eq!(
            poll_cook_ci(&Computing(source), "pr").unwrap().status,
            "verified"
        );
    }
}
