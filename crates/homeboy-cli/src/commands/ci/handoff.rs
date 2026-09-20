use homeboy::agents::agent_task_controller_service::{self, ControllerApplyEventRequest};
use homeboy::core::git::GhClient;
use homeboy::core::Error;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Deserialize)]
struct PullRequestBinding {
    base: PullRequestRef,
    head: PullRequestRef,
}

#[derive(Debug, Clone, Deserialize)]
struct PullRequestRef {
    sha: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CheckRunsResponse {
    check_runs: Vec<CheckRun>,
}

#[derive(Debug, Clone, Deserialize)]
struct CheckRun {
    id: u64,
    name: String,
    status: String,
    conclusion: Option<String>,
    updated_at: Option<String>,
    html_url: Option<String>,
}

pub(crate) struct HandoffArgs {
    pub repo: String,
    pub pr: u64,
    pub loop_id: String,
    pub gate_id: String,
    pub check_id: String,
    pub base_sha: String,
    pub head_sha: String,
    pub environment_digest: String,
}

pub(crate) fn publish(args: HandoffArgs) -> Result<Value, Error> {
    let gh = GhClient::from_repo_arg(&args.repo)?;
    gh.ensure_ready()?;
    let repo = gh.repo_path()?.to_string();
    let pr: PullRequestBinding = gh.api_json(&format!("/repos/{repo}/pulls/{}", args.pr))?;
    if pr.base.sha != args.base_sha || pr.head.sha != args.head_sha {
        return Err(Error::validation_invalid_argument(
            "candidate",
            "GitHub PR base/head no longer matches the Cook candidate identity",
            Some(format!("base={} head={}", pr.base.sha, pr.head.sha)),
            None,
        ));
    }
    let checks: CheckRunsResponse = gh.api_json(&format!(
        "/repos/{repo}/commits/{}/check-runs?per_page=100",
        args.head_sha
    ))?;
    let check = checks
        .check_runs
        .into_iter()
        .find(|check| check.name == args.check_id || check.id.to_string() == args.check_id)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "check_id",
                "GitHub did not return the requested check for the exact PR head",
                Some(args.check_id.clone()),
                None,
            )
        })?;
    let observed_at = check
        .updated_at
        .clone()
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let sequence = chrono::DateTime::parse_from_rfc3339(&observed_at)
        .map(|time| time.timestamp_millis().max(0) as u64)
        .unwrap_or(check.id);
    let publication = json!({
        "schema": "homeboy/external-check-publication/v1",
        "provider": "github",
        "repository": repo,
        "base_sha": args.base_sha,
        "head_sha": args.head_sha,
        "gate_id": args.gate_id,
        "check_id": args.check_id,
        "environment_digest": args.environment_digest,
        "status": check.status,
        "conclusion": check.conclusion,
        "observed_at": observed_at,
        "sequence": sequence,
        "evidence_id": format!("github-check-run:{}", check.id),
        "authoritative": true,
        "hydrated": true,
        "url": check.html_url,
    });
    let report = agent_task_controller_service::apply_event(ControllerApplyEventRequest {
        loop_id: args.loop_id,
        event_type: "external.checks_changed".to_string(),
        event_id: Some(format!("github-check-run:{}:{}", check.id, sequence)),
        event_key: Some(format!("{repo}#{}", args.pr)),
        entity_id: None,
        payload: json!({ "publication": publication }),
    })?;
    serde_json::to_value(report).map_err(|error| Error::internal_json(error.to_string(), None))
}
