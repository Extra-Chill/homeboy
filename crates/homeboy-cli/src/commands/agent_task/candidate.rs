use homeboy::core::{Error, Result};
use serde_json::Value;
use std::path::Path;

#[derive(clap::Subcommand, Debug)]
pub enum CandidateCommand {
    /// Fetch the selected candidate and check out the Cook's head branch.
    Checkout {
        /// Cook id or attempt run id whose candidate to retrieve.
        cook_or_attempt: String,
        /// Clean git worktree to fetch into and check out the Cook's head branch.
        #[arg(long)]
        into: std::path::PathBuf,
    },
}

fn git(path: &Path, args: &[&str]) -> Result<String> {
    git_operation(path, args, "candidate", args[0])
}

fn git_operation(path: &Path, args: &[&str], field: &str, operation: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|e| {
            Error::validation_invalid_argument(
                field,
                format!("{operation} at {}: {e}", path.display()),
                None,
                None,
            )
        })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            field,
            format!(
                "{operation} at {}: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
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
    if into.exists() {
        ensure_clean(&into)?;
    }
    let target = super::status::resolve_cook_reader_target(&cook_or_attempt, false)?;
    let store = homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    let record = homeboy::agents::agent_task_lifecycle::exact_record_bounded_in_store(
        &store,
        &target.run_id,
    )?;
    let recipe = homeboy::agents::agent_task_service::load_recipe_for_attempt(&target.run_id)?;
    let title =
        homeboy::agents::agent_task_service::candidate_publication_title(&store, &target.run_id)?;
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
    if !into.exists() {
        let source = resolve_source_checkout(
            &into,
            &record.metadata,
            recipe
                .as_ref()
                .and_then(|r| r.finalization["source_worktree_path"].as_str()),
            recipe.as_ref().and_then(|r| {
                r.attempts
                    .iter()
                    .find(|a| a.run_id == target.run_id)
                    .and_then(|a| a.plan.metadata["repo"].as_str())
            }),
        )?;
        homeboy::core::worktree::create_at(
            homeboy::core::worktree::WorktreeCreateOptions {
                component_id: source.to_string_lossy().into_owned(),
                source_path: Some(source.to_string_lossy().into_owned()),
                branch: format!("homeboy-checkout/{}", target.run_id),
                from: Some("HEAD".into()),
                task_url: None,
                run_id: None,
                cleanup_policy: None,
                require_handoff_freshness: false,
            },
            &into,
        )?;
    }
    let gated_sha = record
        .candidate
        .as_ref()
        .map(|c| c.candidate_sha.as_str())
        .or_else(|| {
            promotion
                .pointer("/candidate/candidate_sha")
                .and_then(Value::as_str)
        });
    let cook = recipe
        .as_ref()
        .map(|r| r.cook_id.as_str())
        .or_else(|| record.metadata["cook_id"].as_str())
        .unwrap_or(&cook_or_attempt);
    let reference = format!("refs/homeboy/candidates/{cook}");
    if let Some(source) = promotion.pointer("/target/path").and_then(Value::as_str) {
        if git(Path::new(source), &["show-ref", "--verify", &reference]).is_ok() {
            return checkout_from(&into, source, &reference, branch, &title, gated_sha);
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
    checkout_from(
        &into,
        &fetched.output_path.to_string_lossy(),
        &sha,
        branch,
        &title,
        gated_sha.or(Some(&sha)),
    )
}

/// Resolve only the repository used to create a destination; candidate fetching
/// continues to use the canonical ref or retained bundle independently.
fn resolve_source_checkout(
    into: &Path,
    metadata: &Value,
    recipe_source: Option<&str>,
    repo_component: Option<&str>,
) -> Result<std::path::PathBuf> {
    let mut tried = Vec::new();
    let mut usable = |path: &Path| {
        tried.push(path.display().to_string());
        path.is_dir() && git(path, &["rev-parse", "--show-toplevel"]).is_ok()
    };
    for source in [
        metadata
            .pointer("/cook_delivery/artifacts/promotion/target/path")
            .and_then(Value::as_str),
        recipe_source,
    ]
    .into_iter()
    .flatten()
    {
        let path = Path::new(source);
        if usable(path) {
            return Ok(path.to_path_buf());
        }
    }
    if let Some(source) = metadata
        .pointer("/source_checkout/local_path")
        .and_then(Value::as_str)
    {
        let path = Path::new(source);
        if usable(path) {
            if let Ok(common) = git(
                path,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            ) {
                if let Some(main) = Path::new(&common).parent() {
                    if usable(main) {
                        return Ok(main.to_path_buf());
                    }
                }
            }
        }
    }
    let component_id = metadata
        .pointer("/cook_repository_identity/component_id")
        .and_then(Value::as_str)
        .or_else(|| metadata["repo"].as_str())
        .or(repo_component);
    let mut registration_error = None;
    if let Some(id) = component_id {
        match homeboy::core::component::load(id) {
            Ok(component) => {
                let path = Path::new(&component.local_path);
                if usable(path) {
                    return Ok(path.to_path_buf());
                }
            }
            Err(error) => registration_error = Some(format!("; component {id}: {error}")),
        }
    }
    Err(Error::validation_invalid_argument(
        "into",
        format!(
            "cannot create {}: Cook repository is unavailable; tried paths: {}{}",
            into.display(),
            if tried.is_empty() {
                "(none)".into()
            } else {
                tried.join(", ")
            },
            registration_error.unwrap_or_default(),
        ),
        None,
        None,
    ))
}

fn ensure_clean(into: &Path) -> Result<()> {
    let root = git_operation(
        into,
        &["rev-parse", "--show-toplevel"],
        "into",
        "validate worktree",
    )?;
    if into.canonicalize().ok() != Path::new(&root).canonicalize().ok() {
        return Err(Error::validation_invalid_argument(
            "into",
            format!("{} is not a git worktree root", into.display()),
            None,
            None,
        ));
    }
    if !git_operation(
        into,
        &["status", "--porcelain", "--untracked-files=all"],
        "into",
        "inspect worktree",
    )?
    .is_empty()
    {
        return Err(Error::validation_invalid_argument(
            "into",
            format!("worktree {} has uncommitted changes", into.display()),
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
    title: &str,
    gated_sha: Option<&str>,
) -> crate::commands::CmdResult<Value> {
    ensure_clean(into)?;
    git_operation(
        into,
        &["check-ref-format", "--branch", branch],
        "candidate",
        "validate branch",
    )?;
    git_operation(
        into,
        &["fetch", "--no-tags", "--", source, revision],
        "candidate",
        "fetch candidate",
    )?;
    let fetched = git(into, &["rev-parse", "FETCH_HEAD"])?;
    let gated_sha = gated_sha.unwrap_or(&fetched);
    if git(
        into,
        &["cat-file", "-e", &format!("{gated_sha}^{{commit}}")],
    )
    .is_err()
    {
        git_operation(
            into,
            &["fetch", "--no-tags", "--", source, gated_sha],
            "candidate",
            "fetch gated candidate",
        )?;
    }
    let tree = git(into, &["rev-parse", &format!("{gated_sha}^{{tree}}")])?;
    let sha = homeboy::core::git::publication_candidate(&into.to_string_lossy(), &fetched, title)?;
    let publication_tree = git(into, &["rev-parse", &format!("{sha}^{{tree}}")])?;
    if publication_tree != tree {
        return Err(Error::validation_invalid_argument(
            "candidate",
            "publication commit tree differs from gated tree",
            None,
            None,
        ));
    }
    let path = into
        .canonicalize()
        .map_err(|e| Error::internal_io(e.to_string(), None))?;
    // Git refuses to move a branch another worktree has checked out. That
    // worktree keeps its branch; this one gets the candidate detached.
    let branch_owner = other_worktree_on_branch(into, &path, branch)?;
    if branch_owner.is_some() {
        git_operation(
            into,
            &["checkout", "--detach", &sha, "--"],
            "into",
            "checkout branch",
        )?;
    } else {
        git_operation(
            into,
            &["checkout", "-B", branch, &sha, "--"],
            "into",
            "checkout branch",
        )?;
    }
    Ok((
        serde_json::json!({"command": "agent-task.candidate.checkout", "candidate_sha": sha, "gated_sha": gated_sha, "publication_sha": sha, "tree": tree, "identity_note": "The publication commit carries the gated tree unchanged.", "branch": branch, "detached": branch_owner.is_some(), "branch_checked_out_in": branch_owner, "worktree": path}),
        0,
    ))
}

fn other_worktree_on_branch(into: &Path, own: &Path, branch: &str) -> Result<Option<String>> {
    let listing = git(into, &["worktree", "list", "--porcelain"])?;
    let wanted = format!("branch refs/heads/{branch}");
    Ok(listing.split("\n\n").find_map(|entry| {
        let path = entry
            .lines()
            .find_map(|line| line.strip_prefix("worktree "))?;
        let same = Path::new(path).canonicalize().ok().as_deref() == Some(own);
        (!same && entry.lines().any(|line| line == wanted)).then(|| path.to_string())
    }))
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
        checkout_fixture(false, None);
    }

    #[test]
    fn checkout_fetches_bundle_using_candidate_metadata() {
        checkout_fixture(true, None);
    }

    #[test]
    fn candidate_removed_recorded_worktree_falls_back_to_registered_checkout() {
        checkout_fixture(true, Some("removed"));
    }

    #[test]
    fn candidate_without_promotion_still_checks_out() {
        checkout_fixture(true, Some("unpromoted"));
    }

    #[test]
    fn candidate_no_usable_source_names_attempted_paths() {
        checkout_fixture(true, Some("unavailable"));
    }

    fn checkout_fixture(bundle: bool, fallback: Option<&str>) {
        use crate::commands::agent_task::tests::support::{test_plan, with_isolated_home};
        with_isolated_home(|home| {
            let source = home.path().join("source");
            let destination = home.path().join("destination");
            std::fs::create_dir(&source).unwrap();
            git(&source, &["init", "-b", "main"]).unwrap();
            git(
                &source,
                &["remote", "add", "origin", "https://example.test/repo.git"],
            )
            .unwrap();
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
            git(
                &destination,
                &[
                    "remote",
                    "set-url",
                    "origin",
                    "https://example.test/repo.git",
                ],
            )
            .unwrap();
            git(&destination, &["config", "user.name", "Operator"]).unwrap();
            git(
                &destination,
                &["config", "user.email", "operator@example.test"],
            )
            .unwrap();
            std::fs::write(source.join("file"), "candidate").unwrap();
            git(
                &source,
                &[
                    "-c",
                    "user.name=Homeboy",
                    "-c",
                    "user.email=homeboy@localhost",
                    "commit",
                    "-am",
                    "homeboy: cook candidate",
                ],
            )
            .unwrap();
            let sha = git(&source, &["rev-parse", "HEAD"]).unwrap();
            let run_id = "checkout-attempt";
            let lifecycle =
                homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()
                    .unwrap();
            use homeboy::agents::agent_task_service::*;
            let options = CookRequest {
                identity: CookIdentity { cook_id: "checkout-cook".into(), initial_run_id: run_id.into(), initial_plan: test_plan() },
                workspace: CookWorkspace { to_worktree: "fixture@candidate".into(), source_worktree_path: None, task_base_sha: None, source_refs: vec![] },
                provider_transport: CookProviderTransport { provider_invocation: None, attempt_dispatcher: None },
                gates: Default::default(),
                retry_policy: CookRetryPolicy { max_attempts: 1 },
                finalization: CookFinalization { publish_incomplete_draft: false, tracker_note: true, no_finalize: true, draft_pr: false, base: "main".into(), head: Some("cook/head".into()), title: "fix: publish useful candidates".into(), commit_message: "fix: publish useful candidates".into(), protected_branches: vec![] },
                harvest_context: homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process().unwrap(),
            };
            persist_initial_recipe(&options).unwrap();
            let mut plan = test_plan();
            plan.metadata["cook_id"] = json!("checkout-cook");
            plan.metadata["cook_attempt"] = json!(1);
            homeboy::agents::agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
            homeboy::agents::agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                record.set_state_for_test(homeboy_lifecycle_contract::RunExecutionState::Succeeded);
                record.metadata = json!({"cook_id": "checkout-cook", "cook_delivery": {"artifacts": {"promotion": {
                    "target": {"path": source, "branch": "cook/head"}
                }, "candidate": {"candidate_sha": sha, "tree_sha": git(&source, &["rev-parse", "HEAD^{tree}"]).unwrap()}}}});
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
            if let Some(case) = fallback {
                let removed = home.path().join("removed-worktree");
                git(
                    &source,
                    &["worktree", "add", "--detach", removed.to_str().unwrap()],
                )
                .unwrap();
                git(&source, &["worktree", "remove", removed.to_str().unwrap()]).unwrap();
                let invalid = home.path().join("not-a-checkout");
                std::fs::create_dir(&invalid).unwrap();
                let registered = if case == "unavailable" {
                    &invalid
                } else {
                    &source
                };
                let component: homeboy::core::component::Component =
                    serde_json::from_value(json!({
                        "id": "checkout-component", "local_path": registered,
                    }))
                    .unwrap();
                homeboy::core::component::inventory::write_standalone_registration(&component)
                    .unwrap();
                homeboy::agents::agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
                    record.metadata["source_checkout"] = json!({"local_path": removed});
                    record.metadata["cook_repository_identity"] =
                        json!({"component_id": "checkout-component"});
                    if case == "unpromoted" {
                        record.set_state_for_test(
                            homeboy_lifecycle_contract::RunExecutionState::Failed,
                        );
                        record.metadata["cook_delivery"]["artifacts"]
                            .as_object_mut()
                            .unwrap()
                            .remove("promotion");
                    } else {
                        record.metadata["cook_delivery"]["artifacts"]["promotion"]["target"]
                            ["path"] = json!(removed);
                    }
                })
                .unwrap();
                // Persist the removed path in the recipe as well.
                let mut options = options.clone();
                options.workspace.source_worktree_path = Some(removed.clone());
                persist_initial_recipe(&options).unwrap();
                let into = home.path().join("fallback-destination");
                let result = run(CandidateCommand::Checkout {
                    cook_or_attempt: "checkout-cook".into(),
                    into: into.clone(),
                });
                if case == "unavailable" {
                    let error = result.unwrap_err().to_string();
                    for path in [&removed, &invalid, &into] {
                        assert!(error.contains(path.to_str().unwrap()), "{error}");
                    }
                } else {
                    let (value, _) = result.unwrap();
                    assert_eq!(value["gated_sha"], sha);
                    assert_eq!(
                        std::fs::read_to_string(into.join("file")).unwrap(),
                        "candidate"
                    );
                    assert_eq!(
                        git(&into, &["rev-parse", "HEAD^{tree}"]).unwrap(),
                        git(&source, &["rev-parse", "HEAD^{tree}"]).unwrap()
                    );
                }
                return;
            }
            if !bundle {
                for case in ["missing", "dirty", "non-git"] {
                    let into = home.path().join(case);
                    if case == "dirty" {
                        git(
                            home.path(),
                            &["clone", source.to_str().unwrap(), into.to_str().unwrap()],
                        )
                        .unwrap();
                        std::fs::write(into.join("untracked"), "dirty").unwrap();
                    } else if case == "non-git" {
                        std::fs::create_dir(&into).unwrap();
                    }
                    let result = run(CandidateCommand::Checkout {
                        cook_or_attempt: "checkout-cook".into(),
                        into: into.clone(),
                    });
                    if case == "missing" {
                        let (value, _) = result.unwrap();
                        assert_eq!(value["gated_sha"], sha);
                        assert_eq!(
                            value["publication_sha"],
                            git(&into, &["rev-parse", "HEAD"]).unwrap()
                        );
                        assert_eq!(
                            value["tree"],
                            git(&source, &["rev-parse", "HEAD^{tree}"]).unwrap()
                        );
                        assert_eq!(
                            value["tree"],
                            git(&into, &["rev-parse", "HEAD^{tree}"]).unwrap()
                        );
                    } else {
                        let error = result.unwrap_err().to_string();
                        assert!(error.contains("into"), "{error}");
                        assert!(error.contains(into.to_str().unwrap()), "{error}");
                    }
                }
            }
            // A stale worktree may still hold the Cook head branch (bundle case).
            let stale = home.path().join("stale-head");
            if bundle {
                git(
                    &destination,
                    &[
                        "worktree",
                        "add",
                        "-b",
                        "cook/head",
                        stale.to_str().unwrap(),
                    ],
                )
                .unwrap();
            }
            let (value, _) = run(CandidateCommand::Checkout {
                cook_or_attempt: if bundle { run_id } else { "checkout-cook" }.into(),
                into: destination.clone(),
            })
            .unwrap();
            assert_eq!(value["detached"], bundle);
            let published_sha = git(&destination, &["rev-parse", "HEAD"]).unwrap();
            assert_eq!(value["candidate_sha"], published_sha);
            assert_eq!(git(&destination, &["show", "-s", "--format=%an <%ae>|%cn <%ce>|%s", "HEAD"]).unwrap(), "Operator <operator@example.test>|Operator <operator@example.test>|fix: publish useful candidates");
            assert_eq!(
                git(&destination, &["rev-parse", "HEAD^{tree}"]).unwrap(),
                git(&source, &["rev-parse", "HEAD^{tree}"]).unwrap()
            );
            assert_eq!(
                git(&destination, &["rev-parse", "HEAD^"]).unwrap(),
                git(&source, &["rev-parse", "HEAD^"]).unwrap()
            );
            assert_eq!(
                git(&destination, &["branch", "--show-current"]).unwrap(),
                if bundle { "" } else { "cook/head" }
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
            assert_eq!(
                git(&destination, &["rev-parse", "HEAD"]).unwrap(),
                published_sha
            );
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
