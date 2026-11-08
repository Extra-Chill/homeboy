use homeboy::core::{Error, Result};
use serde_json::Value;
use std::path::Path;

#[derive(clap::Subcommand, Debug)]
pub enum CandidateCommand {
    /// Fetch the selected candidate and check out the Cook's head branch.
    Checkout {
        cook_or_attempt: String,
        #[arg(long)]
        into: std::path::PathBuf,
    },
}

fn git(path: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), None))?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "candidate",
            String::from_utf8_lossy(&output.stderr).trim(),
            None,
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(super) fn run(command: CandidateCommand) -> crate::commands::CmdResult<Value> {
    let CandidateCommand::Checkout {
        cook_or_attempt,
        into,
    } = command;
    ensure_clean(&into)?;
    let target = super::status::resolve_cook_reader_target(&cook_or_attempt, false)?;
    let store = homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    let record = homeboy::agents::agent_task_lifecycle::exact_record_bounded_in_store(
        &store,
        &target.run_id,
    )?;
    let recipe = homeboy::agents::agent_task_service::load_recipe_for_attempt(&target.run_id)?;
    let promotion = &record.metadata["cook_delivery"]["artifacts"]["promotion"];
    let branch = recipe
        .as_ref()
        .and_then(|r| r.finalization["head"].as_str())
        .or_else(|| promotion.pointer("/target/branch").and_then(Value::as_str))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "candidate",
                "Cook has no declared head branch",
                None,
                None,
            )
        })?;
    let cook = recipe
        .as_ref()
        .map(|r| r.cook_id.as_str())
        .or_else(|| record.metadata["cook_id"].as_str())
        .unwrap_or(&cook_or_attempt);
    let reference = format!("refs/homeboy/candidates/{cook}");
    if let Some(source) = promotion.pointer("/target/path").and_then(Value::as_str) {
        if git(Path::new(source), &["show-ref", "--verify", &reference]).is_ok() {
            return checkout_from(&into, source, &reference, branch);
        }
    }
    let observation = homeboy::core::observation::ObservationStore::open_initialized()?;
    let artifact = homeboy::core::observation::runs_service::resolve_artifact_for_run(
        &observation,
        &target.run_id,
        "git_bundle",
    )?;
    let sha = artifact
        .metadata_json
        .pointer("/candidate/candidate_sha")
        .or_else(|| artifact.metadata_json.get("candidate_sha"))
        .or_else(|| {
            artifact
                .metadata_json
                .pointer("/agent_task/runner_provenance/candidate_sha")
        })
        .or_else(|| {
            artifact
                .metadata_json
                .pointer("/agent_task/candidate/candidate_sha")
        })
        .and_then(Value::as_str)
        .or_else(|| record.candidate.as_ref().map(|c| c.candidate_sha.as_str()))
        .or_else(|| {
            promotion
                .pointer("/candidate/candidate_sha")
                .and_then(Value::as_str)
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "candidate",
                "candidate bundle has no candidate_sha metadata",
                None,
                None,
            )
        })?
        .to_string();
    let temp = tempfile::tempdir().map_err(|e| Error::internal_io(e.to_string(), None))?;
    let output = Some(temp.path().join("candidate.bundle"));
    use homeboy::core::observation::runs_service as service;
    let fetched = match service::classify_artifact_storage(&artifact) {
        service::ArtifactStorage::LocalFile => service::copy_local_file_artifact(artifact, output)?,
        service::ArtifactStorage::Remote => service::download_remote_artifact(artifact, output)?,
        service::ArtifactStorage::PublicUrl => service::download_public_artifact(artifact, output)?,
        _ => {
            return Err(Error::validation_invalid_argument(
                "candidate",
                "candidate bundle bytes are unavailable",
                None,
                None,
            ))
        }
    };
    checkout_from(&into, &fetched.output_path.to_string_lossy(), &sha, branch)
}

fn ensure_clean(into: &Path) -> Result<()> {
    if !git(into, &["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
        return Err(Error::validation_invalid_argument(
            "into",
            "worktree has uncommitted changes",
            None,
            None,
        ));
    }
    Ok(())
}

fn checkout_from(
    into: &Path,
    source: &str,
    revision: &str,
    branch: &str,
) -> crate::commands::CmdResult<Value> {
    ensure_clean(into)?;
    git(into, &["check-ref-format", "--branch", branch])?;
    git(into, &["fetch", "--no-tags", "--", source, revision])?;
    let sha = git(into, &["rev-parse", "FETCH_HEAD^{commit}"])?;
    git(into, &["checkout", "-B", branch, &sha, "--"])?;
    let path = into
        .canonicalize()
        .map_err(|e| Error::internal_io(e.to_string(), None))?;
    Ok((
        serde_json::json!({"command": "agent-task.candidate.checkout", "candidate_sha": sha, "branch": branch, "worktree": path}),
        0,
    ))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum CandidateState {
    Finalized,
    ApplyReady,
    PatchAvailable,
    NoChangesProduced,
    Empty,
    Missing,
    Unreadable,
    Conflicting,
    RetainedOnly,
    NotAvailable,
    #[default]
    Unknown,
}

impl CandidateState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Finalized => "finalized",
            Self::ApplyReady => "apply_ready",
            Self::PatchAvailable => "patch_available",
            Self::NoChangesProduced => "no_changes_produced",
            Self::Empty => "empty",
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
            Self::Conflicting => "conflicting",
            Self::RetainedOnly => "retained_only",
            Self::NotAvailable => "not_available",
            Self::Unknown => "unknown",
        }
    }

    pub(crate) fn is_available(self) -> bool {
        matches!(
            self,
            Self::Finalized | Self::ApplyReady | Self::PatchAvailable
        )
    }
}

/// The canonical terminal result for a Cook's candidate evidence. Evidence is
/// monotonic: a finalized PR, retained/apply-ready candidate, or typed candidate
/// remains authoritative even when a later provider attempt is empty or fails.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CandidateResult {
    pub(crate) available: usize,
    pub(crate) empty: usize,
    pub(crate) missing: usize,
    pub(crate) unreadable: usize,
    pub(crate) conflicting: usize,
    pub(crate) retained_only: usize,
    pub(crate) unknown: usize,
    pub(crate) diff_bytes: u64,
    pub(crate) changed_files: usize,
    pub(crate) changed_files_unknown_patches: usize,
    pub(crate) provider_executions: Option<usize>,
    pub(crate) finalized: bool,
    pub(crate) retained: bool,
    pub(crate) attempts_omitted: usize,
    pub(crate) outcomes_omitted: usize,
    pub(crate) artifacts_omitted: usize,
    no_changes_produced: bool,
    not_available: bool,
}

impl CandidateResult {
    pub(crate) fn state(self) -> CandidateState {
        if self.finalized {
            CandidateState::Finalized
        } else if self.retained {
            CandidateState::ApplyReady
        } else if self.available > 0 {
            CandidateState::PatchAvailable
        } else if self.is_degraded() {
            // An incomplete scan cannot honestly prove that no candidate exists.
            CandidateState::Unknown
        } else if self.conflicting > 0 {
            CandidateState::Conflicting
        } else if self.unreadable > 0 {
            CandidateState::Unreadable
        } else if self.missing > 0 {
            CandidateState::Missing
        } else if self.retained_only > 0 {
            CandidateState::RetainedOnly
        } else if self.empty > 0 {
            CandidateState::Empty
        } else if self.not_available {
            CandidateState::NotAvailable
        } else if self.no_changes_produced {
            CandidateState::NoChangesProduced
        } else {
            CandidateState::Unknown
        }
    }

    pub(crate) fn is_degraded(self) -> bool {
        self.attempts_omitted > 0 || self.outcomes_omitted > 0 || self.artifacts_omitted > 0
    }
}

/// Classify only the agents-layer canonical candidate projection.
pub(crate) fn classify_candidates(payload: &Value) -> CandidateResult {
    payload
        .get("canonical_candidate")
        .or_else(|| payload.pointer("/durable_candidate/canonical_candidate"))
        .and_then(candidate_result_from_projection)
        .unwrap_or_default()
}

fn candidate_result_from_projection(value: &Value) -> Option<CandidateResult> {
    if value.get("schema").and_then(Value::as_str) != Some("homeboy/agent-task-candidate/v1") {
        return None;
    }
    let state = match value.get("state").and_then(Value::as_str)? {
        "finalized" => CandidateState::Finalized,
        "apply_ready" => CandidateState::ApplyReady,
        "patch_available" => CandidateState::PatchAvailable,
        "no_changes_produced" => CandidateState::NoChangesProduced,
        "empty" => CandidateState::Empty,
        "missing" => CandidateState::Missing,
        "unreadable" => CandidateState::Unreadable,
        "conflicting" => CandidateState::Conflicting,
        "retained_only" => CandidateState::RetainedOnly,
        "unknown" => CandidateState::Unknown,
        "not_available" => CandidateState::NotAvailable,
        _ => return None,
    };
    let count = |key| {
        value
            .pointer(&format!("/counts/{key}"))
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or_default()
    };
    let omitted = |key| {
        value
            .pointer(&format!("/scan/{key}"))
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or_default()
    };
    Some(CandidateResult {
        available: count("patch_available")
            .max(usize::from(state == CandidateState::PatchAvailable)),
        empty: count("empty").max(usize::from(state == CandidateState::Empty)),
        missing: count("missing").max(usize::from(state == CandidateState::Missing)),
        unreadable: count("unreadable").max(usize::from(state == CandidateState::Unreadable)),
        conflicting: count("conflicting").max(usize::from(state == CandidateState::Conflicting)),
        retained_only: count("retained_only")
            .max(usize::from(state == CandidateState::RetainedOnly)),
        unknown: count("unknown"),
        diff_bytes: value
            .get("diff_bytes")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        changed_files: value
            .get("changed_files")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or_default(),
        changed_files_unknown_patches: value
            .get("changed_files_unknown_patches")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or_default(),
        provider_executions: value
            .get("provider_executions")
            .and_then(Value::as_u64)
            .and_then(|count| usize::try_from(count).ok()),
        finalized: state == CandidateState::Finalized,
        retained: state == CandidateState::ApplyReady,
        attempts_omitted: omitted("attempts_omitted"),
        outcomes_omitted: omitted("outcomes_omitted"),
        artifacts_omitted: omitted("artifacts_omitted"),
        no_changes_produced: state == CandidateState::NoChangesProduced,
        not_available: state == CandidateState::NotAvailable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn checkout_fetches_canonical_ref_and_refuses_dirty_worktree() {
        checkout_fixture(false);
    }

    #[test]
    fn checkout_fetches_bundle_using_candidate_metadata() {
        checkout_fixture(true);
    }

    fn checkout_fixture(bundle: bool) {
        use crate::commands::agent_task::tests::support::{test_plan, with_isolated_home};
        with_isolated_home(|home| {
            let source = home.path().join("source");
            let destination = home.path().join("destination");
            std::fs::create_dir(&source).unwrap();
            git(&source, &["init", "-b", "main"]).unwrap();
            git(&source, &["config", "user.name", "Test"]).unwrap();
            git(&source, &["config", "user.email", "test@example.test"]).unwrap();
            std::fs::write(source.join("file"), "base").unwrap();
            git(&source, &["add", "."]).unwrap();
            git(&source, &["commit", "-m", "base"]).unwrap();
            git(
                home.path(),
                &[
                    "clone",
                    source.to_str().unwrap(),
                    destination.to_str().unwrap(),
                ],
            )
            .unwrap();
            std::fs::write(source.join("file"), "candidate").unwrap();
            git(&source, &["commit", "-am", "candidate"]).unwrap();
            let sha = git(&source, &["rev-parse", "HEAD"]).unwrap();
            let run_id = "checkout-attempt";
            let lifecycle =
                homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()
                    .unwrap();
            let mut plan = test_plan();
            plan.metadata["cook_id"] = json!("checkout-cook");
            plan.metadata["cook_attempt"] = json!(1);
            homeboy::agents::agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
            homeboy::agents::agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record.metadata = json!({"cook_id": "checkout-cook", "cook_delivery": {"artifacts": {"promotion": {
                    "target": {"path": source, "branch": "cook/head"}
                }}}});
            }).unwrap();
            homeboy::agents::agent_task_lifecycle::record_cook_attempt_in_store(
                &lifecycle,
                "checkout-cook",
                1,
                run_id,
            )
            .unwrap();
            if bundle {
                let path = home.path().join("candidate.bundle");
                git(
                    &source,
                    &["bundle", "create", path.to_str().unwrap(), "HEAD"],
                )
                .unwrap();
                let store =
                    homeboy::core::observation::ObservationStore::open_initialized().unwrap();
                let artifact = store.record_artifact(run_id, "git_bundle", &path).unwrap();
                store
                    .update_artifact_metadata(&artifact.id, json!({"candidate_sha": sha}))
                    .unwrap();
            } else {
                git(
                    &source,
                    &["update-ref", "refs/homeboy/candidates/checkout-cook", &sha],
                )
                .unwrap();
            }
            let (value, _) = run(CandidateCommand::Checkout {
                cook_or_attempt: if bundle { run_id } else { "checkout-cook" }.into(),
                into: destination.clone(),
            })
            .unwrap();
            assert_eq!(value["candidate_sha"], sha);
            assert_eq!(
                git(&destination, &["branch", "--show-current"]).unwrap(),
                "cook/head"
            );
            assert_eq!(
                std::fs::read_to_string(destination.join("file")).unwrap(),
                "candidate"
            );
            assert!(git(&destination, &["status", "--porcelain"])
                .unwrap()
                .is_empty());
            std::fs::write(destination.join("untracked"), "dirty").unwrap();
            assert!(run(CandidateCommand::Checkout {
                cook_or_attempt: run_id.into(),
                into: destination.clone()
            })
            .unwrap_err()
            .to_string()
            .contains("uncommitted changes"));
            assert_eq!(git(&destination, &["rev-parse", "HEAD"]).unwrap(), sha);
        });
    }

    #[test]
    fn canonical_projection_is_the_only_candidate_source() {
        for state in [
            "finalized",
            "apply_ready",
            "patch_available",
            "no_changes_produced",
            "retained_only",
            "not_available",
        ] {
            let projection = json!({
                "schema": "homeboy/agent-task-candidate/v1", "state": state,
            });
            for payload in [
                json!({ "canonical_candidate": projection }),
                json!({ "durable_candidate": { "canonical_candidate": projection } }),
            ] {
                assert_eq!(classify_candidates(&payload).state().as_str(), state);
            }
        }
        for payload in [
            json!({}),
            json!({ "artifact_refs": [{ "kind": "patch", "size_bytes": 100 }] }),
            json!({ "aggregate": { "outcomes": [{ "status": "no_op" }] } }),
            json!({ "finalization": { "status": "review_ready", "pr_url": "https://example.test/pr/1" } }),
            json!({ "canonical_candidate": { "schema": "homeboy/agent-task-candidate/v1", "state": "promoted" } }),
        ] {
            assert_eq!(
                classify_candidates(&payload).state(),
                CandidateState::Unknown
            );
        }
    }

    #[test]
    fn omitted_scan_cannot_prove_no_changes() {
        let result = classify_candidates(&json!({ "canonical_candidate": {
            "schema": "homeboy/agent-task-candidate/v1", "state": "no_changes_produced",
            "scan": { "attempts_omitted": 1, "outcomes_omitted": 2, "artifacts_omitted": 3 },
        }}));
        assert!(result.is_degraded());
        assert_eq!(result.state(), CandidateState::Unknown);
    }
}
