use serde_json::json;

use crate::agent_task_promotion::AgentTaskPromotionReport;
use crate::agent_task_review_dossier::{
    enrich_dossier, render_review_dossier, AgentTaskReviewDossier, AgentTaskReviewProfile,
};
use homeboy_core::error::{Error, Result};
use homeboy_core::gate::{HomeboyGateKind, HomeboyGateResult, HomeboyGateStatus};
use homeboy_core::proof::HomeboyProof;
use homeboy_core::run_lifecycle_record::{
    ProviderRuntimeState, RunExecutionState, RunLifecycleRecord,
};

pub const AGENT_TASK_PR_FINALIZATION_SCHEMA: &str = "homeboy/agent-task-pr-finalization/v1";
pub const AGENT_TASK_PR_FINALIZATION_OUTCOME_SCHEMA: &str =
    "homeboy/agent-task-pr-finalization-outcome/v1";
pub const AGENT_TASK_PUBLICATION_INTENT_SCHEMA: &str = "homeboy/agent-task-publication-intent/v1";
pub const AGENT_TASK_PUBLICATION_PROOF_SCHEMA: &str = "homeboy/agent-task-publication-proof/v1";

mod backend;
mod proof;
mod schemas;

pub use backend::RealAgentTaskPrFinalizationBackend;
use schemas::{finalization_outcome_schema, publication_intent_schema, publication_proof_schema};
mod types;
pub use types::*;

pub fn finalize_pr(
    options: AgentTaskPrFinalizationOptions,
) -> Result<AgentTaskPrFinalizationReport> {
    finalize_pr_with_backend(
        options,
        &mut RealAgentTaskPrFinalizationBackend,
        &crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?,
    )
}

/// Validate a finalization dossier and its durable candidate without mutation.
pub fn preflight_pr(
    options: AgentTaskPrFinalizationOptions,
) -> Result<AgentTaskPrFinalizationReport> {
    preflight_pr_with_backend(
        options,
        &mut RealAgentTaskPrFinalizationBackend,
        &crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?,
    )
}

/// Hydrate dependencies for fresh manual gates in their immutable checkout:
/// the root plus the descendant roots `commands` name (#16857).
pub fn hydrate_manual_verification_dependencies(
    checkout: &std::path::Path,
    commands: &[String],
) -> Result<Vec<AgentTaskGateSetupEvidence>> {
    crate::agent_task_gate::hydrate_gate_dependency_roots_for_component(
        checkout,
        true,
        "manual_finalization_checkout",
        &homeboy_core::deps::DependencyHydrationPolicy::default(),
        None,
        Some(commands),
    )
}

pub fn finalize_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    options: AgentTaskPrFinalizationOptions,
    backend: &mut B,
    lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
) -> Result<AgentTaskPrFinalizationReport> {
    finalize_pr_with_backend_mode(options, backend, true, lifecycle_store)
}

pub fn preflight_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    options: AgentTaskPrFinalizationOptions,
    backend: &mut B,
    lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
) -> Result<AgentTaskPrFinalizationReport> {
    finalize_pr_with_backend_mode(options, backend, false, lifecycle_store)
}

/// Update an open PR for this candidate, marking a draft ready when the
/// publication is not itself a draft.
fn update_existing_pr<B: AgentTaskPrFinalizationBackend>(
    backend: &mut B,
    options: &mut AgentTaskPrFinalizationOptions,
    existing: &AgentTaskPrRef,
    ready_transitioned: &mut bool,
) -> Result<(&'static str, AgentTaskPrRef)> {
    let existing_body = backend.open_pr_body(
        options.component_id.as_deref(),
        &options.path,
        existing.number,
    )?;
    crate::agent_task_review_dossier::reconcile_existing_pr_relationships(
        &mut options.review_dossier.source_relationships,
        &existing.url,
        existing_body.as_deref().unwrap_or_default(),
    );
    let body = render_review_dossier(&options.review_dossier, &options.review_profile);
    let updated = backend.update_pr(
        options.component_id.as_deref(),
        &options.path,
        existing.number,
        &options.title,
        &body,
    )?;
    if existing.is_draft && !options.draft_pr {
        *ready_transitioned = true;
        Ok(("ready", backend.mark_pr_ready(&options.path, &updated)?))
    } else {
        Ok(("updated", updated))
    }
}

/// `gh pr create` refused because an open PR already exists for this head.
fn is_existing_pr_conflict(message: &str) -> bool {
    message.contains("a pull request for branch") && message.contains("already exists")
}

/// Look the PR up again after a create conflict, giving GitHub's PR list a
/// moment to catch up with a PR created seconds ago.
fn find_open_pr_after_create_conflict<B: AgentTaskPrFinalizationBackend>(
    backend: &mut B,
    component_id: Option<&str>,
    path: &str,
    base: &str,
    head: &str,
) -> Result<Option<AgentTaskPrRef>> {
    const ATTEMPTS: u32 = 5;
    for attempt in 0..ATTEMPTS {
        if let Some(pr) = backend.find_open_pr(component_id, path, base, head)? {
            return Ok(Some(pr));
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    }
    Ok(None)
}

fn finalize_pr_with_backend_mode<B: AgentTaskPrFinalizationBackend>(
    mut options: AgentTaskPrFinalizationOptions,
    backend: &mut B,
    publish: bool,
    lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
) -> Result<AgentTaskPrFinalizationReport> {
    let (publication_identity, _requester) =
        backend.publication_context(&options.run_id, lifecycle_store)?;
    // Recorded before preflight so the validated intent and the published
    // receipt carry the same dossier (manual receipts must match exactly).
    options.review_dossier.evidence.push(
        crate::agent_task_review_dossier::AgentTaskReviewEvidence {
            summary: format!("Publication identity: {publication_identity}"),
            url: None,
        },
    );
    let durable_acceptance;
    if options.manual_finalization {
        options.review_dossier.ai_assistance.model =
            crate::execution_provenance::render_execution_provenance(&Default::default());
        durable_acceptance = validate_manual_finalization_policy(lifecycle_store, &options.run_id)?;
        if options.inherited_gate_evidence.is_some()
            || (options.normalized_gate_results.is_empty()
                && options.verified_candidate_tree.is_none())
        {
            let claimed_evidence = options.inherited_gate_evidence.take();
            let inherited =
                backend.hydrate_optional_gate_proof_in_store(lifecycle_store, &options.run_id)?;
            if let Some(gate_proof) = inherited {
                inherit_promotion_gates(&mut options, gate_proof)?;
                if claimed_evidence.is_some()
                    && claimed_evidence.as_ref() != options.inherited_gate_evidence.as_ref()
                {
                    return Err(Error::validation_invalid_argument(
                        "inherited_gate_evidence",
                        "persisted inherited gate evidence no longer matches the current durable promotion receipt; run fresh verification",
                        None,
                        None,
                    ));
                }
            } else if claimed_evidence.is_some() {
                return Err(Error::validation_invalid_argument(
                    "inherited_gate_evidence",
                    "persisted inherited gate evidence has no current durable promotion receipt; run fresh verification",
                    None,
                    None,
                ));
            }
        }
    } else {
        let lifecycle = backend.hydrate_run_in_store(lifecycle_store, &options.run_id)?;
        let gate_proof = backend.hydrate_gate_proof_in_store(lifecycle_store, &options.run_id)?;
        if gate_proof.run_id != options.run_id {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "durable gate proof belongs to a different run",
                None,
                None,
            ));
        }
        validate_gate_proof_binding(&gate_proof, &options)?;
        durable_acceptance =
            validate_durable_acceptance(&options.run_id, &gate_proof.promotion, lifecycle_store)?;
        validate_durable_publication_eligibility(&lifecycle, &gate_proof.promotion)?;
        let durable_changed_files = normalize_changed_files(&gate_proof.promotion.changed_files);
        if normalize_changed_files(&options.changed_files) != durable_changed_files {
            return Err(changed_files_mismatch_error(
                &options.run_id,
                &options.changed_files,
                &gate_proof.promotion.changed_files,
            ));
        }
        let gated = gate_proof.promotion.candidate.clone().ok_or_else(|| {
            Error::validation_invalid_argument(
                "candidate",
                "promotion has no gated candidate",
                None,
                None,
            )
        })?;
        if options
            .candidate
            .as_ref()
            .is_some_and(|candidate| candidate.tree_sha != gated.tree_sha)
        {
            return Err(Error::validation_invalid_argument(
                "candidate",
                "publication tree differs from gated tree",
                None,
                None,
            ));
        }
        options.candidate = Some(gated);
        options.normalized_gate_results = gate_proof.promotion.gate_results;
        if options.normalized_gate_results.is_empty() {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "durable gate proof contains no normalized deterministic gates",
                None,
                None,
            ));
        }
        options.evidence.lifecycle = Some(lifecycle);
    }
    validate_finalization_gates(&options.normalized_gate_results)?;
    options.review_dossier.apply_overrides()?;
    let current_branch = backend.current_branch(&options.path)?;
    let head = options
        .head
        .clone()
        .unwrap_or_else(|| current_branch.clone());
    if head != current_branch {
        return Err(Error::validation_invalid_argument(
            "head",
            "requested head does not match the checked-out branch; check out the requested branch before finalizing",
            Some(head),
            None,
        ));
    }
    refuse_protected_head(&head, &options.protected_branches)?;

    let verified_base_sha = options.verified_base_sha.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "verified_base_sha",
            "finalization requires the immutable base SHA recorded before declared gates ran",
            None,
            None,
        )
    })?;
    let base = backend.resolve_verified_base(&options.path, verified_base_sha)?;
    let (candidate, mut changed_files) = match options.candidate.clone() {
        Some(candidate) => (candidate, options.changed_files.clone()),
        None => backend.prepare_manual_candidate(&options.path, &base.sha)?,
    };
    for expected in [
        &options.expected_candidate_tree,
        &options.verified_candidate_tree,
    ]
    .into_iter()
    .flatten()
    {
        if expected != &candidate.tree_sha {
            return Err(Error::validation_invalid_argument(
                "candidate",
                "candidate tree changed after verification",
                None,
                None,
            ));
        }
    }
    changed_files.sort();
    changed_files.dedup();
    enrich_dossier(
        &mut options.review_dossier,
        &options.evidence.artifact_refs,
        &options.normalized_gate_results,
    );
    options.review_dossier.verified_base = Some(
        crate::agent_task_review_dossier::AgentTaskReviewVerifiedBase {
            branch: options.base.clone(),
            sha: base.sha.clone(),
            publication_sha: None,
        },
    );
    options.review_dossier.evidence.sort_by(|left, right| {
        left.summary
            .cmp(&right.summary)
            .then(left.url.cmp(&right.url))
    });
    options.review_dossier.evidence.dedup();
    options
        .review_dossier
        .validate_preflight_fields(&options.review_profile)?;
    options.review_dossier.validate(&options.review_profile)?;
    let proof = build_finalization_proof(&options, options.normalized_gate_results.clone());
    let mut intent =
        build_pr_publication_intent(&options, &head, &changed_files, proof.clone(), &base);
    intent.target.publication_route = backend.publication_route(&options.path)?;
    validate_publication_intent(&intent)?;

    if changed_files.is_empty() {
        return Ok(report(
            &options,
            intent,
            &head,
            "no_changes",
            "none",
            None,
            None,
            changed_files,
            Some(proof),
            false,
            false,
            None,
            None,
            None,
            durable_acceptance,
        ));
    }

    let mut git_identity = backend.validate_publication_identity(&options.path)?;
    if !publish {
        git_identity.commit_sha = Some(candidate.candidate_sha.clone());
        return Ok(report(
            &options,
            intent,
            &head,
            "validated",
            "none",
            None,
            None,
            changed_files.clone(),
            Some(proof),
            false,
            false,
            Some(git_identity),
            None,
            Some(AgentTaskPublicationBinding {
                candidate_sha: candidate.candidate_sha.clone(),
                candidate_tree: candidate.tree_sha.clone(),
                remote_sha: String::new(),
                pr_head_sha: String::new(),
                repository: String::new(),
                head_repository: String::new(),
                changed_files: changed_files.clone(),
            }),
            durable_acceptance,
        ));
    }
    let dates = backend.candidate_dates(&options.path, &candidate.candidate_sha)?;
    let published_sha = backend.publish_tree(
        &options.path,
        &candidate,
        &base.sha,
        &options.commit_message,
        &dates,
    )?;
    let commit_sha = published_sha.as_str();
    git_identity.commit_sha = Some(published_sha.clone());
    let git_tracking = Some(backend.push_branch(&options.path, commit_sha, &head)?);
    let existing = backend.find_open_pr(
        options.component_id.as_deref(),
        &options.path,
        &options.base,
        &head,
    )?;
    if let (Some(form_title), Some(pr)) = (&options.cook_form_title, &existing) {
        if let Some(existing_title) =
            backend.open_pr_title(options.component_id.as_deref(), &options.path, pr.number)?
        {
            if !existing_title.trim().is_empty()
                && !existing_title.starts_with("Cook ")
                && existing_title != *form_title
            {
                options.title = existing_title;
            }
        }
    }
    let publication_base_sha = backend.publication_base_sha(&options.path, &options.base)?;
    intent.target.publication_base_sha = publication_base_sha.clone();
    if let Some(verified_base) = options.review_dossier.verified_base.as_mut() {
        verified_base.publication_sha = publication_base_sha;
    }
    let body = render_review_dossier(&options.review_dossier, &options.review_profile);
    let mut newly_created = existing.is_none();
    let mut draft = existing.as_ref().map_or(options.draft_pr, |pr| pr.is_draft);
    let mut ready_transitioned = false;
    let (action, pr) = match existing {
        Some(existing) => {
            update_existing_pr(backend, &mut options, &existing, &mut ready_transitioned)?
        }
        None => match backend.create_pr(
            options.component_id.as_deref(),
            &options.path,
            &options.base,
            &head,
            &options.title,
            &body,
            options.draft_pr,
        ) {
            Ok(created) => ("created", created),
            // A concurrent publication of this same Cook can open the PR
            // between our lookup and our create: GitHub's PR list is briefly
            // eventually consistent, so the lookup misses a seconds-old PR
            // (#15482). Adopt it as an existing PR. It is classified as
            // existing (never closed on drift), and the publication binding
            // below still requires its head to be this candidate commit.
            Err(error) if is_existing_pr_conflict(&error.message) => {
                let Some(adopted) = find_open_pr_after_create_conflict(
                    backend,
                    options.component_id.as_deref(),
                    &options.path,
                    &options.base,
                    &head,
                )?
                else {
                    return Err(error);
                };
                newly_created = false;
                draft = adopted.is_draft;
                update_existing_pr(backend, &mut options, &adopted, &mut ready_transitioned)?
            }
            Err(error) => return Err(error),
        },
    };
    let published_draft = if ready_transitioned {
        false
    } else if newly_created {
        options.draft_pr
    } else {
        draft
    };

    let binding = read_publication_head(&options.run_id, commit_sha, || {
        backend.verify_publication_binding(
            &options.path,
            &options.base,
            &head,
            commit_sha,
            &changed_files,
            &pr,
        )
    })?;

    Ok(report(
        &options,
        intent,
        &head,
        if published_draft {
            "draft_published"
        } else {
            "review_ready"
        },
        action,
        Some(pr.number),
        Some(pr.url),
        changed_files,
        Some(proof),
        true,
        true,
        Some(git_identity),
        git_tracking,
        Some(binding),
        durable_acceptance,
    ))
}

/// Manual finalization deliberately has no promotion lineage. It still cannot
/// replace an acceptance decision that the durable run's policy requires.
fn validate_manual_finalization_policy(
    lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
    run_id: &str,
) -> Result<Option<crate::agent_task_lifecycle::AgentTaskAcceptanceRecord>> {
    let record = match lifecycle_store.read_record(run_id) {
        Ok(record) => record,
        Err(error) if error.message.contains("agent-task run record not found") => return Ok(None),
        Err(error) => return Err(error),
    };
    if record.metadata.get("acceptance_requirement").is_some() || record.acceptance.is_some() {
        return Err(Error::validation_invalid_argument(
            "acceptance",
            "awaiting_acceptance: manual finalization cannot replace a required durable acceptance decision; record acceptance for the corrected candidate first",
            Some(run_id.to_string()),
            None,
        ));
    }
    Ok(None)
}

fn validate_durable_acceptance(
    run_id: &str,
    promotion: &AgentTaskPromotionReport,
    lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
) -> Result<Option<crate::agent_task_lifecycle::AgentTaskAcceptanceRecord>> {
    let record = match lifecycle_store.read_record(run_id) {
        Ok(record) => record,
        // Durable proofs created before acceptance existed have no lifecycle
        // record to carry an acceptance requirement. Preserve that established
        // compatibility case, but propagate every readable-record failure.
        Err(error) if error.message.contains("agent-task run record not found") => return Ok(None),
        Err(error) => return Err(error),
    };
    let Some(acceptance) = record.acceptance else {
        if record.metadata.get("acceptance_requirement").is_some() {
            return Err(Error::validation_invalid_argument(
                "acceptance",
                "awaiting_acceptance: finalization requires a durable acceptance record after applied promotion",
                None,
                None,
            ));
        }
        return Ok(None);
    };
    let candidate = promotion
        .provenance
        .pointer("/candidate/fingerprint")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .filter(
            |candidate: &crate::agent_task_promotion::AgentTaskCandidateFingerprint| {
                !candidate.schema.trim().is_empty()
                    && !candidate.target_path.trim().is_empty()
                    && !candidate.head.trim().is_empty()
                    && !candidate.tree.trim().is_empty()
                    && !candidate.tree.trim().is_empty()
            },
        )
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "acceptance",
                "awaiting_acceptance: finalization requires a complete candidate fingerprint",
                None,
                None,
            )
        })?;
    let base_sha = promotion
        .verified_base
        .as_ref()
        .map(|base| base.sha.as_str())
        .unwrap_or_default();
    if acceptance.verdict != crate::agent_task_lifecycle::AgentTaskAcceptanceVerdict::Accepted
        || !acceptance.matches_candidate(&candidate, base_sha)
    {
        return Err(Error::validation_invalid_argument("acceptance", "awaiting_acceptance: finalization requires an accepted durable verdict bound to the current candidate and verified base", None, None));
    }
    let attestation = acceptance.attestation.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "acceptance",
            "accepted durable verdict has no canonical signed attestation",
            None,
            None,
        )
    })?;
    let request = crate::agent_task_lifecycle::AgentTaskAcceptanceVerificationRequest {
        requirement: acceptance.requirement.clone(),
        verdict: acceptance.verdict,
        candidate: acceptance.candidate.clone(),
        base_sha: acceptance.base_sha.clone(),
        evidence_refs: acceptance.evidence_refs.clone(),
        token: String::new(),
    };
    crate::agent_task_lifecycle::revalidate_durable_attestation(&request, attestation)?;
    Ok(Some(acceptance))
}

pub(crate) fn normalize_changed_files(changed_files: &[String]) -> Vec<String> {
    let mut normalized = changed_files.to_vec();
    normalized.sort();
    normalized.dedup();
    normalized
}

/// Maximum number of individual paths listed in a changed-file mismatch
/// diagnostic before the remainder is summarized as an omitted count. Keeps the
/// fail-closed error bounded on large divergences while still naming the paths
/// an operator needs to resolve the common small cases (#9870).
const CHANGED_FILE_DIAGNOSTIC_PATH_LIMIT: usize = 20;

/// Build a self-diagnosing changed-file mismatch error for finalization.
///
/// Instead of only stating the invariant, this reports the expected/actual
/// counts, the paths `missing_from_caller` (expected but not supplied) and
/// `unexpected_from_caller` (supplied but not expected), each bounded with an
/// omitted count, and an exact inspection command for the full sets. This makes
/// the fail-closed error self-diagnosing regardless of whether the root cause is
/// a caller typo, a stale promotion scope, or genuine Git candidate divergence
/// (#9870).
pub(crate) fn changed_files_mismatch_error(
    run_id: &str,
    caller_changed_files: &[String],
    expected_changed_files: &[String],
) -> Error {
    let caller = normalize_changed_files(caller_changed_files);
    let expected = normalize_changed_files(expected_changed_files);
    let missing_from_caller: Vec<String> = expected
        .iter()
        .filter(|path| !caller.contains(*path))
        .cloned()
        .collect();
    let unexpected_from_caller: Vec<String> = caller
        .iter()
        .filter(|path| !expected.contains(*path))
        .cloned()
        .collect();

    let problem = format!(
        "caller changed files must exactly match the persisted promotion report before \
         finalization. expected_count={} actual_count={}; missing_from_caller={}; \
         unexpected_from_caller={}. Inspect the full recorded set with \
         `homeboy agent-task diagnose {run_id} --full` (promotion.changed_files). A caller \
         typo lists an unexpected path; a stale promotion scope inflates the expected set \
         beyond the current PR diff (see #9706 for adoption scope).",
        expected.len(),
        caller.len(),
        bounded_path_summary(&missing_from_caller),
        bounded_path_summary(&unexpected_from_caller),
    );

    Error::validation_invalid_argument("changed-file", problem, None, None)
}

/// Render a bounded, human-readable summary of a path set: up to
/// [`CHANGED_FILE_DIAGNOSTIC_PATH_LIMIT`] paths followed by an omitted count.
fn bounded_path_summary(paths: &[String]) -> String {
    if paths.is_empty() {
        return "[]".to_string();
    }
    let shown: Vec<&String> = paths
        .iter()
        .take(CHANGED_FILE_DIAGNOSTIC_PATH_LIMIT)
        .collect();
    let omitted = paths.len().saturating_sub(shown.len());
    let joined = shown
        .iter()
        .map(|path| path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    if omitted > 0 {
        format!("[{joined}, +{omitted} more]")
    } else {
        format!("[{joined}]")
    }
}

fn validate_gate_proof_binding(
    gate_proof: &AgentTaskPrDurableGateProof,
    options: &AgentTaskPrFinalizationOptions,
) -> Result<()> {
    use crate::agent_task_promotion::AgentTaskPromotionStatus;
    if gate_proof.promotion.status != AgentTaskPromotionStatus::Applied
        && !(gate_proof.promotion.status == AgentTaskPromotionStatus::VerifiedNoChanges
            && !gate_proof.promotion.changed_files.is_empty()
            && gate_proof.promotion.finalization_eligible())
    {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "durable gate proof must record an applied promotion or a verified existing-candidate delta",
            None,
            None,
        ));
    }
    if gate_proof.promotion.source.run_id.as_deref() != Some(gate_proof.run_id.as_str()) {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "durable gate proof promotion source belongs to a different run",
            None,
            None,
        ));
    }
    if gate_proof.promotion.target.path.as_deref() != Some(options.path.as_str()) {
        return Err(Error::validation_invalid_argument(
            "path",
            "durable gate proof promotion target does not match finalization path",
            None,
            None,
        ));
    }
    Ok(())
}

fn inherit_promotion_gates(
    options: &mut AgentTaskPrFinalizationOptions,
    gate_proof: AgentTaskPrDurableGateProof,
) -> Result<()> {
    validate_gate_proof_binding(&gate_proof, options)?;
    let promotion = gate_proof.promotion;
    if promotion.schema != crate::agent_task_promotion::AGENT_TASK_PROMOTION_REPORT_SCHEMA
        || promotion.to_worktree.trim().is_empty()
        || promotion.target.worktree != promotion.to_worktree
    {
        return Err(Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion",
            "green promotion reuse requires a current receipt bound to one exact target worktree",
            None,
            None,
        ));
    }
    let verified_base = promotion.verified_base.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion.verified_base",
            "green promotion reuse requires the immutable base captured before its gates ran",
            None,
            None,
        )
    })?;
    if verified_base.base != options.base
        || options.verified_base_sha.as_deref() != Some(verified_base.sha.as_str())
    {
        return Err(Error::validation_invalid_argument(
            "verified_base_sha",
            "manual finalization base does not match the green promotion receipt; run fresh verification for the current base",
            options.verified_base_sha.clone(),
            None,
        ));
    }
    if !promotion.finalization_eligible()
        || promotion.gate_outcome().gate_results != promotion.gate_results
    {
        return Err(Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion.gate_results",
            "manual finalization can inherit only authoritative green promotion gates; run fresh verification",
            None,
            None,
        ));
    }
    let candidate: crate::agent_task_gate::AgentTaskGateCandidateCheckout = serde_json::from_value(
        promotion.provenance["candidate_checkout"].clone(),
    )
    .map_err(|_| {
        Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion.provenance.candidate_checkout",
            "green promotion reuse requires an immutable candidate checkout identity",
            None,
            None,
        )
    })?;
    let promoted_candidate: crate::agent_task_promotion::AgentTaskPromotionCandidate =
        serde_json::from_value(promotion.provenance["candidate"].clone()).map_err(|_| {
            Error::validation_invalid_argument(
                "cook_delivery.artifacts.promotion.provenance.candidate",
                "green promotion reuse requires the exact promoted Git candidate fingerprint",
                None,
                None,
            )
        })?;
    let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } =
        promoted_candidate
    else {
        return Err(Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion.provenance.candidate",
            "green promotion reuse requires the exact promoted Git candidate fingerprint",
            None,
            None,
        ));
    };
    if candidate.schema != "homeboy/agent-task-gate-candidate-checkout/v2"
        || candidate.commit.trim().is_empty()
        || candidate.tree.trim().is_empty()
        || candidate.tree != fingerprint.tree
    {
        return Err(Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion.provenance.candidate_checkout",
            "green promotion gate checkout does not match the exact promoted candidate tree; run fresh verification",
            None,
            None,
        ));
    }
    let gate_bindings = promotion
        .deterministic_gates
        .iter()
        .map(|gate| {
            let invocation = gate.invocation()?;
            if gate.id.trim().is_empty()
                || gate.candidate_checkout.as_ref() != Some(&candidate)
            {
                return Err(Error::validation_invalid_argument(
                    "cook_delivery.artifacts.promotion.deterministic_gates",
                    "green promotion gate commands and candidate identities do not match their durable receipt; run fresh verification",
                    None,
                    None,
                ));
            }
            Ok((
                gate.id.clone(),
                invocation.identity_digest()?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let gate_command_sha256: std::collections::BTreeMap<_, _> =
        gate_bindings.iter().cloned().collect();
    if gate_command_sha256.len() != gate_bindings.len() {
        return Err(Error::validation_invalid_argument(
            "cook_delivery.artifacts.promotion.deterministic_gates.id",
            "green promotion reuse requires unique non-empty retained gate ids",
            None,
            None,
        ));
    }
    let gate_ids = gate_bindings
        .into_iter()
        .map(|(gate_id, _)| gate_id)
        .collect::<Vec<_>>();
    let verified_commands =
        crate::agent_task_review_dossier::verified_commands_from_promotion(&promotion);
    options.evidence.verification.targeted_checks_run = verified_commands
        .iter()
        .map(|verified| verified.command.clone())
        .collect();
    options.review_dossier.verified_commands = verified_commands;
    options.review_dossier.evidence.push(
        crate::agent_task_review_dossier::AgentTaskReviewEvidence {
            summary: format!(
                "Verified inherited promotion evidence: {} exact candidate-bound gate(s) from {}.",
                gate_ids.len(),
                gate_proof.run_id
            ),
            url: None,
        },
    );
    options.gate_results = promotion
        .gate_results
        .iter()
        .map(|gate| AgentTaskGateResult {
            name: gate.name.clone(),
            status: "passed".to_string(),
            detail: Some("verified inherited promotion evidence".to_string()),
        })
        .collect();
    options.candidate = promotion.candidate.clone();
    options.changed_files = promotion.changed_files.clone();
    options.normalized_gate_results = promotion.gate_results;
    options.inherited_gate_evidence = Some(AgentTaskInheritedGateEvidence {
        schema: AGENT_TASK_INHERITED_GATE_EVIDENCE_SCHEMA.to_string(),
        status: "verified_inherited".to_string(),
        source_run_id: gate_proof.run_id,
        promotion_schema: promotion.schema,
        target_worktree: promotion.to_worktree,
        target_path: options.path.clone(),
        verified_base,
        candidate,
        gate_ids,
        gate_command_sha256,
    });
    Ok(())
}

fn validate_finalization_gates(gates: &[HomeboyGateResult]) -> Result<()> {
    if gates.is_empty() {
        return Err(Error::validation_invalid_argument(
            "gate_results",
            "at least one deterministic green gate is required before PR finalization",
            None,
            None,
        ));
    }
    let red: Vec<String> = gates
        .iter()
        .filter(|gate| {
            !matches!(
                gate.status,
                HomeboyGateStatus::Passed | HomeboyGateStatus::AcceptedInheritedFailure
            )
        })
        .map(|gate| format!("{}={:?}", gate.name, gate.status))
        .collect();
    if !red.is_empty() {
        return Err(Error::validation_invalid_argument(
            "gate_results",
            format!(
                "finalization requires green gates; red gates: {}",
                red.join(", ")
            ),
            None,
            None,
        ));
    }
    Ok(())
}

fn is_green_status(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "green" | "passed" | "pass" | "succeeded" | "success" | "ok"
    )
}

impl From<AgentTaskGateResult> for HomeboyGateResult {
    fn from(gate: AgentTaskGateResult) -> Self {
        let status = if is_green_status(&gate.status) {
            HomeboyGateStatus::Passed
        } else {
            HomeboyGateStatus::Failed
        };
        let summary = match gate
            .detail
            .as_deref()
            .filter(|detail| !detail.trim().is_empty())
        {
            Some(detail) => format!("{}: {} ({detail})", gate.name, gate.status),
            None => format!("{}: {}", gate.name, gate.status),
        };

        HomeboyGateResult::new(
            format!("finalization.gate.{}", gate.name),
            gate.name.clone(),
            HomeboyGateKind::Command,
            status,
        )
        .summary(summary)
        .evidence(json!({
            "name": gate.name,
            "status": gate.status,
            "detail": gate.detail,
        }))
        .retryable(status == HomeboyGateStatus::Failed)
        .provenance(json!({
            "source_type": "AgentTaskGateResult",
        }))
    }
}

fn refuse_protected_head(head: &str, protected_branches: &[String]) -> Result<()> {
    if protected_branches.iter().any(|branch| branch == head) {
        return Err(Error::validation_invalid_argument(
            "head",
            format!(
                "refusing to finalize directly on protected branch '{}'",
                head
            ),
            None,
            Some(protected_branches.to_vec()),
        ));
    }
    Ok(())
}

pub fn validate_publication_intent(intent: &AgentTaskPublicationIntent) -> Result<()> {
    if intent.schema != AGENT_TASK_PUBLICATION_INTENT_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "publication_intent.schema",
            "publication intent schema is not supported",
            None,
            Some(vec![intent.schema.clone()]),
        ));
    }
    if intent.run_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "publication_intent.run_id",
            "publication intent requires a run id",
            None,
            None,
        ));
    }
    if intent.action.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "publication_intent.action",
            "publication intent requires an action",
            None,
            None,
        ));
    }
    if intent.target.kind.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "publication_intent.target.kind",
            "publication intent requires a target kind",
            None,
            None,
        ));
    }
    if intent
        .target
        .head
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(Error::validation_invalid_argument(
            "publication_intent.target.head",
            "publication intent requires a target head ref",
            None,
            None,
        ));
    }
    Ok(())
}

fn read_publication_head(
    run_id: &str,
    published_sha: &str,
    mut observe: impl FnMut() -> Result<AgentTaskPublicationBinding>,
) -> Result<AgentTaskPublicationBinding> {
    let mut diagnostic = String::new();
    for attempt in 0..3 {
        match observe() {
            Ok(binding) if binding.pr_head_sha == published_sha => return Ok(binding),
            Ok(binding) => {
                diagnostic = format!("expected {published_sha}, observed {}", binding.pr_head_sha)
            }
            Err(error) => diagnostic = error.message,
        }
        if attempt < 2 {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    let mut error = Error::validation_invalid_argument("publication_binding", format!("PR left open; head read-back failed: {diagnostic}; retry with `homeboy agent-task recover {run_id}`"), None, None);
    error.retryable = Some(true);
    Err(error)
}

fn build_pr_publication_intent(
    options: &AgentTaskPrFinalizationOptions,
    head: &str,
    changed_files: &[String],
    proof: HomeboyProof,
    base: &AgentTaskPrResolvedBase,
) -> AgentTaskPublicationIntent {
    AgentTaskPublicationIntent {
        schema: AGENT_TASK_PUBLICATION_INTENT_SCHEMA.to_string(),
        run_id: options.run_id.clone(),
        action: "review_request".to_string(),
        target: AgentTaskPublicationTarget {
            kind: "code_review".to_string(),
            publication_route: None,
            adapter: Some("github_pull_request".to_string()),
            path: Some(options.path.clone()),
            base: Some(options.base.clone()),
            verified_base_sha: Some(base.sha.clone()),
            publication_base_sha: None,
            head: Some(head.to_string()),
            url: None,
        },
        changed_files: changed_files.to_vec(),
        source_refs: options.evidence.source_refs.clone(),
        artifact_refs: options.evidence.artifact_refs.clone(),
        proof,
    }
}

fn publication_proof(
    intent: &AgentTaskPublicationIntent,
    status: &str,
    adapter_action: &str,
    adapter_ref: Option<String>,
    git_identity: Option<homeboy_core::git::GitIdentityProof>,
    git_tracking: Option<AgentTaskPublicationGitTracking>,
    binding: Option<AgentTaskPublicationBinding>,
) -> AgentTaskPublicationProof {
    let mut target = intent.target.clone();
    target.url = adapter_ref.clone();
    AgentTaskPublicationProof {
        schema: AGENT_TASK_PUBLICATION_PROOF_SCHEMA.to_string(),
        run_id: intent.run_id.clone(),
        status: status.to_string(),
        intent_schema: intent.schema.clone(),
        target,
        adapter_action: (adapter_action != "none").then(|| adapter_action.to_string()),
        adapter_ref,
        git_identity,
        git_tracking,
        binding,
        proof: intent.proof.clone(),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "finalization report preserves independently persisted publication proof fields"
)]
fn report(
    options: &AgentTaskPrFinalizationOptions,
    mut publication_intent: AgentTaskPublicationIntent,
    head: &str,
    status: &str,
    pr_action: &str,
    pr_number: Option<u64>,
    pr_url: Option<String>,
    changed_files: Vec<String>,
    proof: Option<HomeboyProof>,
    committed: bool,
    pushed: bool,
    git_identity: Option<homeboy_core::git::GitIdentityProof>,
    git_tracking: Option<AgentTaskPublicationGitTracking>,
    binding: Option<AgentTaskPublicationBinding>,
    acceptance: Option<crate::agent_task_lifecycle::AgentTaskAcceptanceRecord>,
) -> AgentTaskPrFinalizationReport {
    let normalized_gate_results = options.normalized_gate_results.clone();
    let proof =
        proof.unwrap_or_else(|| build_finalization_proof(options, normalized_gate_results.clone()));
    publication_intent.proof = proof.clone();
    let publication_proof = publication_proof(
        &publication_intent,
        status,
        pr_action,
        pr_url.clone(),
        git_identity,
        git_tracking,
        binding,
    );
    let finalization_outcome = finalization_outcome(
        &publication_intent,
        &publication_proof,
        status,
        pr_action,
        pr_number,
        pr_url.clone(),
        &changed_files,
        committed,
        pushed,
    );
    AgentTaskPrFinalizationReport {
        schema: AGENT_TASK_PR_FINALIZATION_SCHEMA.to_string(),
        run_id: options.run_id.clone(),
        status: status.to_string(),
        path: options.path.clone(),
        base: options.base.clone(),
        head: head.to_string(),
        title: options.title.clone(),
        pr_action: pr_action.to_string(),
        pr_number,
        pr_url,
        changed_files,
        gate_results: options.gate_results.clone(),
        normalized_gate_results,

        proof,
        publication_intent,
        publication_proof,
        finalization_outcome,
        acceptance,
        review_dossier: options.review_dossier.clone(),
        manual_finalization: options.manual_finalization,
        inherited_gate_evidence: options.inherited_gate_evidence.clone(),
        evidence: options.evidence.clone(),
    }
}

pub(crate) fn authorized_replacement_gate_proof(promotion: &AgentTaskPromotionReport) -> bool {
    let proof = &promotion.provenance["replacement_gate_proof"];
    proof["schema"] == "homeboy/agent-task-replacement-gate-proof/v1"
        && proof["externally_produced"] == true
        && proof["operator_authorization"]
            .as_str()
            .is_some_and(|value| !value.trim().is_empty())
        && proof["original_history"]["run_id"].as_str() == promotion.source.run_id.as_deref()
        && proof["original_history"]["sha256"]
            .as_str()
            .is_some_and(|digest| digest.len() == 64)
        && promotion.finalization_eligible()
}

fn validate_durable_publication_eligibility(
    lifecycle: &RunLifecycleRecord,
    promotion: &AgentTaskPromotionReport,
) -> Result<()> {
    let producing_runtime = lifecycle
        .provider_runtime
        .iter()
        .rev()
        .find(|runtime| runtime.task_id == promotion.source.task_id);
    let producing_runtime_succeeded =
        producing_runtime.is_some_and(|runtime| runtime.state == ProviderRuntimeState::Succeeded);
    let fingerprinted_candidate = matches!(
        serde_json::from_value::<crate::agent_task_promotion::AgentTaskPromotionCandidate>(
            promotion.provenance["candidate"].clone(),
        ),
        Ok(crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { .. })
    );
    // The same passing verdict recovery step selection uses: a proven
    // inherited failure passes and stays recorded as evidence (#17508).
    let green_gates = promotion.finalization_eligible();
    let green_fingerprinted_candidate =
        fingerprinted_candidate && promotion.applied_with_passing_verdict();
    // Retry history is durable and intentionally retains failed attempts. It
    // must not veto publication when the canonical receipt for the task that
    // produced this promoted candidate records a successful attempt.
    let all_provider_runtimes_succeeded = !lifecycle.provider_runtime.is_empty()
        && lifecycle
            .provider_runtime
            .iter()
            .all(|runtime| runtime.state == ProviderRuntimeState::Succeeded);
    // A partial or candidate-recoverable run publishes the candidate its gates
    // verified (#16441). Homeboy's own promotion binds its gates to the
    // fingerprint it records with them; an operator replacement binds them
    // through its authorized proof. Recording green delivery can already have
    // advanced execution to Succeeded even when the producing runtime failed;
    // retain the same fingerprint/model/proof checks after that transition.
    let verified_candidate_recovery = (matches!(
        lifecycle.execution.state,
        RunExecutionState::PartialFailure | RunExecutionState::CandidateRecoverable
    ) || (lifecycle.execution.state
        == RunExecutionState::Succeeded
        && authorized_replacement_gate_proof(promotion)))
        && producing_runtime.is_some()
        && match promotion.provenance.get("replacement_gate_proof") {
            None => green_fingerprinted_candidate,
            Some(_) => fingerprinted_candidate && authorized_replacement_gate_proof(promotion),
        };
    if (lifecycle.execution.state == RunExecutionState::Succeeded
        && (all_provider_runtimes_succeeded
            || (producing_runtime_succeeded && green_fingerprinted_candidate)))
        || verified_candidate_recovery
    {
        return Ok(());
    }

    let recovery = promotion.provenance.pointer("/adoption/recovery");
    let candidate_ref = promotion.provenance["adoption"]["candidate_ref"].as_str();
    let candidate_head = promotion
        .provenance
        .pointer("/candidate/fingerprint/head")
        .and_then(serde_json::Value::as_str);
    let authenticated_adoption = matches!(
        lifecycle.execution.state,
        RunExecutionState::Cancelled | RunExecutionState::Failed
    ) && no_real_provider_execution(lifecycle)
        && promotion.provenance["adoption"]["source_run_id"]
            == promotion.source.run_id.clone().unwrap_or_default()
        && candidate_ref.is_some_and(is_git_commit_identity)
        && candidate_ref == candidate_head
        && recovery.is_some_and(|recovery| {
            crate::agent_task_lifecycle::candidate_adoption_recovery_eligibility(recovery).is_some()
        })
        && green_gates;
    if authenticated_adoption {
        return Ok(());
    }

    // An externally prepared commit has no successful provider runtime to
    // attest. Its authenticated adoption promotion supplies equivalent,
    // candidate-bound evidence instead.
    let committed_change_provenance = promotion.provenance["change_source"] == "local_commits"
        && promotion
            .provenance
            .get("commit_range")
            .and_then(serde_json::Value::as_str)
            .and_then(|range| range.rsplit_once(".."))
            .is_some_and(|(_, candidate)| Some(candidate) == candidate_head)
        && promotion
            .provenance
            .get("commits")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|commits| !commits.is_empty());
    let candidate_is_bound = candidate_ref.is_some_and(|candidate_ref| {
        is_git_commit_identity(candidate_ref)
            && candidate_head.is_some_and(|candidate_head| {
                is_full_git_commit_identity(candidate_head)
                    && (candidate_ref == candidate_head
                        || candidate_head.starts_with(candidate_ref))
            })
    });
    let authenticated_external_adoption = promotion.applied_with_passing_verdict()
        && promotion.provenance["adoption"]["source_run_id"]
            == promotion.source.run_id.clone().unwrap_or_default()
        && candidate_is_bound
        && committed_change_provenance;
    if authenticated_external_adoption {
        return Ok(());
    }

    let reason = if !green_gates {
        "publication refused: gates are not green; at least one gate is required and inherited failures must have baseline proof"
    } else if !fingerprinted_candidate {
        "publication refused: candidate is not fingerprinted"
    } else if promotion.status != crate::agent_task_promotion::AgentTaskPromotionStatus::Applied {
        "publication refused: candidate is not applied"
    } else if !matches!(
        lifecycle.execution.state,
        RunExecutionState::Succeeded
            | RunExecutionState::PartialFailure
            | RunExecutionState::CandidateRecoverable
    ) {
        "publication refused: wrong execution state without authenticated candidate adoption"
    } else {
        "publication refused: producing runtime has not succeeded or candidate-bound recovery/adoption proof is invalid"
    };
    Err(
        crate::agent_task_service::CookDeliveryBlockerCode::PublishRefused.mark(
            Error::validation_invalid_argument("run_id", reason, None, None),
        ),
    )
}

fn no_real_provider_execution(lifecycle: &RunLifecycleRecord) -> bool {
    lifecycle.external_runtime_ids.is_empty()
        && lifecycle.provider_runtime.iter().all(|runtime| {
            runtime.external_runtime_ids.is_empty()
                && runtime.metadata["evidence_source"] == "canonical_executor_outcome"
        })
}

fn is_git_commit_identity(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_full_git_commit_identity(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[expect(
    clippy::too_many_arguments,
    reason = "outcome fields mirror the durable finalization schema"
)]
fn finalization_outcome(
    intent: &AgentTaskPublicationIntent,
    publication_proof: &AgentTaskPublicationProof,
    status: &str,
    pr_action: &str,
    pr_number: Option<u64>,
    pr_url: Option<String>,
    changed_files: &[String],
    committed: bool,
    pushed: bool,
) -> AgentTaskPrFinalizationOutcome {
    let published = matches!(
        pr_action,
        "created" | "updated" | "ready" | "already_merged"
    );
    AgentTaskPrFinalizationOutcome {
        schema: AGENT_TASK_PR_FINALIZATION_OUTCOME_SCHEMA.to_string(),
        run_id: intent.run_id.clone(),
        status: status.to_string(),
        publication_status: publication_proof.status.clone(),
        publication_action: pr_action.to_string(),
        target: publication_proof.target.clone(),
        base: intent.target.base.clone().unwrap_or_default(),
        head: intent.target.head.clone().unwrap_or_default(),
        pr_number,
        pr_url,
        changed_files: changed_files.to_vec(),
        committed,
        pushed,
        published,
    }
}

fn build_finalization_proof(
    options: &AgentTaskPrFinalizationOptions,
    gates: Vec<HomeboyGateResult>,
) -> HomeboyProof {
    proof::build_finalization_proof(options, gates)
}

#[cfg(test)]
mod tests;
