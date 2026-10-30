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

/// Seam for tests and alternative GitHub transports. Checks must belong to the
/// returned head SHA and include only required checks (all checks if unavailable).
pub trait CookCiSource {
    fn checks(&self, pr_url: &str) -> Result<(String, Vec<Value>)>;
}

pub struct GithubCookCi;
impl CookCiSource for GithubCookCi {
    fn checks(&self, pr_url: &str) -> Result<(String, Vec<Value>)> {
        let parts: Vec<_> = pr_url
            .strip_prefix("https://")
            .unwrap_or("")
            .trim_end_matches('/')
            .split('/')
            .collect();
        if parts.len() != 5 || parts[3] != "pull" || parts[4].parse::<u64>().is_err() {
            return Err(Error::internal_unexpected("expected GitHub PR URL"));
        }
        let client = GhClient::from_repo_arg(&format!("{}/{}/{}", parts[0], parts[1], parts[2]))?;
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
        let repo = format!("repos/{}/{}", parts[1], parts[2]);
        let pr: Value = client.api_json(&format!("{repo}/pulls/{}", parts[4]))?;
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

pub fn poll_cook_ci(source: &dyn CookCiSource, pr_url: &str) -> Result<CookCiEvidence> {
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
        fn checks(&self, _: &str) -> Result<(String, Vec<Value>)> {
            Ok(("head".into(), self.0.clone()))
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
    fn cook_ci_green() {
        assert_eq!(poll("SUCCESS").status, "verified");
    }
    #[test]
    fn cook_ci_red_feedback() {
        let evidence = poll("FAILURE");
        assert_eq!(evidence.status, "ci_gate_failed");
        assert_eq!(evidence.feedback, ["candidate: https://ci/log"]);
    }
    #[test]
    fn cook_ci_infra_cancelled_timeout() {
        for state in ["CANCELLED", "TIMED_OUT"] {
            let evidence = poll(state);
            assert_eq!(evidence.status, "ci_infrastructure");
            assert!(evidence.feedback.is_empty());
        }
    }
    #[test]
    fn cook_ci_empty_and_running_wait() {
        assert_eq!(poll("IN_PROGRESS").status, "awaiting_ci");
        assert_eq!(
            poll_cook_ci(&Fake(vec![]), "pr").unwrap().status,
            "awaiting_ci"
        );
    }
}
