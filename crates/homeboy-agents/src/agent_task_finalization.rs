use serde_json::json;

use crate::agent_task_model::normalize_concrete_model_identifier;
use crate::agent_task_promotion::AgentTaskPromotionReport;
use crate::agent_task_review_dossier::{
    enrich_dossier, render_review_dossier, AgentTaskReviewDossier, AgentTaskReviewProfile,
};
use homeboy_core::error::{Error, Result};
use homeboy_core::gate::{HomeboyGateKind, HomeboyGateResult, HomeboyGateStatus};
use homeboy_core::proof::HomeboyProof;
use homeboy_core::run_lifecycle_record::{ProviderRuntimeState, RunLifecycleRecord};

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
    finalize_pr_with_backend(options, &mut RealAgentTaskPrFinalizationBackend)
}

/// Validate a finalization dossier and its durable candidate without mutation.
pub fn preflight_pr(
    options: AgentTaskPrFinalizationOptions,
) -> Result<AgentTaskPrFinalizationReport> {
    preflight_pr_with_backend(options, &mut RealAgentTaskPrFinalizationBackend)
}

/// Hydrate dependencies for fresh manual gates in their immutable checkout.
pub fn hydrate_manual_verification_dependencies(
    checkout: &std::path::Path,
) -> Result<Vec<AgentTaskGateSetupEvidence>> {
    crate::agent_task_gate::hydrate_gate_dependency_roots_with_policy(
        checkout,
        true,
        "manual_finalization_checkout",
        &homeboy_core::deps::DependencyHydrationPolicy::default(),
    )
}

pub fn finalize_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    options: AgentTaskPrFinalizationOptions,
    backend: &mut B,
) -> Result<AgentTaskPrFinalizationReport> {
    finalize_pr_with_backend_mode(options, backend, true, None)
}

pub fn finalize_pr_with_backend_in_store<B: AgentTaskPrFinalizationBackend>(
    options: AgentTaskPrFinalizationOptions,
    backend: &mut B,
    lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
) -> Result<AgentTaskPrFinalizationReport> {
    finalize_pr_with_backend_mode(options, backend, true, Some(lifecycle_store))
}

pub fn preflight_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    options: AgentTaskPrFinalizationOptions,
    backend: &mut B,
) -> Result<AgentTaskPrFinalizationReport> {
    finalize_pr_with_backend_mode(options, backend, false, None)
}

fn finalize_pr_with_backend_mode<B: AgentTaskPrFinalizationBackend>(
    mut options: AgentTaskPrFinalizationOptions,
    backend: &mut B,
    publish: bool,
    lifecycle_store: Option<&crate::agent_task_lifecycle::AgentTaskLifecycleStore>,
) -> Result<AgentTaskPrFinalizationReport> {
    let mut durable_changed_files = Vec::new();
    let durable_acceptance;
    if options.manual_finalization {
        durable_acceptance = validate_manual_finalization_policy(&options.run_id)?;
        if options.inherited_gate_evidence.is_some()
            || (options.normalized_gate_results.is_empty()
                && options.verified_candidate_sha.is_none())
        {
            let claimed_evidence = options.inherited_gate_evidence.take();
            let inherited = match lifecycle_store {
                Some(store) => {
                    backend.hydrate_optional_gate_proof_in_store(store, &options.run_id)?
                }
                None => backend.hydrate_optional_gate_proof(&options.run_id)?,
            };
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
        let lifecycle = match lifecycle_store {
            Some(store) => backend.hydrate_run_in_store(store, &options.run_id)?,
            None => backend.hydrate_run(&options.run_id)?,
        };
        let gate_proof = match lifecycle_store {
            Some(store) => backend.hydrate_gate_proof_in_store(store, &options.run_id)?,
            None => backend.hydrate_gate_proof(&options.run_id)?,
        };
        let review_form_only_follow_up =
            is_review_form_only_follow_up(&gate_proof.promotion, &gate_proof.run_id);
        let authenticated_follow_up = review_form_only_follow_up
            && gate_proof
                .promotion
                .provenance
                .pointer("/cook_follow_up/source_run_id")
                .and_then(serde_json::Value::as_str)
                == Some(options.run_id.as_str());
        if gate_proof.run_id != options.run_id && !authenticated_follow_up {
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
        let eligibility =
            validate_durable_publication_eligibility(&lifecycle, &gate_proof.promotion)?;
        durable_changed_files = normalize_changed_files(&gate_proof.promotion.changed_files);
        if normalize_changed_files(&options.changed_files) != durable_changed_files {
            return Err(changed_files_mismatch_error(
                &options.run_id,
                &options.changed_files,
                &gate_proof.promotion.changed_files,
            ));
        }
        options.normalized_gate_results = gate_proof.promotion.gate_results;
        if options.normalized_gate_results.is_empty() {
            return Err(Error::validation_invalid_argument(
                "run_id",
                "durable gate proof contains no normalized deterministic gates",
                None,
                None,
            ));
        }
        if eligibility == DurablePublicationEligibility::ProviderRun
            && !review_form_only_follow_up
            && !options.composed_ai_model_disclosure
        {
            options.review_dossier.ai_assistance.model = durable_model(&lifecycle)?;
        }
        options.evidence.lifecycle = Some(lifecycle);
    }
    validate_finalization_gates(
        &options.normalized_gate_results,
        options.accept_inherited_failures,
    )?;
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
    let mut candidate = backend.candidate_state(&options.path, &base, &head)?;
    if let AgentTaskPrCandidateState::BehindBase { .. } = &candidate {
        // A manually verified candidate asserts an immutable commit identity, and
        // merging would move HEAD out from under that proof. Those runs keep the
        // pre-#13695 refusal and stay a human decision.
        if options.verified_candidate_sha.is_none() {
            if let AgentTaskPrBaseConvergence::Converged =
                backend.converge_base(&options.path, &base)?
            {
                candidate = backend.candidate_state(&options.path, &base, &head)?;
            }
        }
    }
    let (mut changed_files, commit_required, push_required) = match candidate {
        AgentTaskPrCandidateState::BehindBase {
            behind,
            base_ref,
            base_sha,
            ..
        } => {
            return Err(Error::validation_invalid_argument(
                "base",
                &format!(
                    "HEAD is behind or diverged from resolved base `{base_ref}` at `{base_sha}` ({behind} base-only commit(s)) and could not be merged automatically; resolve the conflict in the worktree before finalizing"
                ),
                None,
                None,
            ));
        }
        AgentTaskPrCandidateState::Dirty { changed_files } => (changed_files, true, true),
        AgentTaskPrCandidateState::Committed {
            changed_files,
            push_required,
        } => (changed_files, false, push_required),
        AgentTaskPrCandidateState::Equivalent => (Vec::new(), false, false),
        AgentTaskPrCandidateState::Invalid { diagnostic } => {
            return Err(Error::validation_invalid_argument(
                "base",
                &diagnostic,
                None,
                None,
            ));
        }
    };
    let candidate_changed_files = normalize_changed_files(&changed_files);
    if let Some(verified_candidate_sha) = options.verified_candidate_sha.as_deref() {
        if commit_required {
            return Err(Error::validation_invalid_argument(
                "verify",
                "manual verification candidate changed after its gate completed",
                None,
                None,
            ));
        }
        let observed = backend.validate_committed_publication_identity(&options.path, None)?;
        if observed.commit_sha.as_deref() != Some(verified_candidate_sha) {
            return Err(Error::validation_invalid_argument(
                "verify",
                "manual verification candidate changed after its gate completed",
                observed.commit_sha,
                None,
            ));
        }
    }
    if options.expected_candidate_sha.is_some() && (commit_required || push_required) {
        return Err(Error::validation_invalid_argument(
            "publication_intent",
            "recovered manual finalization requires the already-pushed candidate validated by preflight",
            None,
            None,
        ));
    }
    if options.expected_candidate_sha.is_some()
        && normalize_changed_files(&options.changed_files) != candidate_changed_files
    {
        return Err(Error::validation_invalid_argument(
            "publication_intent.changed_files",
            "recovered manual finalization changed files no longer match the preflight-validated candidate",
            None,
            None,
        ));
    }
    if !options.manual_finalization {
        changed_files = durable_changed_files;
    } else if !options.changed_files.is_empty() {
        changed_files = options.changed_files.clone();
    }
    changed_files.sort();
    changed_files.dedup();
    enrich_dossier(
        &mut options.review_dossier,
        &options.evidence.source_refs,
        &options.evidence.artifact_refs,
        &options.normalized_gate_results,
        &options.evidence.verification.ci_expected,
        options.evidence.lifecycle.as_ref(),
    );
    options.review_dossier.evidence.push(
        crate::agent_task_review_dossier::AgentTaskReviewEvidence {
            summary: format!(
                "Verified finalization base: {} at {}",
                options.base, base.sha
            ),
            url: None,
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
    // A review-form-only adoption follow-up can supply a `used_for` claiming no
    // code was changed, but the finalized candidate here has AI-authored changed
    // files — publishing that disclosure would materially understate the work
    // and mislead reviewers (#9897). Fail closed against the patch lineage.
    crate::agent_task_review_dossier::validate_used_for_against_changed_files(
        &options.review_dossier,
        &changed_files,
    )?;
    let proof = build_finalization_proof(&options, options.normalized_gate_results.clone());
    let mut intent =
        build_pr_publication_intent(&options, &head, &changed_files, proof.clone(), &base);
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

    if !options.manual_finalization || options.inherited_gate_evidence.is_some() {
        match lifecycle_store {
            Some(store) => backend.validate_candidate_in_store(store, &options)?,
            None => backend.validate_candidate(&options)?,
        }
    }
    if options.manual_finalization && !publish && !commit_required && push_required {
        return Err(Error::validation_invalid_argument(
            "publication_intent",
            "recoverable manual preflight requires an already-pushed candidate; push the candidate branch and rerun preflight",
            None,
            None,
        ));
    }
    // Validate intent before commit mutation, then bind evidence to immutable HEAD before push.
    let prospective_identity = if commit_required {
        Some(backend.validate_publication_identity(&options.path)?)
    } else {
        None
    };
    if !publish {
        let git_identity = match prospective_identity {
            Some(git_identity) => git_identity,
            None => backend.validate_committed_publication_identity(&options.path, None)?,
        };
        return Ok(report(
            &options,
            intent,
            &head,
            "validated",
            "none",
            None,
            None,
            changed_files,
            Some(proof),
            false,
            false,
            Some(git_identity),
            // Validation-only finalization performs no push, so there is no
            // publication git tracking to record.
            None,
            None,
            durable_acceptance,
        ));
    }
    if commit_required {
        backend.commit_all(&options.path, &options.commit_message)?;
    }
    let git_identity = backend
        .validate_committed_publication_identity(&options.path, prospective_identity.as_ref())?;
    let commit_sha = git_identity.commit_sha.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "git_identity",
            "committed Git identity proof must be bound to a commit SHA before publication",
            None,
            None,
        )
    })?;
    if let Some(verified_candidate_sha) = options.verified_candidate_sha.as_deref() {
        if verified_candidate_sha != commit_sha {
            return Err(Error::validation_invalid_argument(
                "verify",
                "manual verification candidate changed after its gate completed",
                Some(commit_sha.to_string()),
                None,
            ));
        }
    }
    if let Some(expected_candidate_sha) = options.expected_candidate_sha.as_deref() {
        if expected_candidate_sha != commit_sha {
            return Err(Error::validation_invalid_argument(
                "publication_intent",
                "recovered manual finalization candidate no longer matches the preflight-validated commit",
                Some(commit_sha.to_string()),
                None,
            ));
        }
    }
    let git_tracking = if push_required {
        Some(backend.push_branch(&options.path, commit_sha, &head)?)
    } else {
        None
    };
    let existing = backend.find_open_pr(&options.path, &options.base, &head)?;
    let publication_base_sha = backend.publication_base_sha(&options.path, &options.base)?;
    intent.target.publication_base_sha = publication_base_sha.clone();
    let base_observation = match publication_base_sha {
        Some(publication_base_sha) if publication_base_sha == base.sha => format!(
            "Base unchanged since verification: {} remains at {}.",
            options.base, base.sha
        ),
        Some(publication_base_sha) => format!(
            "Base advanced after verification: verified {} at {}; publication observed {}. Candidate ancestry was validated against the verified snapshot.",
            options.base, base.sha, publication_base_sha
        ),
        None => format!(
            "Base observation unavailable immediately before publication; candidate ancestry was validated against verified {} at {}.",
            options.base, base.sha
        ),
    };
    options.review_dossier.evidence.push(
        crate::agent_task_review_dossier::AgentTaskReviewEvidence {
            summary: base_observation,
            url: None,
        },
    );
    options.review_dossier.evidence.sort_by(|left, right| {
        left.summary
            .cmp(&right.summary)
            .then(left.url.cmp(&right.url))
    });
    options.review_dossier.evidence.dedup();
    let body = render_review_dossier(&options.review_dossier, &options.review_profile);
    // The lookup and base observation may take long enough for another writer to
    // replace the branch. Bind the live remote again at the last safe point.
    let observed_remote_sha = backend.verify_remote_candidate(&options.path, &head, commit_sha)?;
    if observed_remote_sha != commit_sha {
        return Err(publication_drift_error(
            commit_sha,
            &observed_remote_sha,
            None,
            "no PR mutation performed",
        ));
    }
    let newly_created = existing.is_none();
    let draft = existing.as_ref().is_some_and(|pr| pr.is_draft);
    let quarantine_capability = backend.quarantine_capability(newly_created, draft)?;
    if !quarantine_capability_is_safe(quarantine_capability, newly_created, draft) {
        return Err(Error::validation_invalid_argument(
            "publication_quarantine",
            format!(
                "refusing PR mutation without a guaranteed safe quarantine transition; cleanup_capability={}",
                quarantine_capability_name(quarantine_capability)
            ),
            None,
            None,
        ));
    }
    let (action, pr) = match existing {
        Some(existing) => (
            "updated",
            backend.update_pr(&options.path, existing.number, &options.title, &body)?,
        ),
        None => (
            "created",
            backend.create_pr(
                &options.path,
                &options.base,
                &head,
                &options.title,
                &body,
                options.draft_pr,
            )?,
        ),
    };
    let published_draft = if newly_created {
        options.draft_pr
    } else {
        draft
    };

    let binding = match backend.verify_publication_binding(
        &options.path,
        &options.base,
        &head,
        commit_sha,
        &changed_files,
        &pr,
    ) {
        Ok(binding) => binding,
        Err(error) => {
            return Err(publication_drift_with_cleanup_error(
                backend,
                &options.path,
                &pr,
                commit_sha,
                "not_observed",
                None,
                quarantine_capability,
                &error.message,
            ));
        }
    };
    if let Err(_error) = validate_publication_binding(&binding, commit_sha, &changed_files) {
        return Err(publication_drift_with_cleanup_error(
            backend,
            &options.path,
            &pr,
            commit_sha,
            &binding.remote_sha,
            Some(&binding.pr_head_sha),
            quarantine_capability,
            "binding tuple mismatch",
        ));
    }

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
        commit_required,
        push_required,
        Some(git_identity),
        git_tracking,
        Some(binding),
        durable_acceptance,
    ))
}

/// Manual finalization deliberately has no promotion lineage. It still cannot
/// replace an acceptance decision that the durable run's policy requires.
fn validate_manual_finalization_policy(
    run_id: &str,
) -> Result<Option<crate::agent_task_lifecycle::AgentTaskAcceptanceRecord>> {
    let record = match crate::agent_task_lifecycle::status(run_id) {
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
    lifecycle_store: Option<&crate::agent_task_lifecycle::AgentTaskLifecycleStore>,
) -> Result<Option<crate::agent_task_lifecycle::AgentTaskAcceptanceRecord>> {
    let record = match lifecycle_store.map_or_else(
        || crate::agent_task_lifecycle::status(run_id),
        |store| store.read_record(run_id),
    ) {
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
                    && !candidate.base.trim().is_empty()
                    && !candidate.sha256.trim().is_empty()
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
            && gate_proof.promotion.finalization_eligible(false))
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
            "latest_promotion",
            "green promotion reuse requires a current receipt bound to one exact target worktree",
            None,
            None,
        ));
    }
    let verified_base = promotion.verified_base.clone().ok_or_else(|| {
        Error::validation_invalid_argument(
            "latest_promotion.verified_base",
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
    if !promotion.finalization_eligible(false)
        || promotion.gate_outcome().gate_results != promotion.gate_results
    {
        return Err(Error::validation_invalid_argument(
            "latest_promotion.gate_results",
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
            "latest_promotion.provenance.candidate_checkout",
            "green promotion reuse requires an immutable candidate checkout identity",
            None,
            None,
        )
    })?;
    let promoted_candidate: crate::agent_task_promotion::AgentTaskPromotionCandidate =
        serde_json::from_value(promotion.provenance["candidate"].clone()).map_err(|_| {
            Error::validation_invalid_argument(
                "latest_promotion.provenance.candidate",
                "green promotion reuse requires the exact promoted Git candidate fingerprint",
                None,
                None,
            )
        })?;
    let crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { fingerprint } =
        promoted_candidate
    else {
        return Err(Error::validation_invalid_argument(
            "latest_promotion.provenance.candidate",
            "green promotion reuse requires the exact promoted Git candidate fingerprint",
            None,
            None,
        ));
    };
    if candidate.schema != "homeboy/agent-task-gate-candidate-checkout/v1"
        || candidate.commit.trim().is_empty()
        || candidate.tree.trim().is_empty()
        || candidate.candidate_sha256.trim().is_empty()
        || candidate.tree != fingerprint.tree
        || candidate.candidate_sha256 != fingerprint.sha256
    {
        return Err(Error::validation_invalid_argument(
            "latest_promotion.provenance.candidate_checkout",
            "green promotion gate checkout does not match the exact promoted candidate tree and digest; run fresh verification",
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
                    "latest_promotion.deterministic_gates",
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
            "latest_promotion.deterministic_gates.id",
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
    if options.review_dossier.how_to_test.is_empty() {
        options.review_dossier.how_to_test = verified_commands
            .iter()
            .map(
                |verified| crate::agent_task_review_dossier::AgentTaskReviewTestStep {
                    command: verified.command.clone(),
                    expected: "passes as inherited from the exact green promotion receipt"
                        .to_string(),
                },
            )
            .collect();
    }
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

/// A form-only continuation has two authenticated provider executions to
/// disclose. Its terminal promotion is the durable boundary that preserves the
/// composed model attribution rather than replacing it with the form provider.
fn is_review_form_only_follow_up(promotion: &AgentTaskPromotionReport, run_id: &str) -> bool {
    promotion
        .provenance
        .pointer("/cook_follow_up/kind")
        .and_then(serde_json::Value::as_str)
        == Some("review_form_only")
        && promotion
            .provenance
            .pointer("/cook_follow_up/source_run_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|source_run_id| !source_run_id.is_empty() && source_run_id != run_id)
}

fn validate_finalization_gates(
    gates: &[HomeboyGateResult],
    accept_inherited_failures: bool,
) -> Result<()> {
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
            gate.status != HomeboyGateStatus::Passed
                && !(accept_inherited_failures
                    && gate.status == HomeboyGateStatus::AcceptedInheritedFailure)
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
        gate_result_from_legacy(gate)
    }
}

pub(crate) fn gate_result_from_legacy(gate: AgentTaskGateResult) -> HomeboyGateResult {
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

fn validate_publication_binding(
    binding: &AgentTaskPublicationBinding,
    candidate_sha: &str,
    changed_files: &[String],
) -> Result<()> {
    if binding.candidate_sha != candidate_sha
        || binding.remote_sha != candidate_sha
        || binding.pr_head_sha != candidate_sha
    {
        return Err(Error::validation_invalid_argument(
            "publication_binding",
            "candidate SHA, pushed remote ref SHA, and GitHub PR head SHA must match before finalization succeeds",
            None,
            None,
        ));
    }
    if binding.candidate_tree.is_empty()
        || binding.repository.is_empty()
        || binding.head_repository != binding.repository
        || normalize_changed_files(&binding.changed_files) != normalize_changed_files(changed_files)
    {
        return Err(Error::validation_invalid_argument(
            "publication_binding",
            "publication binding must record the candidate tree, exact changed files, and a same-repository PR head",
            None,
            None,
        ));
    }
    Ok(())
}

const PUBLICATION_IDENTITY_DIAGNOSTIC_LIMIT: usize = 160;

fn publication_drift_error(
    expected_candidate_sha: &str,
    observed_remote_sha: &str,
    observed_pr_head_sha: Option<&str>,
    cleanup_state: &str,
) -> Error {
    let bounded = |value: &str| {
        let value = value.trim();
        let characters = value.chars().count();
        if characters <= PUBLICATION_IDENTITY_DIAGNOSTIC_LIMIT {
            value.to_string()
        } else {
            format!(
                "{}...(+{} chars)",
                value
                    .chars()
                    .take(PUBLICATION_IDENTITY_DIAGNOSTIC_LIMIT)
                    .collect::<String>(),
                characters - PUBLICATION_IDENTITY_DIAGNOSTIC_LIMIT
            )
        }
    };
    Error::validation_invalid_argument(
        "publication_binding",
        format!(
            "publication candidate drift refused: expected_candidate_sha={}; observed_remote_sha={}; observed_pr_head_sha={}; cleanup_state={}",
            bounded(expected_candidate_sha),
            bounded(observed_remote_sha),
            observed_pr_head_sha.map(bounded).unwrap_or_else(|| "not_observed".to_string()),
            bounded(cleanup_state),
        ),
        None,
        None,
    )
}

fn quarantine_capability_is_safe(
    capability: AgentTaskPrQuarantineCapability,
    newly_created: bool,
    was_draft: bool,
) -> bool {
    matches!(
        (capability, newly_created, was_draft),
        (AgentTaskPrQuarantineCapability::CloseNewPr, true, _)
            | (
                AgentTaskPrQuarantineCapability::PreserveExistingDraft,
                false,
                true
            )
            | (
                AgentTaskPrQuarantineCapability::ConvertExistingReadyPrToDraft,
                false,
                false
            )
    )
}

fn quarantine_capability_name(capability: AgentTaskPrQuarantineCapability) -> &'static str {
    match capability {
        AgentTaskPrQuarantineCapability::CloseNewPr => "close_new_pr",
        AgentTaskPrQuarantineCapability::PreserveExistingDraft => "preserve_existing_draft",
        AgentTaskPrQuarantineCapability::ConvertExistingReadyPrToDraft => {
            "convert_existing_ready_pr_to_draft"
        }
        AgentTaskPrQuarantineCapability::Unsupported => "unsupported",
    }
}

fn publication_drift_with_cleanup_error<B: AgentTaskPrFinalizationBackend>(
    backend: &mut B,
    path: &str,
    pr: &AgentTaskPrRef,
    expected_candidate_sha: &str,
    observed_remote_sha: &str,
    observed_pr_head_sha: Option<&str>,
    capability: AgentTaskPrQuarantineCapability,
    binding_error: &str,
) -> Error {
    let cleanup = match backend.quarantine_pr(path, pr, capability) {
        Ok(state) => state,
        Err(error) => format!(
            "failed; cleanup_capability={}; cleanup_error={}",
            quarantine_capability_name(capability),
            bounded_publication_diagnostic(&error.message)
        ),
    };
    let drift = publication_drift_error(
        expected_candidate_sha,
        observed_remote_sha,
        observed_pr_head_sha,
        &cleanup,
    );
    Error::validation_invalid_argument(
        "publication_binding",
        format!(
            "{}; binding_error={}",
            drift.message,
            bounded_publication_diagnostic(binding_error)
        ),
        None,
        None,
    )
}

fn bounded_publication_diagnostic(value: &str) -> String {
    let value = value.trim();
    let characters = value.chars().count();
    if characters <= PUBLICATION_IDENTITY_DIAGNOSTIC_LIMIT {
        value.to_string()
    } else {
        format!(
            "{}...(+{} chars)",
            value
                .chars()
                .take(PUBLICATION_IDENTITY_DIAGNOSTIC_LIMIT)
                .collect::<String>(),
            characters - PUBLICATION_IDENTITY_DIAGNOSTIC_LIMIT
        )
    }
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
        accept_inherited_failures: options.accept_inherited_failures,
        proof,
        publication_intent,
        publication_proof,
        finalization_outcome,
        acceptance,
        review_dossier: options.review_dossier.clone(),
        manual_finalization: options.manual_finalization,
        inherited_gate_evidence: options.inherited_gate_evidence.clone(),
        manual_candidate_binding: None,
        evidence: options.evidence.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurablePublicationEligibility {
    ProviderRun,
    PreProviderCandidateAdoptionRecovery,
    AuthenticatedExternalCandidateAdoption,
}

fn validate_durable_publication_eligibility(
    lifecycle: &RunLifecycleRecord,
    promotion: &AgentTaskPromotionReport,
) -> Result<DurablePublicationEligibility> {
    use homeboy_core::run_lifecycle_record::{ProviderRuntimeState, RunExecutionState};
    let all_provider_runtimes_succeeded = !lifecycle.provider_runtime.is_empty()
        && lifecycle
            .provider_runtime
            .iter()
            .all(|runtime| runtime.state == ProviderRuntimeState::Succeeded);
    let producing_runtime = lifecycle.provider_runtime.iter().find(|runtime| {
        runtime.task_id == promotion.source.task_id
            && runtime.state == ProviderRuntimeState::Succeeded
    });
    let fingerprinted_candidate = matches!(
        serde_json::from_value::<crate::agent_task_promotion::AgentTaskPromotionCandidate>(
            promotion.provenance["candidate"].clone(),
        ),
        Ok(crate::agent_task_promotion::AgentTaskPromotionCandidate::Git { .. })
    );
    let successful_fallback_produced_candidate = lifecycle.execution.state
        == RunExecutionState::Succeeded
        && producing_runtime.is_some()
        && fingerprinted_candidate;
    if (all_provider_runtimes_succeeded || successful_fallback_produced_candidate)
        && (lifecycle.execution.state == RunExecutionState::Succeeded
            // `CandidateRecoverable` and `PartialRecoverable` were folded into
            // `PartialFailure` before #6761, so they reached this check as
            // `PartialFailure` and were eligible. They are listed explicitly
            // now to keep that behavior — splitting the projection must not
            // quietly narrow durable-publication eligibility.
            || (matches!(
                lifecycle.execution.state,
                RunExecutionState::PartialFailure
                    | RunExecutionState::CandidateRecoverable
                    | RunExecutionState::PartialRecoverable
            ) && lifecycle.provider_runtime.iter().all(|runtime| {
                runtime.metadata["evidence_source"] == "durable_provider_execution"
            })))
    {
        return Ok(DurablePublicationEligibility::ProviderRun);
    }

    let recovery = promotion.provenance.pointer("/adoption/recovery");
    let candidate_ref = promotion.provenance["adoption"]["candidate_ref"].as_str();
    let candidate_head = promotion
        .provenance
        .pointer("/candidate/fingerprint/head")
        .and_then(serde_json::Value::as_str);
    let adoption_model = promotion.provenance["adoption"]["ai_model"].as_str();
    let authenticated_adoption = matches!(
        lifecycle.execution.state,
        RunExecutionState::Cancelled | RunExecutionState::Failed
    ) && no_real_provider_execution(lifecycle)
        && promotion.provenance["adoption"]["source_run_id"]
            == promotion.source.run_id.clone().unwrap_or_default()
        && candidate_ref.is_some_and(is_git_commit_identity)
        && candidate_ref == candidate_head
        && adoption_model.is_some_and(is_concrete_model)
        && recovery.is_some_and(|recovery| {
            crate::agent_task_lifecycle::candidate_adoption_recovery_eligibility(recovery).is_some()
        })
        && !promotion.gate_results.is_empty()
        && promotion
            .gate_results
            .iter()
            .all(|gate| gate.status == HomeboyGateStatus::Passed);
    if authenticated_adoption {
        return Ok(DurablePublicationEligibility::PreProviderCandidateAdoptionRecovery);
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
    let authenticated_external_adoption = promotion.status
        == crate::agent_task_promotion::AgentTaskPromotionStatus::Applied
        && promotion.provenance["adoption"]["source_run_id"]
            == promotion.source.run_id.clone().unwrap_or_default()
        && candidate_is_bound
        && adoption_model.is_some_and(is_concrete_model)
        && committed_change_provenance
        && !promotion.gate_results.is_empty()
        && promotion
            .gate_results
            .iter()
            .all(|gate| gate.status == HomeboyGateStatus::Passed);
    if authenticated_external_adoption {
        return Ok(DurablePublicationEligibility::AuthenticatedExternalCandidateAdoption);
    }

    Err(Error::validation_invalid_argument("run_id", "durable run must have succeeded execution and succeeded provider runtime before publication; the only exceptions are an applied, green, fingerprinted candidate-adoption recovery with durable zero-execution pre-provider transport provenance or an applied, green, committed-change-provenance-bound authenticated external candidate adoption", None, None))
}

fn no_real_provider_execution(lifecycle: &RunLifecycleRecord) -> bool {
    lifecycle.external_runtime_ids.is_empty()
        && lifecycle.provider_runtime.iter().all(|runtime| {
            runtime.external_runtime_ids.is_empty()
                && runtime.metadata["evidence_source"] == "canonical_executor_outcome"
        })
}

fn is_concrete_model(value: &str) -> bool {
    normalize_concrete_model_identifier(value).is_some()
}

fn is_git_commit_identity(value: &str) -> bool {
    (7..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_full_git_commit_identity(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn durable_model(lifecycle: &RunLifecycleRecord) -> Result<String> {
    let model = lifecycle
        .provider_runtime
        .iter()
        .rev()
        .filter(|runtime| runtime.state == ProviderRuntimeState::Succeeded)
        .find_map(|runtime| {
            runtime
                .metadata
                .get("model")
                .and_then(serde_json::Value::as_str)
        })
        .unwrap_or_default()
        .to_string();
    if model.trim().is_empty()
        || matches!(
            model.trim().to_ascii_lowercase().as_str(),
            "not recorded" | "unknown" | "ai-assisted" | "ai assisted"
        )
    {
        return Err(Error::validation_invalid_argument(
            "run_id",
            "durable provider metadata must record a concrete model before publication",
            None,
            None,
        ));
    }
    Ok(model)
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
    let published = matches!(pr_action, "created" | "updated");
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
