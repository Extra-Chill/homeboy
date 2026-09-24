//! Per-connected-account provider capacity as typed readiness evidence
//! (#14858).
//!
//! Plain ready/unusable readiness cannot tell a route that is genuinely
//! unavailable apart from one that is authenticated and healthy but simply
//! out of capacity until a known reset instant. A live orchestration wave hit
//! exactly that ambiguity twice in one run: `providers --validate-readiness`
//! reported `credentials_unusable`/`credentials_unverified` for a route that
//! was actually fine, and a later dispatch exhausted mid-run with only an
//! untyped stream error, because neither had a distinct place to say
//! "capacity" instead of "auth" or "runtime".
//!
//! This module owns the shape of that answer. It is deliberately provider
//! neutral: capacity reporting is part of the existing
//! [`super::command_runner::ProviderReadinessInvocationResult`] contract
//! (an optional `capacity` object), not vendor knowledge hardcoded in core. A
//! provider that never fills in `capacity` is not penalized for it — the
//! evidence here reports `Unknown` with a reason, and the route stays exactly
//! as dispatchable as it already was.
//!
//! Evidence built here rides inside
//! [`super::dispatchability::AgentTaskProviderRuntimeEvidence`], so it shares
//! that struct's identity/freshness discipline with the existing
//! [`super::ProviderRuntimeReadinessCache`] (#14703): capacity data costs
//! nothing beyond the probe Homeboy was already caching.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::command_runner::{
    ProviderReadinessInvocationAccountCapacity, ProviderReadinessInvocationResult,
};
use super::usage_cap::reset_at_from_outcome;
use crate::agent_task::{AgentTaskFailureClassification, AgentTaskOutcome};

/// Runtime-readiness classification a provider-declared readiness invocation
/// uses to mean "this route is authenticated and otherwise fine, but the
/// connected account has no capacity left right now". Distinct from
/// `auth_failure` (bad/rejected credentials) and `account` (billing/account
/// block), matching the vocabulary already accepted by
/// `readiness_error`/`sanitize_classification`.
pub const PROVIDER_READINESS_CAPACITY_CLASSIFICATION: &str = "capacity";

/// One connected account/route's capacity, as far as Homeboy can observe it
/// before dispatch. `remaining`/`limit` are opaque provider-defined numbers
/// (whatever unit the provider names) rather than a Homeboy-invented scale,
/// because capacity semantics vary by provider (requests, tokens, a
/// percentage, a currency amount, ...).
///
/// `reset_at` fields are RFC 3339 strings rather than `chrono::DateTime`
/// directly, matching the rest of this crate's serialized evidence (e.g.
/// `ProviderRouteEvidence`): the workspace does not enable chrono's `serde`
/// feature, and every other on-wire reset instant already takes this shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AgentTaskProviderCapacityReadiness {
    /// The provider publishes no capacity data for this probe. This is not
    /// evidence the route is unavailable — silence is not a synonym for
    /// exhausted, and the route stays exactly as dispatchable as its other
    /// checks already say.
    Unknown { reason: String },
    /// The provider reported capacity and has not signaled exhaustion.
    Known {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remaining: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reset_at: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        accounts: Vec<AgentTaskProviderCapacityAccount>,
    },
    /// The connected account/route is presently out of capacity. `reset_at`
    /// is populated only when the provider (or Homeboy's own detection of
    /// its output) actually supplied a reset instant — an exhausted route
    /// with an unknown reset is still reported as exhausted, just without a
    /// resume time attached.
    Exhausted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reset_at: Option<String>,
        reason: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        accounts: Vec<AgentTaskProviderCapacityAccount>,
    },
}

/// Capacity of one connected account behind a route that rotates across
/// several (for example a pool of subscription plans), so an operator can
/// see every plan's state and reset instant, not only the route summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskProviderCapacityAccount {
    pub account: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<String>,
}

/// Upper bound on reported accounts so a misbehaving provider cannot flood
/// readiness evidence.
const MAX_CAPACITY_ACCOUNTS: usize = 64;

impl Default for AgentTaskProviderCapacityReadiness {
    fn default() -> Self {
        Self::Unknown {
            reason: "capacity was not evaluated".to_string(),
        }
    }
}

impl AgentTaskProviderCapacityReadiness {
    pub fn is_exhausted(&self) -> bool {
        matches!(self, Self::Exhausted { .. })
    }

    /// Per-account breakdown, empty when the provider reports only a route
    /// summary.
    pub fn accounts(&self) -> &[AgentTaskProviderCapacityAccount] {
        match self {
            Self::Known { accounts, .. } | Self::Exhausted { accounts, .. } => accounts,
            Self::Unknown { .. } => &[],
        }
    }

    /// The reset instant, parsed, when this evidence carries one that parses
    /// as RFC 3339. A present-but-unparseable string degrades to `None`
    /// rather than panicking or failing the caller.
    pub fn reset_at(&self) -> Option<DateTime<Utc>> {
        let raw = match self {
            Self::Known { reset_at, .. } | Self::Exhausted { reset_at, .. } => reset_at.as_deref(),
            Self::Unknown { .. } => None,
        }?;
        DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|value| value.with_timezone(&Utc))
    }

    fn unknown(reason: impl Into<String>) -> Self {
        Self::Unknown {
            reason: reason.into(),
        }
    }
}

/// Build typed capacity readiness from one completed provider readiness
/// probe. Called only when the probe actually ran (see
/// [`capacity_readiness_unknown`] for every case where it did not).
pub fn capacity_readiness_from_probe_result(
    verdict: &ProviderReadinessInvocationResult,
) -> AgentTaskProviderCapacityReadiness {
    let reset_at = normalized_reset_at(
        verdict
            .capacity
            .as_ref()
            .and_then(|capacity| capacity.reset_at.as_deref()),
    );
    let accounts = verdict
        .capacity
        .as_ref()
        .map(|capacity| capacity_accounts(&capacity.accounts))
        .unwrap_or_default();
    if verdict.classification.trim() == PROVIDER_READINESS_CAPACITY_CLASSIFICATION {
        let reason = if verdict.reason.trim().is_empty() {
            "the provider reported its account capacity as exhausted".to_string()
        } else {
            homeboy_core::redaction::redact_string(&verdict.reason)
        };
        return AgentTaskProviderCapacityReadiness::Exhausted {
            reset_at,
            reason,
            accounts,
        };
    }
    match verdict.capacity.as_ref() {
        Some(capacity) if capacity.remaining.is_some() || capacity.limit.is_some() => {
            AgentTaskProviderCapacityReadiness::Known {
                remaining: capacity.remaining.clone(),
                limit: capacity.limit.clone(),
                unit: capacity.unit.clone(),
                reset_at,
                accounts,
            }
        }
        _ => AgentTaskProviderCapacityReadiness::unknown(
            "the provider's readiness invocation does not report capacity",
        ),
    }
}

// Round-trip through `DateTime` only to validate the provider's string is
// actually a parseable RFC 3339 instant; an unparseable value is dropped
// rather than stored as an unusable reset time.
fn normalized_reset_at(value: Option<&str>) -> Option<String> {
    value
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc).to_rfc3339())
}

fn capacity_accounts(
    accounts: &[ProviderReadinessInvocationAccountCapacity],
) -> Vec<AgentTaskProviderCapacityAccount> {
    accounts
        .iter()
        .take(MAX_CAPACITY_ACCOUNTS)
        .enumerate()
        .map(|(index, account)| {
            let label = account.account.trim();
            let state = account.state.trim();
            AgentTaskProviderCapacityAccount {
                account: if label.is_empty() {
                    format!("account#{index}")
                } else {
                    homeboy_core::redaction::redact_string(label)
                },
                state: if state.is_empty() {
                    "unknown".to_string()
                } else {
                    homeboy_core::redaction::redact_string(state)
                },
                remaining: account.remaining.clone().filter(|value| value.is_number()),
                reset_at: normalized_reset_at(account.reset_at.as_deref()),
            }
        })
        .collect()
}

/// Capacity readiness for a route whose live probe did not run (not
/// requested, no `readiness_invocation` declared, or an earlier structural
/// check already failed). Absence of a probe is itself a reason, named
/// explicitly rather than left for a caller to infer from a missing field.
pub fn capacity_readiness_unknown(reason: impl Into<String>) -> AgentTaskProviderCapacityReadiness {
    AgentTaskProviderCapacityReadiness::unknown(reason)
}

/// Typed capacity evidence for a task that already failed mid-run, so an
/// operator (or the scheduler's retry/rotation budget) reads an actionable
/// cause instead of an opaque provider error (#14858).
///
/// Reuses the existing provider-quota classification
/// ([`AgentTaskFailureClassification::ProviderQuotaExhausted`], set by the
/// structured-error adapter, #13703) and usage-cap reset detection
/// ([`super::usage_cap::reset_at_from_outcome`], #13644) rather than
/// duplicating either: this is the same causal signal already recorded on the
/// outcome, read through the same typed shape pre-dispatch capacity evidence
/// uses. Returns `None` for any outcome that was not classified as capacity
/// exhaustion — this is deliberately narrower than "any failure", so a
/// generic provider error is never silently reported as capacity.
pub fn capacity_readiness_from_outcome(
    outcome: &AgentTaskOutcome,
) -> Option<AgentTaskProviderCapacityReadiness> {
    if outcome.failure_classification
        != Some(AgentTaskFailureClassification::ProviderQuotaExhausted)
    {
        return None;
    }
    let reset_at = reset_at_from_outcome(outcome).map(|value| value.to_rfc3339());
    let reason = outcome
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.class == "provider.capacity_rejected")
        .and_then(|diagnostic| diagnostic.data.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| outcome.summary.clone())
        .filter(|reason| !reason.trim().is_empty())
        .unwrap_or_else(|| "the provider reported its account capacity as exhausted".to_string());
    Some(AgentTaskProviderCapacityReadiness::Exhausted {
        reset_at,
        reason,
        accounts: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task_provider::command_runner::ProviderReadinessInvocationCapacity;
    use chrono::TimeZone;

    fn verdict(
        ready: bool,
        classification: &str,
        reason: &str,
        capacity: Option<ProviderReadinessInvocationCapacity>,
    ) -> ProviderReadinessInvocationResult {
        ProviderReadinessInvocationResult {
            ready,
            classification: classification.to_string(),
            retryable: false,
            remediation: String::new(),
            reason: reason.to_string(),
            cache_key: "test".to_string(),
            identity: Value::Null,
            capacity,
        }
    }

    #[test]
    fn known_capacity_is_reported_with_its_remaining_and_limit() {
        let readiness = capacity_readiness_from_probe_result(&verdict(
            true,
            "ready",
            "",
            Some(ProviderReadinessInvocationCapacity {
                remaining: Some(Value::from(42)),
                limit: Some(Value::from(100)),
                unit: Some("requests".to_string()),
                reset_at: None,
                accounts: Vec::new(),
            }),
        ));
        assert_eq!(
            readiness,
            AgentTaskProviderCapacityReadiness::Known {
                remaining: Some(Value::from(42)),
                limit: Some(Value::from(100)),
                unit: Some("requests".to_string()),
                reset_at: None,
                accounts: Vec::new(),
            }
        );
        assert!(!readiness.is_exhausted());
    }

    #[test]
    fn exhausted_capacity_carries_its_parsed_reset_instant() {
        let readiness = capacity_readiness_from_probe_result(&verdict(
            false,
            "capacity",
            "5-hour usage limit reached",
            Some(ProviderReadinessInvocationCapacity {
                remaining: Some(Value::from(0)),
                limit: None,
                unit: None,
                reset_at: Some("2026-08-27T12:37:03Z".to_string()),
                accounts: Vec::new(),
            }),
        ));
        let expected_reset_at = chrono::Utc
            .with_ymd_and_hms(2026, 8, 27, 12, 37, 3)
            .single()
            .unwrap();
        assert_eq!(
            readiness,
            AgentTaskProviderCapacityReadiness::Exhausted {
                reset_at: Some(expected_reset_at.to_rfc3339()),
                reason: "5-hour usage limit reached".to_string(),
                accounts: Vec::new(),
            }
        );
        assert!(readiness.is_exhausted());
        assert_eq!(readiness.reset_at(), Some(expected_reset_at));
    }

    #[test]
    fn exhausted_capacity_without_a_known_reset_still_types_as_exhausted() {
        let readiness = capacity_readiness_from_probe_result(&verdict(
            false,
            "capacity",
            "no quota left",
            None,
        ));
        assert_eq!(
            readiness,
            AgentTaskProviderCapacityReadiness::Exhausted {
                reset_at: None,
                reason: "no quota left".to_string(),
                accounts: Vec::new(),
            }
        );
        assert!(readiness.is_exhausted());
        assert_eq!(readiness.reset_at(), None);
    }

    fn account(
        label: &str,
        state: &str,
        remaining: Option<Value>,
        reset_at: Option<&str>,
    ) -> ProviderReadinessInvocationAccountCapacity {
        ProviderReadinessInvocationAccountCapacity {
            account: label.to_string(),
            state: state.to_string(),
            remaining,
            reset_at: reset_at.map(str::to_string),
        }
    }

    #[test]
    fn pooled_accounts_are_reported_per_account_with_normalized_reset_instants() {
        let readiness = capacity_readiness_from_probe_result(&verdict(
            true,
            "ready",
            "",
            Some(ProviderReadinessInvocationCapacity {
                remaining: Some(Value::from(91)),
                limit: Some(Value::from(100)),
                unit: Some("percent".to_string()),
                reset_at: Some("2026-09-24T15:50:00Z".to_string()),
                accounts: vec![
                    account(
                        "plan-a@example.com",
                        "exhausted",
                        Some(Value::from(0)),
                        Some("2026-09-25T03:00:00.307Z"),
                    ),
                    account(
                        "plan-b",
                        "available",
                        Some(Value::from(91)),
                        Some("2026-09-24T15:50:00Z"),
                    ),
                    account(
                        "",
                        "",
                        Some(Value::from("not a number")),
                        Some("not a timestamp"),
                    ),
                ],
            }),
        ));
        assert_eq!(
            readiness.accounts(),
            &[
                AgentTaskProviderCapacityAccount {
                    account: "plan-a@example.com".to_string(),
                    state: "exhausted".to_string(),
                    remaining: Some(Value::from(0)),
                    reset_at: Some("2026-09-25T03:00:00.307+00:00".to_string()),
                },
                AgentTaskProviderCapacityAccount {
                    account: "plan-b".to_string(),
                    state: "available".to_string(),
                    remaining: Some(Value::from(91)),
                    reset_at: Some("2026-09-24T15:50:00+00:00".to_string()),
                },
                AgentTaskProviderCapacityAccount {
                    account: "account#2".to_string(),
                    state: "unknown".to_string(),
                    remaining: None,
                    reset_at: None,
                },
            ]
        );
    }

    #[test]
    fn an_exhausted_pool_keeps_every_account_and_bounds_the_list() {
        let accounts = (0..100)
            .map(|index| {
                account(
                    &format!("plan-{index}"),
                    "exhausted",
                    Some(Value::from(0)),
                    None,
                )
            })
            .collect();
        let readiness = capacity_readiness_from_probe_result(&verdict(
            false,
            "capacity",
            "every pooled plan is spent",
            Some(ProviderReadinessInvocationCapacity {
                remaining: Some(Value::from(0)),
                accounts,
                ..Default::default()
            }),
        ));
        assert!(readiness.is_exhausted());
        assert_eq!(readiness.accounts().len(), MAX_CAPACITY_ACCOUNTS);
        let serialized = serde_json::to_value(&readiness).expect("serializes");
        assert_eq!(serialized["accounts"][0]["account"], "plan-0");
    }

    #[test]
    fn the_readiness_wire_format_carries_capacity_accounts() {
        let verdict: ProviderReadinessInvocationResult = serde_json::from_value(serde_json::json!({
            "ready": false,
            "classification": "capacity",
            "retryable": true,
            "remediation": "wait",
            "reason": "provider_capacity_exhausted",
            "cache_key": "k",
            "identity": {},
            "capacity": {
                "remaining": 0,
                "limit": 100,
                "unit": "percent",
                "reset_at": "2026-09-27T15:26:09Z",
                "accounts": [
                    { "account": "one@example.com", "state": "exhausted", "remaining": 0, "reset_at": "2026-09-27T15:26:09Z", "windows": [] },
                    { "account": "two@example.com", "state": "exhausted", "remaining": 0, "reset_at": "2026-09-28T23:29:35Z" }
                ]
            }
        }))
        .expect("readiness result deserializes");
        let readiness = capacity_readiness_from_probe_result(&verdict);
        assert!(readiness.is_exhausted());
        assert_eq!(
            readiness
                .accounts()
                .iter()
                .map(|account| (account.account.as_str(), account.reset_at.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("one@example.com", Some("2026-09-27T15:26:09+00:00")),
                ("two@example.com", Some("2026-09-28T23:29:35+00:00")),
            ]
        );
    }

    #[test]
    fn a_route_without_accounts_omits_the_field() {
        let readiness = capacity_readiness_from_probe_result(&verdict(
            true,
            "ready",
            "",
            Some(ProviderReadinessInvocationCapacity {
                remaining: Some(Value::from(1)),
                ..Default::default()
            }),
        ));
        let serialized = serde_json::to_value(&readiness).expect("serializes");
        assert!(serialized.get("accounts").is_none(), "{serialized}");
    }

    #[test]
    fn a_provider_that_publishes_nothing_is_unknown_not_unavailable() {
        let readiness = capacity_readiness_from_probe_result(&verdict(true, "ready", "", None));
        assert_eq!(
            readiness,
            AgentTaskProviderCapacityReadiness::Unknown {
                reason: "the provider's readiness invocation does not report capacity".to_string(),
            }
        );
        assert!(!readiness.is_exhausted());
    }

    #[test]
    fn an_empty_capacity_object_is_still_unknown() {
        // A provider that declares a `capacity` object but fills in neither
        // `remaining` nor `limit` has not actually published anything
        // observable; treat it the same as omitting `capacity` entirely.
        let readiness = capacity_readiness_from_probe_result(&verdict(
            true,
            "ready",
            "",
            Some(ProviderReadinessInvocationCapacity {
                remaining: None,
                limit: None,
                unit: Some("requests".to_string()),
                reset_at: None,
                accounts: Vec::new(),
            }),
        ));
        assert!(matches!(
            readiness,
            AgentTaskProviderCapacityReadiness::Unknown { .. }
        ));
    }

    #[test]
    fn a_route_with_no_live_probe_reports_the_specific_reason_it_was_not_evaluated() {
        let readiness = capacity_readiness_unknown("live capacity validation was not requested");
        assert_eq!(
            readiness,
            AgentTaskProviderCapacityReadiness::Unknown {
                reason: "live capacity validation was not requested".to_string(),
            }
        );
    }

    /// Mid-run exhaustion classification (#14858 acceptance): a provider
    /// runtime stream carrying an actionable quota/usage-limit rejection
    /// must produce a typed capacity failure with its reset instant, not an
    /// opaque generic error. This drives the outcome through the real
    /// production adapters — `structured_error::normalize_runtime_stream_error`
    /// (#13703) for classification and the same diagnostic shape
    /// `usage_cap`'s detector attaches (#13644) for the reset instant —
    /// rather than hand-building the outcome's typed fields directly.
    #[test]
    fn mid_run_quota_exhaustion_is_a_typed_capacity_failure_with_its_reset_instant() {
        use crate::agent_task::{AgentTaskDiagnostic, AgentTaskOutcome, AgentTaskOutcomeStatus};
        use crate::agent_task_provider::structured_error::{
            normalize_runtime_stream_error, normalized_error_failure_classification,
        };
        use crate::agent_task_provider::usage_cap::{
            detect_usage_cap, AGENT_TASK_PROVIDER_USAGE_CAP_DIAGNOSTIC_CLASS,
        };

        // A real OpenCode terminal error event carrying an explicit,
        // *permanent* usage-limit rejection: `isRetryable:false` is what
        // distinguishes this from a merely-throttled 429 (which classifies as
        // `provider_rate_limited`, not exhaustion) in
        // `structured_error_failure_classification`.
        let stream = r#"{"type":"error","error":{"name":"APIError","data":{"message":"5-hour usage limit reached. Resets in 3hr 3min.","statusCode":403,"isRetryable":false}}}"#;
        let normalized =
            normalize_runtime_stream_error(Some("opencode"), stream).expect("adapter normalizes");
        let failure_classification = normalized_error_failure_classification(&normalized);

        let now = chrono::Utc::now();
        let reset_at = detect_usage_cap(normalized["message"].as_str().expect("message"), now)
            .expect("usage-cap reset time parses");

        let outcome = AgentTaskOutcome {
            task_id: "mid-run-exhaustion".to_string(),
            status: AgentTaskOutcomeStatus::Failed,
            failure_classification,
            diagnostics: vec![AgentTaskDiagnostic {
                class: AGENT_TASK_PROVIDER_USAGE_CAP_DIAGNOSTIC_CLASS.to_string(),
                message: format!(
                    "provider usage cap reached; resets at {}",
                    reset_at.to_rfc3339()
                ),
                data: serde_json::json!({ "reset_at": reset_at.to_rfc3339() }),
            }],
            ..Default::default()
        };

        let readiness =
            capacity_readiness_from_outcome(&outcome).expect("quota exhaustion is typed capacity");
        assert_eq!(
            readiness,
            AgentTaskProviderCapacityReadiness::Exhausted {
                reset_at: Some(reset_at.to_rfc3339()),
                reason: "the provider reported its account capacity as exhausted".to_string(),
                accounts: Vec::new(),
            }
        );
        assert!(readiness.is_exhausted());
        assert_eq!(readiness.reset_at(), Some(reset_at));
    }

    #[test]
    fn mid_run_quota_exhaustion_without_a_known_reset_is_still_typed_capacity() {
        use crate::agent_task::{
            AgentTaskFailureClassification, AgentTaskOutcome, AgentTaskOutcomeStatus,
        };

        // The provider classified as quota-exhausted, but nothing on the
        // outcome carries a parseable reset time (the common case when a
        // provider's rejection text never names one). Capacity evidence is
        // still typed as exhausted rather than falling back to unknown.
        let outcome = AgentTaskOutcome {
            task_id: "mid-run-exhaustion-no-reset".to_string(),
            status: AgentTaskOutcomeStatus::Failed,
            failure_classification: Some(AgentTaskFailureClassification::ProviderQuotaExhausted),
            ..Default::default()
        };

        let readiness =
            capacity_readiness_from_outcome(&outcome).expect("quota exhaustion is typed capacity");
        assert_eq!(
            readiness,
            AgentTaskProviderCapacityReadiness::Exhausted {
                reset_at: None,
                reason: "the provider reported its account capacity as exhausted".to_string(),
                accounts: Vec::new(),
            }
        );
        assert_eq!(readiness.reset_at(), None);
    }

    #[test]
    fn a_generic_provider_failure_is_not_reported_as_capacity() {
        use crate::agent_task::{
            AgentTaskFailureClassification, AgentTaskOutcome, AgentTaskOutcomeStatus,
        };

        let outcome = AgentTaskOutcome {
            task_id: "generic-failure".to_string(),
            status: AgentTaskOutcomeStatus::Failed,
            failure_classification: Some(AgentTaskFailureClassification::Provider),
            ..Default::default()
        };

        assert!(capacity_readiness_from_outcome(&outcome).is_none());
    }
}
