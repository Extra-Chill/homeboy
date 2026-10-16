//! The Cook delivery read model (#16049, slice 1).
//!
//! Delivery is everything after the provider finishes: apply, gates, base
//! convergence, commit, push and PR. Its facts are persisted across several
//! run-metadata keys written by different stages, and they have picked up
//! more than one spelling over time. [`CookDelivery::from_run_metadata`] is
//! the only code that interprets those keys. Reporters read the typed result
//! instead of the raw JSON, so they agree on one answer. Writers are
//! unchanged; this module never mutates a record.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_task_gate::AgentTaskGateStatus;
use crate::agent_task_promotion::AgentTaskPromotionStatus;

/// Moving-base recovery stops retrying after this many observed base moves.
/// Mirrors the writer's continuation budget in the Cook spine.
const BASE_MOVEMENT_BUDGET: u32 = 3;

/// One typed read of where a Cook's candidate is on its way to publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CookDelivery {
    pub state: CookDeliveryState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<CookDeliveryCandidate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<CookDeliveryDestination>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<CookDeliveryGate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication: Option<CookDeliveryPublication>,
    pub base_movements: u32,
    /// Present exactly when `state` is [`CookDeliveryState::Blocked`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<CookDeliveryBlocker>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookDeliveryState {
    /// The candidate reached its destination but its gates have not passed.
    Applied,
    /// The candidate's deterministic gates passed; it is not yet published.
    Verified,
    /// A pull request publishes the candidate.
    Published,
    /// Delivery finished without a change to publish.
    NoChange,
    /// Delivery stopped; [`CookDelivery::blocker`] says why.
    Blocked,
}

/// The closed set of reasons delivery can stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookDeliveryBlockerCode {
    GateFailed,
    GatesDeferred,
    BaseConflict,
    ForeignCommits,
    PublishRefused,
    Policy,
    Cancelled,
}

impl CookDeliveryBlockerCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GateFailed => "gate_failed",
            Self::GatesDeferred => "gates_deferred",
            Self::BaseConflict => "base_conflict",
            Self::ForeignCommits => "foreign_commits",
            Self::PublishRefused => "publish_refused",
            Self::Policy => "policy",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CookDeliveryCandidate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// The latest promotion's status. `no_op_gate_failed` reads as
    /// `no_changes_gate_failed`.
    pub promotion: AgentTaskPromotionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_artifact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub patch_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_base: Option<String>,
    /// The post-apply checkpoint proves this exact candidate reached its
    /// declared destination. A promotion status alone does not.
    pub target_applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CookDeliveryDestination {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CookDeliveryGate {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub state: AgentTaskGateStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub proof_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CookDeliveryPublication {
    /// The receipt status exactly as the publisher recorded it.
    pub status: String,
    /// A pull request receipt with a usable identity.
    pub published: bool,
    pub draft: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pushed_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
}

impl CookDeliveryPublication {
    /// The PR number when recorded, otherwise its URL.
    pub fn pr_identity(&self) -> Option<String> {
        self.pr_number
            .map(|number| number.to_string())
            .or_else(|| self.pr_url.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CookDeliveryBlocker {
    pub code: CookDeliveryBlockerCode,
    /// Names the step that stopped delivery.
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// The durable command recorded for this blocker, when one is legal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    /// The writer's own error code, when it recorded one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// JSON pointer into run metadata holding the blocker's evidence.
    pub evidence: String,
}

impl CookDelivery {
    /// Read delivery from a run's persisted metadata. `None` when the run has
    /// no delivery evidence (no promotion, publication or recovery record).
    pub fn from_run_metadata(metadata: &Value) -> Option<Self> {
        let (promotion_pointer, promotion) = latest_promotion(metadata);
        let promotion_status = promotion.and_then(promotion_status);
        let finalization = present(metadata, "/cook_finalization");
        let recovery = present(metadata, "/cook_moving_base_recovery");
        let manual_intent = present(metadata, "/manual_finalization_intent");
        let manual_failure = present(metadata, "/manual_finalization_failure");
        if promotion_status.is_none()
            && finalization.is_none()
            && recovery.is_none()
            && manual_intent.is_none()
            && manual_failure.is_none()
        {
            return None;
        }

        let checkpoint = present(metadata, "/cook_recovery_source_checkpoint");
        let candidate = promotion
            .zip(promotion_status)
            .map(|(promotion, status)| read_candidate(promotion, status, checkpoint, recovery));
        let gates = promotion.map(read_gates).unwrap_or_default();
        let publication = finalization.map(|finalization| {
            read_publication(
                finalization,
                present(metadata, "/cook_force_with_lease_receipt"),
            )
        });
        let destination = read_destination(promotion, finalization.or(manual_intent));
        let base_movements = recovery
            .and_then(|recovery| recovery.get("base_movements"))
            .and_then(Value::as_u64)
            .map_or(0, |moves| u32::try_from(moves).unwrap_or(u32::MAX));

        let blocked = |blocker: CookDeliveryBlocker| (CookDeliveryState::Blocked, Some(blocker));
        let (state, blocker) = if publication.as_ref().is_some_and(|p| p.published) {
            (CookDeliveryState::Published, None)
        } else if finalization.is_some_and(is_no_change_receipt) {
            (CookDeliveryState::NoChange, None)
        } else if matches!(
            promotion_status,
            Some(
                AgentTaskPromotionStatus::GateFailed
                    | AgentTaskPromotionStatus::NoChangesGateFailed
            )
        ) {
            blocked(gate_blocker(
                CookDeliveryBlockerCode::GateFailed,
                &gates,
                promotion_pointer,
            ))
        } else if promotion_status == Some(AgentTaskPromotionStatus::GateDeferred) {
            blocked(gate_blocker(
                CookDeliveryBlockerCode::GatesDeferred,
                &gates,
                promotion_pointer,
            ))
        } else if let Some(blocker) =
            recovery.and_then(|recovery| moving_base_blocker(recovery, base_movements, promotion))
        {
            blocked(blocker)
        } else if let Some(blocker) = manual_failure
            .map(manual_failure_blocker)
            .or_else(|| finalization.and_then(finalization_blocker))
        {
            blocked(blocker)
        } else {
            let state = match promotion_status {
                Some(AgentTaskPromotionStatus::Applied) => CookDeliveryState::Verified,
                Some(
                    AgentTaskPromotionStatus::VerifiedNoChanges
                    | AgentTaskPromotionStatus::NoChanges,
                ) => CookDeliveryState::NoChange,
                Some(AgentTaskPromotionStatus::VerificationPending) => CookDeliveryState::Applied,
                // Publication is only attempted after green gates.
                _ if publication.is_some() => CookDeliveryState::Verified,
                _ => CookDeliveryState::Applied,
            };
            (state, None)
        };

        Some(Self {
            state,
            candidate,
            destination,
            gates,
            publication,
            base_movements,
            blocker,
        })
    }

    pub fn blocker_code(&self) -> Option<CookDeliveryBlockerCode> {
        self.blocker.as_ref().map(|blocker| blocker.code)
    }

    /// The candidate's promotion status, when a promotion was recorded.
    pub fn promotion(&self) -> Option<AgentTaskPromotionStatus> {
        self.candidate.as_ref().map(|candidate| candidate.promotion)
    }
}

fn present<'a>(metadata: &'a Value, pointer: &str) -> Option<&'a Value> {
    metadata.pointer(pointer).filter(|value| !value.is_null())
}

fn string_at(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// `latest_promotion`, or the last entry of the `promotions` history when a
/// record predates the latest-promotion key.
fn latest_promotion(metadata: &Value) -> (String, Option<&Value>) {
    if let Some(promotion) = present(metadata, "/latest_promotion") {
        return ("/latest_promotion".to_string(), Some(promotion));
    }
    metadata
        .get("promotions")
        .and_then(Value::as_array)
        .and_then(|history| {
            let index = history.len().checked_sub(1)?;
            Some((format!("/promotions/{index}"), history.get(index)?))
        })
        .filter(|(_, promotion)| !promotion.is_null())
        .map_or(
            ("/latest_promotion".to_string(), None),
            |(pointer, promotion)| (pointer, Some(promotion)),
        )
}

/// Dry runs are not delivery, and an unknown status is not evidence.
fn promotion_status(promotion: &Value) -> Option<AgentTaskPromotionStatus> {
    let status = match promotion.get("status")?.as_str()? {
        "no_op_gate_failed" => AgentTaskPromotionStatus::NoChangesGateFailed,
        "gates_deferred" => AgentTaskPromotionStatus::GateDeferred,
        status => serde_json::from_value(Value::String(status.to_string())).ok()?,
    };
    (status != AgentTaskPromotionStatus::DryRun).then_some(status)
}

fn read_candidate(
    promotion: &Value,
    status: AgentTaskPromotionStatus,
    checkpoint: Option<&Value>,
    recovery: Option<&Value>,
) -> CookDeliveryCandidate {
    // The first post-apply checkpoint keeps the candidate's identity when a
    // later report omits it.
    let identity = |pointer: &str| {
        string_at(promotion, pointer)
            .or_else(|| checkpoint.and_then(|checkpoint| string_at(checkpoint, pointer)))
    };
    let patch_artifact = promotion
        .get("patch_artifact")
        .or_else(|| promotion.get("patch"))
        .and_then(|artifact| {
            string_at(artifact, "/id").or_else(|| string_at(artifact, "/artifact_id"))
        });
    let candidate_recorded = promotion
        .pointer("/provenance/candidate")
        .is_some_and(|candidate| !candidate.is_null());
    let target_applied = matches!(
        status,
        AgentTaskPromotionStatus::VerificationPending
            | AgentTaskPromotionStatus::Applied
            | AgentTaskPromotionStatus::GateFailed
    ) && promotion
        .pointer("/provenance/post_apply")
        .and_then(Value::as_bool)
        == Some(true)
        && string_at(promotion, "/patch_artifact/id").is_some()
        && destination_worktree(promotion).is_some()
        && candidate_recorded;
    CookDeliveryCandidate {
        task_id: string_at(promotion, "/task_id"),
        promotion: status,
        fingerprint: identity("/provenance/candidate/fingerprint/sha256"),
        patch_artifact,
        patch_sha: identity("/patch_artifact/sha256"),
        verified_base: identity("/verified_base/sha")
            .or_else(|| recovery.and_then(|recovery| string_at(recovery, "/prior_verified_base"))),
        target_applied,
    }
}

fn destination_worktree(promotion: &Value) -> Option<String> {
    string_at(promotion, "/target/worktree").or_else(|| string_at(promotion, "/to_worktree"))
}

fn read_destination(
    promotion: Option<&Value>,
    publication: Option<&Value>,
) -> Option<CookDeliveryDestination> {
    let from_promotion =
        |pointer: &str| promotion.and_then(|promotion| string_at(promotion, pointer));
    let from_publication =
        |pointer: &str| publication.and_then(|publication| string_at(publication, pointer));
    let destination = CookDeliveryDestination {
        worktree: promotion.and_then(destination_worktree),
        path: from_promotion("/target/path").or_else(|| from_publication("/path")),
        branch: from_promotion("/target/branch").or_else(|| from_publication("/head")),
        head: from_promotion("/target/head"),
    };
    (destination
        != CookDeliveryDestination {
            worktree: None,
            path: None,
            branch: None,
            head: None,
        })
    .then_some(destination)
}

/// Detailed gate reports are authoritative; the normalized `gate_results`
/// view is read only when a report carries no detailed gates.
fn read_gates(promotion: &Value) -> Vec<CookDeliveryGate> {
    promotion
        .get("deterministic_gates")
        .or_else(|| promotion.get("gate_results"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|gate| {
            let state = gate
                .get("status")
                .or_else(|| gate.get("state"))
                .and_then(Value::as_str)
                .and_then(gate_status)?;
            let proof_refs = ["/capture/stdout/sha256", "/capture/stderr/sha256"]
                .into_iter()
                .filter_map(|pointer| string_at(gate, pointer))
                .map(|sha| format!("sha256:{sha}"))
                .collect();
            Some(CookDeliveryGate {
                id: gate_name(gate),
                state,
                summary: string_at(gate, "/failure_evidence/summary")
                    .or_else(|| string_at(gate, "/summary"))
                    .or_else(|| string_at(gate, "/message")),
                proof_refs,
            })
        })
        .collect()
}

/// Gate reports, normalized gate results and older fixtures spell the same
/// outcome differently.
fn gate_status(status: &str) -> Option<AgentTaskGateStatus> {
    match status {
        "passed" => Some(AgentTaskGateStatus::Succeeded),
        "failure" => Some(AgentTaskGateStatus::Failed),
        "blocked" => Some(AgentTaskGateStatus::Unavailable),
        status => serde_json::from_value(Value::String(status.to_string())).ok(),
    }
}

/// A production gate report is named by `id` and `command: Vec<String>`;
/// normalized results and older reports carry `name` or a string command
/// (#14735).
fn gate_name(gate: &Value) -> Option<String> {
    string_at(gate, "/id")
        .or_else(|| string_at(gate, "/name"))
        .or_else(|| string_at(gate, "/failure_evidence/command"))
        .or_else(|| match gate.get("command")? {
            Value::String(command) => Some(command.clone()),
            Value::Array(parts) => {
                let joined = parts
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ");
                (!joined.is_empty()).then_some(joined)
            }
            _ => None,
        })
}

fn gate_blocker(
    code: CookDeliveryBlockerCode,
    gates: &[CookDeliveryGate],
    evidence: String,
) -> CookDeliveryBlocker {
    let (gate_state, verb) = match code {
        CookDeliveryBlockerCode::GatesDeferred => (AgentTaskGateStatus::Deferred, "deferred"),
        _ => (AgentTaskGateStatus::Failed, "failed"),
    };
    let gate = gates.iter().find(|gate| gate.state == gate_state);
    let named = gate.and_then(|gate| gate.id.as_deref());
    let summary = gate.and_then(|gate| gate.summary.as_deref());
    let message = match (named, summary, code) {
        (Some(id), Some(summary), _) => format!("deterministic gate `{id}` {verb}: {summary}"),
        (Some(id), None, CookDeliveryBlockerCode::GatesDeferred) => format!(
            "deterministic gate `{id}` deferred because no runner could execute it; the candidate is unverified"
        ),
        (Some(id), None, _) => format!("deterministic gate `{id}` failed"),
        (None, Some(summary), _) => summary.to_string(),
        (None, None, CookDeliveryBlockerCode::GatesDeferred) => {
            "deterministic gates deferred; the candidate is unverified".to_string()
        }
        (None, None, _) => "deterministic gates rejected the candidate".to_string(),
    };
    CookDeliveryBlocker {
        code,
        message,
        paths: Vec::new(),
        next_action: None,
        cause: None,
        evidence,
    }
}

fn moving_base_blocker(
    recovery: &Value,
    base_movements: u32,
    promotion: Option<&Value>,
) -> Option<CookDeliveryBlocker> {
    let message = string_at(recovery, "/blocker")?;
    // The writer gives up on a destination that changed outside the
    // controller; it is foreign work, not a base that moved.
    let code = if message.contains("differs from the exact promoted candidate") {
        CookDeliveryBlockerCode::ForeignCommits
    } else {
        CookDeliveryBlockerCode::BaseConflict
    };
    let paths = recovery
        .pointer("/promotion/changed_files")
        .or_else(|| promotion.and_then(|promotion| promotion.get("changed_files")))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    let next_action = (code == CookDeliveryBlockerCode::BaseConflict
        && base_movements < BASE_MOVEMENT_BUDGET)
        .then(|| string_at(recovery, "/continuation"))
        .flatten();
    Some(CookDeliveryBlocker {
        code,
        message,
        paths,
        next_action,
        cause: None,
        evidence: "/cook_moving_base_recovery".to_string(),
    })
}

fn manual_failure_blocker(failure: &Value) -> CookDeliveryBlocker {
    CookDeliveryBlocker {
        code: CookDeliveryBlockerCode::PublishRefused,
        message: string_at(failure, "/error/message")
            .unwrap_or_else(|| "manual publication failed".to_string()),
        paths: Vec::new(),
        next_action: None,
        cause: string_at(failure, "/error/code"),
        evidence: "/manual_finalization_failure".to_string(),
    }
}

fn finalization_blocker(finalization: &Value) -> Option<CookDeliveryBlocker> {
    let status = finalization.get("status").and_then(Value::as_str)?;
    matches!(status, "failed" | "finalization_failed").then(|| CookDeliveryBlocker {
        code: CookDeliveryBlockerCode::PublishRefused,
        message: string_at(finalization, "/message")
            .unwrap_or_else(|| "Cook finalization failed after promotion.".to_string()),
        paths: Vec::new(),
        next_action: None,
        cause: string_at(finalization, "/code"),
        evidence: "/cook_finalization".to_string(),
    })
}

fn is_no_change_receipt(finalization: &Value) -> bool {
    finalization.get("status").and_then(Value::as_str) == Some("intentional_no_change_finalized")
}

/// The publication a single `cook_finalization` receipt proves, for callers
/// that hold the receipt rather than the run metadata.
pub(crate) fn publication_receipt(finalization: &Value) -> CookDeliveryPublication {
    read_publication(finalization, None)
}

fn read_publication(
    finalization: &Value,
    lease_receipt: Option<&Value>,
) -> CookDeliveryPublication {
    let status = string_at(finalization, "/status").unwrap_or_default();
    // Receipts have recorded the PR identity at several paths. A malformed
    // field never hides a valid identity at another.
    let pr_number = ["/pr_number", "/pr/number"]
        .into_iter()
        .find_map(|pointer| {
            finalization
                .pointer(pointer)?
                .as_u64()
                .filter(|number| *number > 0)
        });
    let pr_url = ["/pr_url", "/pull_request_url", "/pr/url"]
        .into_iter()
        .find_map(|pointer| string_at(finalization, pointer));
    let receipt = matches!(status.as_str(), "review_ready" | "draft_published");
    CookDeliveryPublication {
        published: receipt && (pr_number.is_some() || pr_url.is_some()),
        draft: status == "draft_published",
        commit: string_at(finalization, "/publication_proof/binding/candidate_sha")
            .or_else(|| string_at(finalization, "/publication_proof/git_identity/commit_sha")),
        pushed_sha: string_at(finalization, "/publication_proof/binding/remote_sha")
            .or_else(|| {
                string_at(
                    finalization,
                    "/publication_proof/git_tracking/verified_remote_sha",
                )
            })
            .or_else(|| lease_receipt.and_then(|receipt| string_at(receipt, "/remote_sha"))),
        pr_number,
        pr_url,
        status,
    }
}

#[cfg(test)]
#[path = "cook_delivery_tests.rs"]
mod tests;
