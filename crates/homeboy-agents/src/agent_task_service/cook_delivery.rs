//! The Cook delivery record (#16049).
//!
//! Delivery is everything after the provider finishes: apply, gates, base
//! convergence, commit, push and PR. Its facts are persisted as one versioned
//! `cook_delivery` record (schema [`COOK_DELIVERY_SCHEMA`]) that the lifecycle
//! store's single delivery writer keeps current beside the older per-stage
//! keys. [`CookDelivery::from_run_metadata`] reads that record, and falls back
//! to deriving the same value from the per-stage keys (and their historical
//! spellings) for runs recorded before it existed. Reporters read the typed
//! result instead of the raw JSON, so they agree on one answer.
//!
//! [`CookDelivery::lifecycle`] is the one place a delivery decides what it
//! contributes to its run's state and its Cook's status.
//!
//! The delivery steps run it (#16049 slice 3): [`ensure_verified`] applies the
//! candidate and runs its gates, and [`ensure_published`] converges the base
//! and finalizes. Each step either continues with its result or ends the Cook
//! with the spine's report. The spine calls them around `evaluate_cook_loop`,
//! since loop feedback decides whether a verified candidate is publishable;
//! [`publish_accepted`] finalizes policy-accepted ones.

use std::ops::ControlFlow;

use homeboy_core::cook_status::{CookDisposition, CookStatus};
use homeboy_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::cook::*;
use super::cook_baseline::compare_gate_failures_to_verified_base;
use super::cook_promotion::*;
use super::cook_recipe::CookRecipeStore;
use super::AgentTaskRunResult;
use crate::agent_task_gate::AgentTaskGateStatus;
use crate::agent_task_lifecycle::{
    self, AgentTaskLifecycleStore, AgentTaskRunRecord, AgentTaskRunState,
};
use crate::agent_task_promotion::{AgentTaskPromotionReport, AgentTaskPromotionStatus};
use crate::agent_task_timeout::expired_cook_deadline;

/// Schema of the persisted `cook_delivery` run-metadata record.
pub const COOK_DELIVERY_SCHEMA: &str = "homeboy/cook-delivery/v1";

/// Run-metadata key of the persisted delivery record.
pub(crate) const COOK_DELIVERY_KEY: &str = "cook_delivery";

/// Moving-base recovery stops retrying after this many observed base moves.
/// Mirrors the writer's continuation budget in the Cook spine.
const BASE_MOVEMENT_BUDGET: u32 = 3;

/// One typed read of where a Cook's candidate is on its way to publication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookDelivery {
    pub state: CookDeliveryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<CookDeliveryCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<CookDeliveryDestination>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<CookDeliveryGate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<CookDeliveryPublication>,
    #[serde(default)]
    pub base_movements: u32,
    /// Present exactly when `state` is [`CookDeliveryState::Blocked`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocker: Option<CookDeliveryBlocker>,
}

/// The persisted `cook_delivery` record: the delivery under a schema tag.
#[derive(Serialize, Deserialize)]
struct CookDeliveryRecord {
    schema: String,
    #[serde(flatten)]
    delivery: CookDelivery,
}

/// What a delivery contributes to its run's lifecycle, decided only by
/// [`CookDelivery::lifecycle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookDeliveryLifecycle {
    /// The run state this delivery asks for. `CandidateRecoverable` while the
    /// candidate is unverified or rejected; `Succeeded` once it is verified.
    /// `None` when delivery says nothing about the run state.
    pub run_state: Option<AgentTaskRunState>,
    /// The Cook status this delivery reports on its own, if any.
    pub cook_status: Option<CookStatus>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookDeliveryCandidate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// The latest promotion's status. `no_op_gate_failed` reads as
    /// `no_changes_gate_failed`.
    pub promotion: AgentTaskPromotionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_artifact: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_base: Option<String>,
    /// The post-apply checkpoint proves this exact candidate reached its
    /// declared destination. A promotion status alone does not.
    pub target_applied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookDeliveryDestination {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookDeliveryGate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub state: AgentTaskGateStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proof_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookDeliveryPublication {
    /// The receipt status exactly as the publisher recorded it.
    pub status: String,
    /// A pull request receipt with a usable identity.
    pub published: bool,
    pub draft: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pushed_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookDeliveryBlocker {
    pub code: CookDeliveryBlockerCode,
    /// Names the step that stopped delivery.
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// The durable command recorded for this blocker, when one is legal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_action: Option<String>,
    /// The writer's own error code, when it recorded one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    /// JSON pointer into run metadata holding the blocker's evidence.
    pub evidence: String,
}

impl CookDelivery {
    /// Read delivery from a run's persisted metadata. `None` when the run has
    /// no delivery evidence (no promotion, publication or recovery record).
    ///
    /// The persisted `cook_delivery` record is authoritative when present and
    /// valid; otherwise delivery is derived from the per-stage keys.
    pub fn from_run_metadata(metadata: &Value) -> Option<Self> {
        match present(metadata, "/cook_delivery") {
            Some(record) => {
                Self::from_record(record).or_else(|| Self::from_legacy_metadata(metadata))
            }
            None => Self::from_legacy_metadata(metadata),
        }
    }

    /// The delivery a persisted `cook_delivery` record holds, if it is one this
    /// binary understands.
    fn from_record(record: &Value) -> Option<Self> {
        serde_json::from_value::<CookDeliveryRecord>(record.clone())
            .ok()
            .filter(|record| record.schema == COOK_DELIVERY_SCHEMA)
            .map(|record| record.delivery)
    }

    /// The versioned `cook_delivery` record persisting this delivery.
    pub(crate) fn to_record(&self) -> Value {
        serde_json::to_value(CookDeliveryRecord {
            schema: COOK_DELIVERY_SCHEMA.to_string(),
            delivery: self.clone(),
        })
        .expect("a Cook delivery always serializes")
    }

    /// Derive delivery from the per-stage keys the delivery writers persist
    /// beside the record. The single writer projects the record from this, so
    /// the two always agree.
    pub(crate) fn from_legacy_metadata(metadata: &Value) -> Option<Self> {
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

    /// What this delivery contributes to its run state and Cook status.
    ///
    /// Decided from [`Self::state`] and the blocker code alone, so every
    /// writer and reader agrees: an unverified (`applied`, `gates_deferred`)
    /// or rejected (`gate_failed`) candidate leaves its run recoverable; a
    /// verified or published one lets it succeed. Gate failures with and
    /// without a patch both report `gate_failed`-family Cook statuses; the
    /// no-patch one is the canonical `no_changes_gate_failed`.
    pub fn lifecycle(&self) -> CookDeliveryLifecycle {
        use CookDeliveryBlockerCode as Code;
        let recoverable = Some(AgentTaskRunState::CandidateRecoverable);
        let (run_state, cook_status) = match (self.state, self.blocker_code()) {
            (CookDeliveryState::Applied, _) if self.candidate.is_some() => (recoverable, None),
            (CookDeliveryState::Applied, _) => (None, None),
            (CookDeliveryState::Verified, _) => (Some(AgentTaskRunState::Succeeded), None),
            (CookDeliveryState::Published, _) => (
                Some(AgentTaskRunState::Succeeded),
                Some(match self.publication.as_ref().is_some_and(|p| p.draft) {
                    true => CookStatus::DraftPublished,
                    false => CookStatus::ReviewReady,
                }),
            ),
            (CookDeliveryState::NoChange, _) => (None, None),
            (CookDeliveryState::Blocked, Some(Code::GateFailed)) => (
                recoverable,
                Some(
                    match self.promotion() == Some(AgentTaskPromotionStatus::NoChangesGateFailed) {
                        true => CookStatus::NoChangesGateFailed,
                        false => CookStatus::GateFailed,
                    },
                ),
            ),
            (CookDeliveryState::Blocked, Some(Code::GatesDeferred)) => {
                (recoverable, Some(CookStatus::GatesDeferred))
            }
            (CookDeliveryState::Blocked, Some(Code::PublishRefused)) => (
                None,
                Some(CookStatus::Unknown("finalization_failed".to_string())),
            ),
            (CookDeliveryState::Blocked, _) => (None, None),
        };
        CookDeliveryLifecycle {
            run_state,
            cook_status,
        }
    }

    /// The candidate reached its destination but has not been verified:
    /// gates are pending or could not execute.
    pub fn is_unverified(&self) -> bool {
        self.state == CookDeliveryState::Applied
            || self.blocker_code() == Some(CookDeliveryBlockerCode::GatesDeferred)
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
    let status: AgentTaskPromotionStatus =
        serde_json::from_value(promotion.get("status")?.clone()).ok()?;
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

/// Everything a delivery step borrows from its Cook spine attempt.
pub(super) struct CookDeliveryContext<'a, 'e> {
    pub store: &'a CookRecipeStore,
    pub lifecycle_store: &'a AgentTaskLifecycleStore,
    pub options: &'a CookRequest,
    pub side_effects: &'a mut CookSideEffects<'e>,
    pub observer: Option<&'a CookProgressObserver<'a>>,
    pub cook_id: &'a str,
    pub run_id: &'a str,
    pub attempt: u32,
    pub attempts: &'a mut Vec<AgentTaskCookAttemptReport>,
}

/// A step's result: hand the value to the next step, or end the Cook.
pub(super) type CookDeliveryStep<T> = ControlFlow<AgentTaskRunResult<AgentTaskCookReport>, T>;

impl CookDeliveryContext<'_, '_> {
    fn progress(&self, phase: &str, detail: Option<&str>) -> Result<()> {
        let (store, observer) = (self.lifecycle_store, self.observer);
        report_cook_progress(
            store,
            observer,
            self.cook_id,
            self.run_id,
            phase,
            self.attempt,
            detail,
        )
    }

    fn push_attempt(
        &mut self,
        record: &AgentTaskRunRecord,
        promotion: Option<AgentTaskPromotionReport>,
    ) {
        self.attempts.push(AgentTaskCookAttemptReport {
            attempt: self.attempt,
            run_id: self.run_id.to_string(),
            run_state: format!("{:?}", record.state),
            aggregate_path: record.aggregate_path.clone(),
            promotion,
            feedback: None,
        });
    }

    /// End the Cook with this attempt's terminal report.
    fn report(
        &mut self,
        status: &str,
        finalization: Option<Value>,
        stop_reason: Option<&str>,
        exit_code: i32,
    ) -> AgentTaskRunResult<AgentTaskCookReport> {
        cook_report(CookReportInput {
            cook_id: self.cook_id.to_string(),
            status,
            disposition: CookDisposition::Terminal,
            attempts: std::mem::take(self.attempts),
            finalization,
            stop_reason: stop_reason.map(str::to_string),
            exit_code,
            invocation_latest_run_id: Some(self.run_id),
        })
    }

    fn moving_base_report(
        &mut self,
        recovery: MovingBaseCookRecovery,
        continuation_queued: bool,
    ) -> AgentTaskRunResult<AgentTaskCookReport> {
        moving_base_recovery_report(
            self.cook_id.to_string(),
            std::mem::take(self.attempts),
            recovery,
            continuation_queued,
            Some(self.run_id),
        )
    }
}

/// A finalization's status, and whether it published the candidate.
pub(super) fn publication_status(finalization: &Value) -> (String, bool) {
    let status = finalization["status"].as_str().unwrap_or("unknown");
    let published = matches!(status, "review_ready" | "draft_published");
    (status.to_string(), published)
}

fn to_json<T: Serialize>(value: &T) -> Result<Value> {
    serde_json::to_value(value).map_err(|error| Error::internal_json(error.to_string(), None))
}

/// Apply the candidate and run its gates (promotion). Continues with the
/// report, a gate-failed one compared against its verified base; ends the
/// Cook when another live owner holds the claim or promotion was rejected.
pub(super) fn ensure_verified(
    ctx: &mut CookDeliveryContext<'_, '_>,
    record: &AgentTaskRunRecord,
) -> Result<CookDeliveryStep<AgentTaskPromotionReport>> {
    let (lifecycle_store, options, observer) = (ctx.lifecycle_store, ctx.options, ctx.observer);
    let (cook_id, run_id, attempt) = (ctx.cook_id, ctx.run_id, ctx.attempt);
    // Gate dispatch boundary. Promotion is where the deterministic gates
    // run, and gates are the expensive half of the budget: five gates at
    // the default 30-minute timeout outlast the provider execution that
    // produced the candidate. Checked before dispatch so an expired budget
    // does not start a fresh multi-hour gate sweep.
    //
    // The provider's candidate is already durable at this point, so this
    // stops without discarding work: the Cook is recoverable by the normal
    // continuation route once the operator grants more budget.
    if let Some(deadline) = expired_cook_deadline() {
        ctx.push_attempt(record, None);
        let boundary = format!("attempt {attempt} could enter promotion and run its gates");
        let attempts = std::mem::take(ctx.attempts);
        let report =
            cook_deadline_report(cook_id.to_string(), attempts, run_id, &deadline, &boundary);
        return Ok(ControlFlow::Break(report));
    }
    ctx.progress("promotion", None)?;
    let mut promotion = match promote_with_gate_setup_retries(
        ctx.side_effects,
        lifecycle_store,
        options,
        run_id,
        |retry, delay, error| {
            let _ = report_cook_progress(
                lifecycle_store,
                observer,
                cook_id,
                run_id,
                "promotion",
                attempt,
                Some(&format!(
                    "gate setup failed; retrying promotion automatically ({retry} of {GATE_SETUP_PROMOTION_RETRIES}) in {}s: {}",
                    delay.as_secs(),
                    error.message
                )),
            );
        },
    ) {
        Ok(report) => report,
        Err(error) if promotion_claim_held_by_live_owner(lifecycle_store, run_id, &error) => {
            // Another live owner holds the exactly-once promotion claim and
            // is still running the gates. It owns the outcome and its
            // terminal notification; this pass must not record a
            // controller failure or declare the Cook terminal (#15472).
            ctx.push_attempt(record, None);
            return Ok(ControlFlow::Break(cook_report(CookReportInput {
                cook_id: cook_id.to_string(),
                status: CookStatus::InFlight.as_str(),
                disposition: CookDisposition::InFlight,
                attempts: std::mem::take(ctx.attempts),
                finalization: None,
                stop_reason: Some(format!(
                    "promotion is in progress under its live owner; watch with `homeboy agent-task status {run_id}`"
                )),
                exit_code: 0,
                invocation_latest_run_id: Some(run_id),
            })));
        }
        Err(error) => {
            ctx.push_attempt(record, None);
            let selection_required = error.details["selection_required"] == Value::Bool(true);
            let recovery = if selection_required {
                agent_task_lifecycle::record_metadata_value_in_store(
                    lifecycle_store,
                    run_id,
                    "cook_selection_required",
                    error.details.clone(),
                )?;
                "Cook found distinct canonical patch candidates. Select one of the exact commands in failure_context before promotion.".to_string()
            } else {
                "promotion provider response was rejected. The successful candidate remains durable; use failure_context to inspect or continue the Cook.".to_string()
            };
            agent_task_lifecycle::record_cook_controller_failure_in_store(
                lifecycle_store,
                run_id,
                &bounded_error_diagnostic(&error),
            )?;
            let status = if selection_required {
                "selection_required"
            } else {
                "durable_failure"
            };
            let report = ctx.report(status, None, Some(&recovery), 1);
            return Ok(ControlFlow::Break(report));
        }
    };
    if promotion.status.gate_failed() {
        let base_sha = promotion
            .verified_base
            .as_ref()
            .map(|base| base.sha.clone())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "promotion.verified_base",
                    "gate-failed Cook promotion has no immutable verified base for differential verification",
                    None,
                    None,
                )
            })?;
        let repository_root = promotion.target.path.clone().ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.target.path",
                "gate-failed Cook promotion has no candidate repository path for differential verification",
                None,
                None,
            )
        })?;
        // The differential replays run after the promotion's supervision
        // scope closed. Reopen the declared scope so a Lab runner enforces
        // the declared gate budgets instead of its defaults (#15856).
        crate::agent_task_promotion::with_gate_supervision(
            options.gates.declared_supervision(),
            || {
                compare_gate_failures_to_verified_base(
                    &mut promotion,
                    std::path::Path::new(&repository_root),
                    std::path::Path::new(&repository_root),
                    &base_sha,
                    options.gates.gate_timeout(),
                    options
                        .gates
                        .gate_environment
                        .admitted_component_id
                        .as_deref(),
                    |_compared, _total| Ok(()),
                )
            },
        )?;
        lifecycle_store.record_promotion(run_id, to_json(&promotion)?)?;
    }
    Ok(ControlFlow::Continue(promotion))
}

/// Converge the base and publish a green candidate (commit, push, PR).
/// `--no-finalize` stops before any effect; a pending moving-base recovery is
/// rebased and re-gated first.
pub(super) fn ensure_published(
    ctx: &mut CookDeliveryContext<'_, '_>,
    promotion: AgentTaskPromotionReport,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    let (store, lifecycle_store, options) = (ctx.store, ctx.lifecycle_store, ctx.options);
    let (cook_id, run_id) = (ctx.cook_id, ctx.run_id);
    if options.finalization.no_finalize {
        return Ok(ctx.report(
            "green_no_finalize",
            None,
            Some(
                "deterministic gates completed green; --no-finalize skipped \
     commit, push, and PR finalization",
            ),
            0,
        ));
    }
    let mut active_moving_base_recovery = None;
    let promotion = match moving_base_recovery_for_run_with_stores(store, lifecycle_store, run_id)?
    {
        Some(recovery) => {
            match (ctx.side_effects).recover_moving_base(lifecycle_store, options, &recovery) {
                Ok(promotion) => {
                    lifecycle_store.record_promotion(run_id, to_json(&promotion)?)?;
                    let mut recovery = refreshed_moving_base_recovery(recovery, &promotion);
                    lifecycle_store
                        .record_cook_moving_base_recovery(run_id, to_json(&recovery)?)?;
                    if promotion.status != AgentTaskPromotionStatus::Applied {
                        recovery.blocker = format!(
                            "rebased candidate did not pass the declared deterministic gates ({:?}); finalization was not attempted",
                            promotion.status
                        );
                        lifecycle_store
                            .record_cook_moving_base_recovery(run_id, to_json(&recovery)?)?;
                        return Ok(ctx.moving_base_report(recovery, false));
                    }
                    active_moving_base_recovery = Some(recovery);
                    promotion
                }
                Err(error) => {
                    let recovery = next_moving_base_recovery(recovery, error.to_string());
                    lifecycle_store
                        .record_cook_moving_base_recovery(run_id, to_json(&recovery)?)?;
                    let continuation_queued = queue_moving_base_continuation(store, &recovery)?;
                    return Ok(ctx.moving_base_report(recovery, continuation_queued));
                }
            }
        }
        None => promotion,
    };
    ctx.progress("finalization", None)?;
    let finalization = match ctx
        .side_effects
        .finalize(lifecycle_store, options, run_id, &promotion)
    {
        Ok(finalization) => {
            if active_moving_base_recovery.is_some() {
                lifecycle_store.clear_cook_moving_base_recovery(run_id)?;
            }
            finalization
        }
        Err(error) if is_moving_base_finalization_error(&error) => {
            let recovery = next_moving_base_recovery(
                active_moving_base_recovery.unwrap_or_else(|| {
                    moving_base_recovery_from_promotion_in_store(
                        lifecycle_store,
                        cook_id,
                        run_id,
                        promotion,
                    )
                }),
                error.to_string(),
            );
            lifecycle_store.record_cook_moving_base_recovery(run_id, to_json(&recovery)?)?;
            let continuation_queued = queue_moving_base_continuation(store, &recovery)?;
            return Ok(ctx.moving_base_report(recovery, continuation_queued));
        }
        Err(error) if error.message.contains("awaiting_acceptance") => {
            return Ok(ctx.report(
                "awaiting_acceptance",
                Some(serde_json::json!({
                    "status": "awaiting_acceptance",
                    "reason": error.message,
                    "run_id": run_id,
                    "status_command": cook_recovery_command_in_store(
                        lifecycle_store,
                        run_id,
                        &["status", run_id, "--full"],
                    ),
                })),
                Some(
                    "deterministic gates passed; an independent acceptance verdict \
     is required before PR finalization",
                ),
                1,
            ));
        }
        Err(error) => return Err(error),
    };
    let (final_status, published) = publication_status(&finalization);
    if published {
        let absorbed_source_run_id = promotion
            .provenance
            .pointer("/cook_follow_up/source_run_id")
            .and_then(Value::as_str)
            .unwrap_or(run_id);
        super::cook_recipe::complete_absorbed_review_form_follow_ups(
            store,
            lifecycle_store,
            absorbed_source_run_id,
        )?;
    }
    let stop_reason = (final_status == "no_changes").then_some(
        "cook completed provider execution and gates, but finalization found no changed files; task likely still requires review or retry",
    );
    let exit_code = if published { 0 } else { 1 };
    Ok(ctx.report(&final_status, Some(finalization), stop_reason, exit_code))
}

/// Publish a candidate a loop policy accepted off the green path (an existing
/// candidate behind a verified no-change review, or an accepted inherited
/// baseline-red gate). A published outcome reports `published_status` when set.
pub(super) fn publish_accepted(
    ctx: &mut CookDeliveryContext<'_, '_>,
    promotion: &AgentTaskPromotionReport,
    detail: &str,
    published_status: Option<&str>,
    stop_reason: Option<&str>,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>> {
    ctx.progress("finalization", Some(detail))?;
    let (lifecycle_store, options, run_id) = (ctx.lifecycle_store, ctx.options, ctx.run_id);
    let finalization = (ctx.side_effects).finalize(lifecycle_store, options, run_id, promotion)?;
    let (final_status, published) = publication_status(&finalization);
    let status = published_status
        .filter(|_| published)
        .unwrap_or(&final_status);
    let exit_code = if published { 0 } else { 1 };
    Ok(ctx.report(status, Some(finalization), stop_reason, exit_code))
}

#[cfg(test)]
#[path = "cook_delivery_tests.rs"]
mod tests;

#[cfg(test)]
mod step_tests {

    use super::super::cook::tests::{compile_options, promotion};
    use super::*;

    /// `--no-finalize` ends a green Cook before any publication effect.
    #[test]
    fn ensure_published_honors_no_finalize_without_finalizing() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let store = CookRecipeStore::from_current_data_root().expect("recipe store");
            let lifecycle_store =
                AgentTaskLifecycleStore::from_current_environment().expect("lifecycle store");
            let options = compile_options("delivery-no-finalize");
            assert!(options.finalization.no_finalize);
            let mut side_effects = CookSideEffects::for_test(
                |_, _, _| unreachable!("publication does not promote"),
                |_, _, _| unreachable!("no-finalize must not recover a moving base"),
                |_, _, _, _| unreachable!("no-finalize must not finalize"),
            );
            let mut attempts = Vec::new();
            let run_id = options.identity.initial_run_id.clone();

            let report = ensure_published(
                &mut CookDeliveryContext {
                    store: &store,
                    lifecycle_store: &lifecycle_store,
                    options: &options,
                    side_effects: &mut side_effects,
                    observer: None,
                    cook_id: &options.identity.cook_id,
                    run_id: &run_id,
                    attempt: 1,
                    attempts: &mut attempts,
                },
                promotion(&run_id),
            )
            .expect("no-finalize publication");

            assert_eq!(report.value.status, "green_no_finalize");
            assert_eq!(report.exit_code, 0);
            assert!(report.value.finalization.is_none());
        });
    }
}
