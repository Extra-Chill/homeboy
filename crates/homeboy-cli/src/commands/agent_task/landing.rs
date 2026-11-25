//! Workflow-callable batch landing. Unknown failures are never assumed flaky.
use super::{args::CookFeedbackArgs, CmdResult};
use clap::Args;
use homeboy::core::{Error, Result};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
    time::{Duration, Instant},
};

#[derive(Args, Debug)]
pub struct BatchLandArgs {
    /// Published children as COOK_ID=https://github.com/OWNER/REPO/pull/N (repeatable).
    #[arg(long = "child", required = true)]
    pub children: Vec<String>,
    /// Merge strategy: squash, merge, or rebase.
    #[arg(long, default_value = "squash", value_parser = ["squash", "merge", "rebase"])]
    pub method: String,
    /// Permit bypassing repository protection with an administrator merge.
    #[arg(long)]
    pub admin: bool,
    /// Check names that must exist and succeed (repeatable).
    #[arg(long = "required-check")]
    pub required_checks: Vec<String>,
    #[arg(long, default_value_t = 1)]
    pub max_reruns: usize,
    #[arg(long, default_value_t = 1)]
    pub max_updates: usize,
    #[arg(long, default_value_t = 2)]
    pub max_repairs: usize,
    /// Overall observation bound. Unfinished children get explicit reasons.
    #[arg(long, default_value_t = 3600)]
    pub timeout_seconds: u64,
    #[arg(long, default_value_t = 30)]
    pub poll_seconds: u64,
}

fn gh(args: &[&str]) -> Result<String> {
    let output = Command::new("gh")
        .args(args)
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), None))?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
fn gh_json(args: &[&str]) -> Result<Value> {
    serde_json::from_str(&gh(args)?).map_err(|e| Error::internal_unexpected(e.to_string()))
}

struct Child {
    cook: String,
    url: String,
    repo: String,
    run: String,
    events: Vec<Value>,
    reason: Option<String>,
    merged: bool,
}
impl Child {
    fn count(&self, action: &str) -> usize {
        self.events.iter().filter(|e| e["action"] == action).count()
    }
    fn record(&mut self, action: &str, detail: Value) -> Result<()> {
        let event = json!({"action": action, "pr": self.url, "detail": detail});
        let store =
            homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
        homeboy::agents::agent_task_lifecycle::record_cook_landing_event_in_store(
            &store,
            &self.run,
            event.clone(),
        )?;
        self.events.push(event);
        Ok(())
    }
}

pub(crate) fn land(args: BatchLandArgs) -> CmdResult<Value> {
    let mut children = Vec::new();
    let mut seen = BTreeSet::new();
    // Validate all identities before performing any remote mutations.
    for input in &args.children {
        let (cook, url) = input
            .split_once('=')
            .ok_or_else(|| Error::internal_unexpected("expected COOK_ID=PR_URL"))?;
        let path = url
            .strip_prefix("https://github.com/")
            .unwrap_or_default()
            .split('/')
            .collect::<Vec<_>>();
        if path.len() != 4
            || path[2] != "pull"
            || path[3].parse::<u64>().is_err()
            || !seen.insert(url.to_string())
        {
            return Err(Error::internal_unexpected(
                "expected a unique GitHub PR URL",
            ));
        }
        let selection = homeboy::agents::agent_task_service::select_cook_candidate(cook)?;
        if selection.incomplete || selection.run_id.is_empty() {
            return Err(Error::internal_unexpected(
                "child has no unique Cook candidate",
            ));
        }
        let record = homeboy::agents::agent_task_lifecycle::exact_record(&selection.run_id)?;
        let events = record.metadata["landing_events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e.get("event"))
            .filter(|e| e["pr"] == url)
            .cloned()
            .collect();
        children.push(Child {
            cook: cook.into(),
            url: url.into(),
            repo: format!("{}/{}", path[0], path[1]),
            run: selection.run_id,
            events,
            reason: None,
            merged: false,
        });
    }
    let deadline = Instant::now() + Duration::from_secs(args.timeout_seconds);
    loop {
        for child in children
            .iter_mut()
            .filter(|c| !c.merged && c.reason.is_none())
        {
            if let Err(error) = advance(child, &args) {
                child.record("blocked", json!({"reason": error.message}))?;
                child.reason = Some(error.message);
            }
        }
        if children.iter().all(|c| c.merged || c.reason.is_some()) {
            break;
        }
        if Instant::now() >= deadline {
            for child in children
                .iter_mut()
                .filter(|c| !c.merged && c.reason.is_none())
            {
                child.record(
                    "blocked",
                    json!({"reason": "landing observation deadline exceeded"}),
                )?;
                child.reason = Some("landing observation deadline exceeded".into());
            }
            break;
        }
        std::thread::sleep(
            Duration::from_secs(args.poll_seconds.max(1))
                .min(deadline.saturating_duration_since(Instant::now())),
        );
    }
    let mut ledger: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for child in &children {
        for event in child.events.iter().filter(|e| e["action"] == "flake") {
            if let Some(test) = event["detail"]["test"].as_str() {
                ledger
                    .entry(test.into())
                    .or_default()
                    .insert(child.url.clone());
                *counts.entry(test.into()).or_default() += 1;
            }
        }
    }
    let flakes = ledger
        .into_iter()
        .map(|(test, prs)| json!({"count": counts[&test], "test": test, "prs": prs}))
        .collect::<Vec<_>>();
    let merged = children.iter().all(|c| c.merged);
    Ok((
        json!({"schema": "homeboy/batch-landing/v1", "complete": true, "all_merged": merged,
        "children": children.iter().map(|c| json!({"cook_id": c.cook, "pr": c.url, "merged": c.merged, "operator_reason": c.reason})).collect::<Vec<_>>(),
        "flake_ledger": flakes}),
        if merged { 0 } else { 1 },
    ))
}

fn repair(child: &mut Child, args: &BatchLandArgs, sha: &str, message: &str) -> Result<()> {
    // Do not repeatedly enqueue feedback for the same candidate while its Cook repairs it.
    if child
        .events
        .iter()
        .any(|e| e["action"] == "repair" && e["detail"]["sha"] == sha)
    {
        return Ok(());
    }
    if child.count("repair") >= args.max_repairs {
        return Err(Error::internal_unexpected("Cook repair budget exhausted"));
    }
    child.record("repair", json!({"sha": sha, "feedback": message}))?;
    let (feedback, _) = super::feedback::feedback(CookFeedbackArgs {
        cook_id: child.cook.clone(),
        candidate: Some(sha.into()),
        text: Some(message.into()),
        file: None,
        stdin: false,
        author: "homeboy-landing".into(),
        source: "batch-landing".into(),
        idempotency_key: Some(format!("landing-{}-{sha}", child.cook)),
        status: false,
    })?;
    if feedback["remediation_queued"] != true {
        return Err(Error::internal_unexpected(format!(
            "Cook repair blocked: {}",
            feedback["remediation_blocker"]
        )));
    }
    let status =
        Command::new(std::env::current_exe().map_err(|e| Error::internal_io(e.to_string(), None))?)
            .args(["agent-task", "recover", &child.cook])
            .status()
            .map_err(|e| Error::internal_io(e.to_string(), None))?;
    if !status.success() {
        return Err(Error::internal_unexpected("Cook recovery admission failed"));
    }
    Ok(())
}

fn advance(child: &mut Child, args: &BatchLandArgs) -> Result<()> {
    let pr = gh_json(&[
        "pr",
        "view",
        &child.url,
        "--json",
        "state,isDraft,headRefOid,mergeStateStatus,statusCheckRollup,files",
    ])?;
    if pr["state"] == "MERGED" {
        child.record("merged", json!({}))?;
        child.merged = true;
        return Ok(());
    }
    if pr["state"] != "OPEN" {
        return Err(Error::internal_unexpected("PR is closed without merging"));
    }
    let sha = pr["headRefOid"]
        .as_str()
        .ok_or_else(|| Error::internal_unexpected("PR has no head SHA"))?;
    let selected = homeboy::agents::agent_task_service::select_cook_candidate(&child.cook)?;
    if selected.incomplete || selected.run_id.is_empty() {
        return Ok(());
    }
    let record = homeboy::agents::agent_task_lifecycle::exact_record(&selected.run_id)?;
    if homeboy::agents::agent_task_feedback::candidate_sha(&record).as_deref() != Some(sha) {
        return Ok(());
    }
    if pr["mergeStateStatus"] == "DIRTY" {
        return repair(child, args, sha, "Landing found merge conflicts. Rebase onto the PR base, repair conflicts, rerun gates and update this same PR.");
    }
    let checks = pr["statusCheckRollup"]
        .as_array()
        .ok_or_else(|| Error::internal_unexpected("missing check rollup"))?;
    if checks.is_empty() || checks.iter().any(pending) {
        return Ok(());
    }
    for required in &args.required_checks {
        if !checks.iter().any(|c| check_name(c) == required) {
            return Ok(());
        }
    }
    if checks.iter().all(green) {
        if pr["isDraft"] == true {
            child.record("ready", json!({"sha": sha}))?;
            gh(&["pr", "ready", &child.url])?;
        }
        child.record(
            "merge",
            json!({"sha": sha, "method": args.method, "admin": args.admin}),
        )?;
        let method = format!("--{}", args.method);
        let mut flags = vec![
            "pr",
            "merge",
            &child.url,
            &method,
            "--delete-branch",
            "--match-head-commit",
            sha,
        ];
        if args.admin {
            flags.push("--admin");
        }
        gh(&flags)?;
        return Ok(());
    }
    let changed = pr["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["path"].as_str())
        .collect::<BTreeSet<_>>();
    let endpoint = format!("repos/{}/commits/{sha}/check-runs?per_page=100", child.repo);
    let runs = gh_json(&["api", &endpoint])?;
    let mut failures = Vec::new();
    for check in checks.iter().filter(|c| !green(c)) {
        let run = runs["check_runs"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["name"] == check_name(check));
        let Some(run) = run else {
            return repair(
                child,
                args,
                sha,
                &format!(
                    "Landing check failed: {}. Diagnose and repair this same PR.",
                    check_name(check)
                ),
            );
        };
        let annotations = gh_json(&[
            "api",
            &format!(
                "repos/{}/check-runs/{}/annotations?per_page=100",
                child.repo, run["id"]
            ),
        ])?;
        let annotations = annotations
            .as_array()
            .ok_or_else(|| Error::internal_unexpected("invalid check annotations"))?;
        let relevant = annotations
            .iter()
            .filter(|a| a["annotation_level"] == "failure")
            .collect::<Vec<_>>();
        if annotations.len() >= 100
            || relevant.is_empty()
            || relevant.iter().any(|a| !untouched_test(a, &changed))
        {
            return repair(child, args, sha, &format!("Landing check failed: {}. Evidence: {annotations:?}. Repair and republish this same PR.", check_name(check)));
        }
        let run_id = run["details_url"]
            .as_str()
            .and_then(|u| u.split("/actions/runs/").nth(1))
            .and_then(|s| s.split('/').next())
            .filter(|s| s.parse::<u64>().is_ok());
        let Some(run_id) = run_id else {
            return repair(
                child,
                args,
                sha,
                "Failed check cannot be rerun automatically; diagnose and repair.",
            );
        };
        for a in relevant {
            failures.push((
                format!(
                    "{}:{}",
                    a["path"].as_str().unwrap_or_default(),
                    a["title"].as_str().unwrap_or(check_name(check))
                ),
                run_id.to_string(),
            ));
        }
    }
    // A failed run is acted on once per observed check set, not once per poll.
    let fingerprint = serde_json::to_string(checks).unwrap_or_default();
    if child
        .events
        .iter()
        .any(|e| e["detail"]["fingerprint"] == fingerprint)
    {
        return Ok(());
    }
    for (test, _) in &failures {
        child.record("flake", json!({"test": test, "sha": sha}))?;
    }
    if pr["mergeStateStatus"] == "BEHIND" && child.count("update") < args.max_updates {
        child.record("update", json!({"sha": sha, "fingerprint": fingerprint}))?;
        gh(&["pr", "update-branch", &child.url])?;
    } else if child.count("rerun") < args.max_reruns {
        child.record("rerun", json!({"sha": sha, "fingerprint": fingerprint}))?;
        for run in failures
            .into_iter()
            .map(|(_, run)| run)
            .collect::<BTreeSet<_>>()
        {
            gh(&["run", "rerun", &run, "--failed", "--repo", &child.repo])?;
        }
    } else {
        return repair(
            child,
            args,
            sha,
            "Landing flake retry budget exhausted. Diagnose recurring failures and update this PR.",
        );
    }
    Ok(())
}
fn check_name(c: &Value) -> &str {
    c["name"]
        .as_str()
        .or_else(|| c["context"].as_str())
        .unwrap_or_default()
}
fn green(c: &Value) -> bool {
    c["conclusion"] == "SUCCESS"
        || c["conclusion"] == "NEUTRAL"
        || c["conclusion"] == "SKIPPED"
        || c["state"] == "SUCCESS"
}
fn pending(c: &Value) -> bool {
    if c.get("status").is_some() {
        c["status"] != "COMPLETED"
    } else {
        c["state"] == "PENDING" || c["state"] == "EXPECTED"
    }
}
fn untouched_test(a: &Value, changed: &BTreeSet<&str>) -> bool {
    a["path"].as_str().is_some_and(|path| {
        (path.contains("/tests/")
            || path.starts_with("tests/")
            || path.ends_with("_test.rs")
            || path.ends_with("_tests.rs"))
            && !changed.contains(path)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_explicit_untouched_test_evidence_is_flaky() {
        let changed = BTreeSet::from(["tests/touched.rs"]);
        assert!(untouched_test(&json!({"path": "tests/flake.rs"}), &changed));
        assert!(!untouched_test(
            &json!({"path": "tests/touched.rs"}),
            &changed
        ));
        assert!(!untouched_test(&json!({"path": "src/lib.rs"}), &changed));
        assert!(!untouched_test(&json!({}), &changed));
    }
    #[test]
    fn pending_and_failed_checks_never_merge() {
        assert!(pending(&json!({"status": "IN_PROGRESS"})));
        assert!(pending(&json!({"state": "PENDING"})));
        assert!(!green(&json!({"conclusion": "FAILURE"})));
        assert!(green(&json!({"conclusion": "SUCCESS"})));
    }
}
