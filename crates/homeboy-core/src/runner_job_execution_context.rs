//! Authenticated execution identity for a claimed runner job.
//!
//! This is deliberately built from the durable broker job and its live claim,
//! not from environment or runner-local lifecycle projections. Consumers may
//! project it into those surfaces for child-process compatibility, but cannot
//! construct authority from them.

use homeboy_engine_primitives::content_hash;
use serde::{Deserialize, Serialize};

use crate::api_jobs::{Job, RemoteRunnerJobRequest};
use crate::error::{Error, ErrorCode, Result};

pub const RUNNER_JOB_EXECUTION_CONTEXT_SCHEMA: &str = "homeboy/runner-job-execution-context/v1";
pub const RUNNER_JOB_EXECUTION_CONTEXT_EVIDENCE_SCHEMA: &str =
    "homeboy/runner-job-execution-context-evidence/v1";
pub const RUNNER_JOB_EXECUTION_CONTEXT_CAPABILITY: &str = "runner-job-execution-context";
pub const RUNNER_JOB_EXECUTION_CONTEXT_CAPABILITY_VERSION: u32 = 1;
const MAX_EVIDENCE_BYTES: usize = 8 * 1024;

/// A worker must explicitly advertise this before the broker will hand it a
/// context-bearing job. This prevents an older worker from ignoring the added
/// claim field and executing an unauthenticated handoff.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerJobExecutionProtocol {
    pub capability: String,
    pub version: u32,
}

impl RunnerJobExecutionProtocol {
    pub fn current() -> Self {
        Self {
            capability: RUNNER_JOB_EXECUTION_CONTEXT_CAPABILITY.to_string(),
            version: RUNNER_JOB_EXECUTION_CONTEXT_CAPABILITY_VERSION,
        }
    }

    pub fn verify(&self) -> Result<()> {
        if self.capability == RUNNER_JOB_EXECUTION_CONTEXT_CAPABILITY
            && self.version == RUNNER_JOB_EXECUTION_CONTEXT_CAPABILITY_VERSION
        {
            return Ok(());
        }
        Err(rejected(
            "worker does not advertise the required execution-context protocol",
        ))
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerJobExecutionContext {
    schema: String,
    id: String,
    controller_run_id: String,
    controller_attempt_id: String,
    runner_id: String,
    runner_job_id: String,
    accepted_handoff_id: String,
    runtime_id: String,
    /// Opaque reference to the broker claim/reservation. Raw claim material
    /// never crosses a durable evidence or provider-process boundary.
    claim_ref: String,
    dispatch_receipt: String,
    verification: RunnerJobExecutionVerification,
    /// Authority is intentionally process-local. A wire value is only an
    /// assertion until it has been recomputed from the live durable claim.
    #[serde(skip)]
    authenticated: bool,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerJobExecutionVerification {
    state: String,
    verified_at_ms: u64,
}

impl std::fmt::Debug for RunnerJobExecutionContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RunnerJobExecutionContext")
            .field("id", &self.id)
            .field("runner_id", &self.runner_id)
            .field("runner_job_id", &self.runner_job_id)
            .field("runtime_id", &self.runtime_id)
            .field("authenticated", &self.authenticated)
            .finish_non_exhaustive()
    }
}

impl RunnerJobExecutionContext {
    /// Construct the only remote context source: a durable accepted job with a
    /// live broker claim. Generic runner jobs use their durable run/job identity
    /// as their controller attempt; agent-task callers retain their explicit IDs.
    pub fn from_claim(job: &Job, request: &RemoteRunnerJobRequest) -> Result<Self> {
        let runner_id = required(&job.claimed_by_runner_id, "claimed runner")?;
        let expected_runner_id = required(&job.target_runner_id, "target runner")?;
        if runner_id != expected_runner_id || runner_id != request.runner_id.trim() {
            return Err(rejected(
                "runner identity does not match the accepted dispatch receipt",
            ));
        }
        let claim_id = required(&job.claim_id, "claim")?;
        let controller_run_id = metadata_id(request, "controller_run_id")
            .or_else(|| {
                request
                    .lifecycle
                    .as_ref()
                    .and_then(|l| l.durable_run_id.clone())
            })
            .or_else(|| {
                request
                    .metadata
                    .as_ref()
                    .and_then(|m| m.get("run_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| job.id.to_string());
        let controller_attempt_id = metadata_id(request, "controller_attempt_id")
            .unwrap_or_else(|| controller_run_id.clone());
        let accepted_handoff_id = metadata_id(request, "accepted_handoff_id")
            .unwrap_or_else(|| format!("{}:{}", controller_attempt_id, job.id));
        let runtime_id = request
            .command
            .first()
            .map(String::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| rejected("accepted dispatch has no runtime command"))?
            .to_string();
        let dispatch_receipt = receipt(
            &controller_run_id,
            &controller_attempt_id,
            &runner_id,
            &job.id.to_string(),
            &accepted_handoff_id,
            &runtime_id,
            &claim_id,
        );

        Ok(Self {
            schema: RUNNER_JOB_EXECUTION_CONTEXT_SCHEMA.to_string(),
            id: context_id(&dispatch_receipt),
            controller_run_id,
            controller_attempt_id,
            runner_id,
            runner_job_id: job.id.to_string(),
            accepted_handoff_id,
            runtime_id,
            claim_ref: claim_ref(&claim_id),
            dispatch_receipt,
            verification: RunnerJobExecutionVerification {
                state: "verified".to_string(),
                verified_at_ms: job.updated_at_ms,
            },
            authenticated: false,
        })
    }

    pub fn local(runtime_id: impl Into<String>) -> Self {
        let runtime_id = runtime_id.into();
        let dispatch_receipt = "local".to_string();
        Self {
            schema: RUNNER_JOB_EXECUTION_CONTEXT_SCHEMA.to_string(),
            id: context_id(&dispatch_receipt),
            controller_run_id: "local".to_string(),
            controller_attempt_id: "local".to_string(),
            runner_id: "local".to_string(),
            runner_job_id: "local".to_string(),
            accepted_handoff_id: "local".to_string(),
            runtime_id,
            claim_ref: "local".to_string(),
            dispatch_receipt,
            verification: RunnerJobExecutionVerification {
                state: "local".to_string(),
                verified_at_ms: 0,
            },
            authenticated: true,
        }
    }

    /// Direct-daemon jobs are accepted by the durable local-child reservation
    /// rather than a reverse-broker claim. The reservation is the lease/receipt
    /// that binds this context to the accepted daemon job.
    pub fn direct_daemon(
        controller_run_id: Option<&str>,
        runner_id: &str,
        runner_job_id: &str,
        runtime_id: &str,
        reservation_id: &str,
    ) -> Result<Self> {
        Self::direct_daemon_with_dispatch_metadata(
            controller_run_id,
            None,
            None,
            runner_id,
            runner_job_id,
            runtime_id,
            reservation_id,
        )
    }

    /// Construct a direct-daemon context from controller-generated dispatch
    /// identities. The reservation remains the daemon-side acceptance proof.
    pub fn direct_daemon_with_dispatch_metadata(
        controller_run_id: Option<&str>,
        controller_attempt_id: Option<&str>,
        accepted_handoff_id: Option<&str>,
        runner_id: &str,
        runner_job_id: &str,
        runtime_id: &str,
        reservation_id: &str,
    ) -> Result<Self> {
        let runner_id = non_empty(runner_id, "runner")?;
        let runner_job_id = non_empty(runner_job_id, "runner job")?;
        let runtime_id = non_empty(runtime_id, "runtime")?;
        let reservation_id = non_empty(reservation_id, "reservation")?;
        let controller_run_id = controller_run_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&runner_job_id)
            .to_string();
        let controller_attempt_id = controller_attempt_id
            .map(|value| non_empty(value, "controller attempt"))
            .transpose()?
            .unwrap_or_else(|| controller_run_id.clone());
        let accepted_handoff_id = accepted_handoff_id
            .map(|value| non_empty(value, "accepted handoff"))
            .transpose()?
            .unwrap_or_else(|| format!("{controller_attempt_id}:{runner_job_id}"));
        let dispatch_receipt = receipt(
            &controller_run_id,
            &controller_attempt_id,
            &runner_id,
            &runner_job_id,
            &accepted_handoff_id,
            &runtime_id,
            &reservation_id,
        );
        Ok(Self {
            schema: RUNNER_JOB_EXECUTION_CONTEXT_SCHEMA.to_string(),
            id: context_id(&dispatch_receipt),
            controller_run_id,
            controller_attempt_id,
            runner_id,
            runner_job_id,
            accepted_handoff_id,
            runtime_id,
            claim_ref: claim_ref(&reservation_id),
            dispatch_receipt,
            verification: RunnerJobExecutionVerification {
                state: "verified".to_string(),
                verified_at_ms: 0,
            },
            authenticated: true,
        })
    }

    /// Turn a received assertion into a capability after proving it against the
    /// actual broker claim. The authentication bit is never serialized, so a
    /// forged JSON payload cannot reach a provider entry point.
    pub fn verify_claim(&self, job: &Job, request: &RemoteRunnerJobRequest) -> Result<Self> {
        if job.status != crate::api_jobs::JobStatus::Running
            || job
                .claim_expires_at_ms
                .is_none_or(|expires_at| expires_at <= crate::api_jobs::timestamp_ms())
        {
            return Err(rejected("durable runner claim is no longer live"));
        }
        let verified = Self::from_claim(job, request)?;
        if self != &verified {
            return Err(rejected(
                "dispatch receipt does not match the accepted runner job",
            ));
        }
        Ok(Self {
            authenticated: true,
            ..verified
        })
    }

    /// Verify the self-contained form used by process-boundary consumers before
    /// they serialize it into a provider command payload.
    pub fn verify_integrity(&self) -> Result<()> {
        if self.schema != RUNNER_JOB_EXECUTION_CONTEXT_SCHEMA
            || self.id != context_id(&self.dispatch_receipt)
            || !matches!(self.verification.state.as_str(), "verified" | "local")
            || !self.authenticated
        {
            return Err(rejected(
                "execution context schema, identity, or verification is invalid",
            ));
        }
        Ok(())
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn runner_id(&self) -> &str {
        &self.runner_id
    }

    pub fn runner_job_id(&self) -> &str {
        &self.runner_job_id
    }

    pub fn runtime_id(&self) -> &str {
        &self.runtime_id
    }

    pub fn is_local(&self) -> bool {
        self.authenticated && self.verification.state == "local"
    }

    /// A single bounded, content-addressed durable record shared by controller
    /// and runner recovery. The complete typed context is retained so a restart
    /// never needs to infer authority from lifecycle or environment state.
    pub fn evidence_record(&self) -> Result<serde_json::Value> {
        let context = serde_json::to_value(self).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize runner context evidence".to_string()),
            )
        })?;
        let bytes = serde_json::to_vec(&context).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("encode runner context evidence".to_string()),
            )
        })?;
        if bytes.len() > MAX_EVIDENCE_BYTES {
            return Err(rejected(
                "accepted dispatch context evidence exceeds its bounded record",
            ));
        }
        Ok(serde_json::json!({
            "schema": RUNNER_JOB_EXECUTION_CONTEXT_EVIDENCE_SCHEMA,
            "content_sha256": format!("sha256:{}", content_hash::sha256_hex(&bytes)),
            "context": context,
        }))
    }

    pub fn from_evidence_record(evidence: &serde_json::Value) -> Result<Self> {
        if evidence.get("schema").and_then(serde_json::Value::as_str)
            != Some(RUNNER_JOB_EXECUTION_CONTEXT_EVIDENCE_SCHEMA)
        {
            return Err(rejected("context evidence has an unsupported schema"));
        }
        let context = evidence
            .get("context")
            .ok_or_else(|| rejected("context evidence is missing its context"))?;
        let bytes = serde_json::to_vec(context).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("encode runner context evidence".to_string()),
            )
        })?;
        let expected = format!("sha256:{}", content_hash::sha256_hex(&bytes));
        if evidence
            .get("content_sha256")
            .and_then(serde_json::Value::as_str)
            != Some(&expected)
        {
            return Err(rejected(
                "context evidence content address does not match its context",
            ));
        }
        let context: Self = serde_json::from_value(context.clone()).map_err(|error| {
            Error::new(
                ErrorCode::RunnerLabTransportFailure,
                format!("runner execution context rejected: invalid durable evidence: {error}"),
                serde_json::json!({ "phase": "runner_job_execution_context" }),
            )
        })?;
        // Evidence establishes durable identity, not runtime authority. A
        // reverse worker must still call `verify_claim` with its broker payload.
        if context.schema != RUNNER_JOB_EXECUTION_CONTEXT_SCHEMA
            || context.id != context_id(&context.dispatch_receipt)
            || !matches!(context.verification.state.as_str(), "verified" | "local")
        {
            return Err(rejected("context evidence has invalid identity fields"));
        }
        Ok(context)
    }
}

fn metadata_id(request: &RemoteRunnerJobRequest, key: &str) -> Option<String> {
    request
        .metadata
        .as_ref()?
        .get(key)?
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn required(value: &Option<String>, label: &str) -> Result<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| rejected(&format!("accepted dispatch is missing its {label} receipt")))
}

fn non_empty(value: &str, label: &str) -> Result<String> {
    value
        .trim()
        .is_empty()
        .then(|| rejected(&format!("accepted dispatch is missing its {label} receipt")))
        .map_or_else(|| Ok(value.trim().to_string()), Err)
}

fn receipt(
    run: &str,
    attempt: &str,
    runner: &str,
    job: &str,
    handoff: &str,
    runtime: &str,
    lease: &str,
) -> String {
    let payload = format!("{run}\n{attempt}\n{runner}\n{job}\n{handoff}\n{runtime}\n{lease}");
    format!("sha256:{}", content_hash::sha256_hex(payload.as_bytes()))
}

fn claim_ref(claim_id: &str) -> String {
    format!("sha256:{}", content_hash::sha256_hex(claim_id.as_bytes()))
}

fn context_id(dispatch_receipt: &str) -> String {
    format!(
        "rjec:{}",
        content_hash::sha256_hex(dispatch_receipt.as_bytes())
    )
}

pub(crate) fn rejected(reason: &str) -> Error {
    Error::new(
        ErrorCode::RunnerLabTransportFailure,
        format!("runner execution context rejected: {reason}"),
        serde_json::json!({ "phase": "runner_job_execution_context" }),
    )
    .with_hint("Claim a fresh runner job through the controller before retrying.".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_context_is_explicit_and_never_nullable() {
        let context = RunnerJobExecutionContext::local("homeboy");
        assert!(context.is_local());
        assert_eq!(context.runner_id(), "local");
    }

    #[test]
    fn serialized_context_preserves_direct_receipt() {
        let context = RunnerJobExecutionContext::direct_daemon(
            Some("run-1"),
            "runner-1",
            "job-1",
            "homeboy",
            "reservation-1",
        )
        .expect("context");
        let decoded: RunnerJobExecutionContext =
            serde_json::from_value(serde_json::to_value(context).expect("serialize"))
                .expect("deserialize");

        let expected = RunnerJobExecutionContext::direct_daemon(
            Some("run-1"),
            "runner-1",
            "job-1",
            "homeboy",
            "reservation-1",
        )
        .expect("expected context");
        assert_eq!(decoded.id(), expected.id());
        assert!(decoded.verify_integrity().is_err());
    }

    #[test]
    fn direct_context_preserves_controller_dispatch_metadata() {
        let context = RunnerJobExecutionContext::direct_daemon_with_dispatch_metadata(
            Some("run-1"),
            Some("run-1:attempt-2"),
            Some("daemon:runner-1:run-1:attempt-2"),
            "runner-1",
            "job-1",
            "homeboy",
            "reservation-1",
        )
        .expect("context");
        let serialized = serde_json::to_value(context).expect("serialize context");

        assert_eq!(serialized["controller_run_id"], "run-1");
        assert_eq!(serialized["controller_attempt_id"], "run-1:attempt-2");
        assert_eq!(
            serialized["accepted_handoff_id"],
            "daemon:runner-1:run-1:attempt-2"
        );
    }

    #[test]
    fn evidence_record_is_content_addressed_and_bounded() {
        let context = RunnerJobExecutionContext::local("homeboy");
        let mut evidence = context.evidence_record().expect("evidence");
        assert_eq!(
            evidence["schema"],
            RUNNER_JOB_EXECUTION_CONTEXT_EVIDENCE_SCHEMA
        );
        assert!(evidence["content_sha256"]
            .as_str()
            .is_some_and(|value| value.starts_with("sha256:")));
        assert_eq!(evidence["context"]["runner_id"], "local");
        assert_eq!(
            RunnerJobExecutionContext::from_evidence_record(&evidence)
                .expect("recover evidence")
                .id(),
            context.id()
        );
        evidence["context"]["runner_id"] = serde_json::json!("tampered");
        assert!(RunnerJobExecutionContext::from_evidence_record(&evidence).is_err());
    }

    #[test]
    fn debug_redacts_claim_and_dispatch_receipts() {
        let context = RunnerJobExecutionContext::direct_daemon(
            Some("run-1"),
            "runner-1",
            "job-1",
            "homeboy",
            "reservation-secret",
        )
        .expect("context");
        let debug = format!("{context:?}");
        assert!(!debug.contains("reservation-secret"));
        assert!(!debug.contains("dispatch_receipt"));
    }

    #[test]
    fn serialized_evidence_and_context_redact_raw_claim_material() {
        let secret = "reservation-secret";
        let context = RunnerJobExecutionContext::direct_daemon(
            Some("run-1"),
            "runner-1",
            "job-1",
            "homeboy",
            secret,
        )
        .expect("context");

        let serialized = serde_json::to_string(&context).expect("serialize context");
        let evidence = serde_json::to_string(&context.evidence_record().expect("evidence"))
            .expect("serialize evidence");
        assert!(!serialized.contains(secret));
        assert!(!evidence.contains(secret));
        assert!(serialized.contains("claim_ref"));
    }
}
