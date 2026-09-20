use homeboy::agents::agent_task_controller_service::{self, ControllerApplyEventRequest};
use homeboy::agents::agent_task_lifecycle;
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
    #[serde(default)]
    total_count: Option<usize>,
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
    let handoff = authoritative_handoff(&args.loop_id)?;
    let base_sha = required_string(&handoff, "base_sha")?;
    let head_sha = required_string(&handoff, "head_sha")?;
    let environment_digest = required_string(&handoff, "environment_digest")?;
    let gate_id = required_string(&handoff, "gate_id")?;
    let check_id = required_string(&handoff, "check_id")?;

    for (field, supplied, authoritative) in [
        ("gate_id", args.gate_id.as_str(), gate_id.as_str()),
        ("check_id", args.check_id.as_str(), check_id.as_str()),
        ("base_sha", args.base_sha.as_str(), base_sha.as_str()),
        ("head_sha", args.head_sha.as_str(), head_sha.as_str()),
        (
            "environment_digest",
            args.environment_digest.as_str(),
            environment_digest.as_str(),
        ),
    ] {
        if supplied != authoritative {
            return Err(Error::validation_invalid_argument(
                field,
                "caller-supplied identity does not match the durable Cook handoff",
                Some(format!("expected={authoritative} supplied={supplied}")),
                None,
            ));
        }
    }

    let gh = GhClient::from_repo_arg(&args.repo)?;
    gh.ensure_ready()?;
    let repo = gh.repo_path()?.to_string();
    let pr: PullRequestBinding = gh.api_json(&format!("/repos/{repo}/pulls/{}", args.pr))?;
    let expected_repository = handoff["publication"]["binding"]["repository"]
        .as_str()
        .or_else(|| handoff["publication"]["repository"].as_str())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "provider_ci_handoff",
                "durable Cook handoff has no authenticated repository identity",
                None,
                None,
            )
        })?;
    let expected_pr = handoff["publication"]["pr_number"]
        .as_u64()
        .or_else(|| handoff["publication"]["pr"]["number"].as_u64())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "provider_ci_handoff",
                "durable Cook handoff has no published pull request identity",
                None,
                None,
            )
        })?;
    if repo != expected_repository || args.pr != expected_pr {
        return Err(Error::validation_invalid_argument(
            "candidate",
            "GitHub repository or PR no longer matches the durable Cook publication",
            Some(format!(
                "expected={expected_repository}#{expected_pr} actual={repo}#{}",
                args.pr
            )),
            None,
        ));
    }
    if pr.base.sha != base_sha || pr.head.sha != head_sha {
        return Err(Error::validation_invalid_argument(
            "candidate",
            "GitHub PR base/head no longer matches the Cook candidate identity",
            Some(format!("base={} head={}", pr.base.sha, pr.head.sha)),
            None,
        ));
    }
    let checks = fetch_check_runs(&gh, &repo, &head_sha)?;
    let check = checks
        .into_iter()
        .filter(|check| check.name == check_id || check.id.to_string() == check_id)
        .max_by_key(|check| {
            (
                check
                    .updated_at
                    .as_deref()
                    .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok()),
                check.id,
            )
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "check_id",
                "GitHub did not return the requested check for the exact PR head",
                Some(check_id.clone()),
                None,
            )
        })?;
    let observed_at = check.updated_at.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "check.updated_at",
            "GitHub check evidence has no provider observation timestamp",
            Some(check.id.to_string()),
            None,
        )
    })?;
    let sequence = chrono::DateTime::parse_from_rfc3339(&observed_at)
        .map(|time| time.timestamp_millis().max(0) as u64)
        .map_err(|_| {
            Error::validation_invalid_argument(
                "check.updated_at",
                "GitHub check evidence has an invalid provider observation timestamp",
                Some(observed_at.clone()),
                None,
            )
        })?;
    let publication = json!({
        "schema": "homeboy/external-check-publication/v1",
        "provider": "github",
        "repository": repo,
        "base_sha": base_sha,
        "head_sha": head_sha,
        "gate_id": gate_id,
        "check_id": check_id,
        "environment_digest": environment_digest,
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

fn required_string(handoff: &Value, field: &str) -> Result<String, Error> {
    handoff[field]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "provider_ci_handoff",
                format!("durable Cook handoff has no {field}"),
                None,
                None,
            )
        })
}

fn authoritative_handoff(loop_id: &str) -> Result<Value, Error> {
    let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let matches = agent_task_lifecycle::list_records_in_store(&store)?
        .into_iter()
        .filter_map(|record| {
            let handoff = record.metadata.get("provider_ci_handoff")?;
            (handoff["loop_id"].as_str() == Some(loop_id))
            .then(|| handoff.clone())
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [handoff] => Ok(handoff.clone()),
        [] => Err(Error::validation_invalid_argument(
            "loop_id",
            "no durable Cook provider CI handoff matches the requested binding",
            Some(loop_id.to_string()),
            None,
        )),
        _ => Err(Error::validation_invalid_argument(
            "loop_id",
            "multiple durable Cook provider CI handoffs match the requested binding",
            Some(loop_id.to_string()),
            None,
        )),
    }
}

fn fetch_check_runs(gh: &GhClient, repo: &str, head_sha: &str) -> Result<Vec<CheckRun>, Error> {
    let mut page = 1;
    let mut checks = Vec::new();
    loop {
        let response: CheckRunsResponse = gh.api_json(&format!(
            "/repos/{repo}/commits/{head_sha}/check-runs?per_page=100&page={page}"
        ))?;
        let count = response.check_runs.len();
        checks.extend(response.check_runs);
        if count < 100
            || response
                .total_count
                .is_some_and(|total| checks.len() >= total)
        {
            return Ok(checks);
        }
        page += 1;
    }
}
