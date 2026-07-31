use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use super::{AgentTaskAcceptanceRequirement, AgentTaskAcceptanceVerdict};
use homeboy_core::error::{Error, Result};

/// Stable identity of the configured verifier that issued a durable verdict.
/// Configuration must be an opaque, non-secret reference (for example a policy
/// revision or service configuration id), never the verifier credential itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskAcceptanceVerifierProvenance {
    pub verifier: String,
    pub configuration: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskAcceptanceVerificationRequest {
    pub requirement: AgentTaskAcceptanceRequirement,
    pub verdict: AgentTaskAcceptanceVerdict,
    pub candidate: crate::agent_task_promotion::AgentTaskCandidateFingerprint,
    pub base_sha: String,
    pub evidence_refs: Vec<String>,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskAcceptanceAttestation {
    pub actor: String,
    pub authority: String,
    pub policy: String,
    pub issued_at: String,
    pub provider_ref: String,
    /// Opaque signature issued by the authority over its structured verdict.
    pub signature: String,
    /// Configured trust key that verified `signature`.
    pub key_id: String,
}

const ACCEPTANCE_VERDICT_SCHEMA: &str = "homeboy/agent-task-acceptance-verdict/v1";
const MAX_ISSUED_AT_FUTURE_SKEW: Duration = Duration::from_secs(5 * 60);
const PROCESS_IO_DRAIN_TIMEOUT: Duration = Duration::from_millis(100);

pub(crate) fn validate_acceptance_requirement(
    requirement: &AgentTaskAcceptanceRequirement,
) -> Result<()> {
    if requirement.authority.trim().is_empty() || requirement.policy.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "acceptance",
            "acceptance requires non-empty authority and policy",
            None,
            None,
        ));
    }
    Ok(())
}

pub(crate) fn validate_attestation(
    requirement: &AgentTaskAcceptanceRequirement,
    attestation: &AgentTaskAcceptanceAttestation,
) -> Result<()> {
    validate_acceptance_requirement(requirement)?;
    if attestation.actor.trim().is_empty()
        || attestation.authority.trim().is_empty()
        || attestation.policy.trim().is_empty()
        || attestation.provider_ref.trim().is_empty()
        || attestation.signature.trim().is_empty()
        || attestation.key_id.trim().is_empty()
        || attestation.authority != requirement.authority
        || attestation.policy != requirement.policy
    {
        return Err(Error::validation_invalid_argument(
            "acceptance",
            "authority verifier attestation does not satisfy the configured acceptance policy",
            None,
            None,
        ));
    }
    let issued_at = chrono::DateTime::parse_from_rfc3339(&attestation.issued_at).map_err(|_| {
        Error::validation_invalid_argument(
            "acceptance",
            "authority verifier attestation has an invalid issued_at timestamp",
            None,
            None,
        )
    })?;
    if issued_at.with_timezone(&chrono::Utc)
        > chrono::Utc::now()
            + chrono::Duration::from_std(MAX_ISSUED_AT_FUTURE_SKEW).expect("valid duration")
    {
        return Err(Error::validation_invalid_argument(
            "acceptance",
            "authority verifier attestation issued_at exceeds the allowed future clock skew",
            None,
            None,
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct CommandAcceptanceVerdict {
    schema: String,
    attestation: AgentTaskAcceptanceAttestation,
}

struct CommandAcceptanceVerifier {
    command: Vec<String>,
    provenance: AgentTaskAcceptanceVerifierProvenance,
    timeout: Duration,
    output_limit_bytes: usize,
    key_id: String,
    key: Vec<u8>,
    key_env: String,
}

/// Register the configured production verifier. Tests retain direct fixture
/// injection through the registry rather than requiring a subprocess.
pub fn register_acceptance_verifier_from_config(
    config: &homeboy_core::defaults::AgentTaskConfig,
) -> Result<()> {
    let Some(config) = config.acceptance_verifier.as_ref() else {
        return Ok(());
    };
    let homeboy_core::defaults::AgentTaskAcceptanceVerifierTrustConfig::HmacSha256 {
        key_id,
        key_env,
    } = &config.trust;
    let key = std::env::var(key_env).map_err(|_| {
        Error::validation_invalid_argument(
            "agent_task.acceptance_verifier.trust.key_env",
            "configured acceptance verifier HMAC key is unavailable",
            None,
            None,
        )
    })?;
    if config.command.is_empty()
        || config.configuration.trim().is_empty()
        || config.timeout_ms == 0
        || config.output_limit_bytes == 0
        || key_id.trim().is_empty()
        || key.is_empty()
    {
        return Err(Error::validation_invalid_argument(
            "agent_task.acceptance_verifier",
            "acceptance verifier requires non-empty command, configuration, timeout_ms, and output_limit_bytes",
            None,
            None,
        ));
    }
    register_acceptance_verifier(Box::new(CommandAcceptanceVerifier {
        command: config.command.clone(),
        provenance: AgentTaskAcceptanceVerifierProvenance {
            verifier: config.command[0].clone(),
            configuration: config.configuration.clone(),
        },
        timeout: Duration::from_millis(config.timeout_ms),
        output_limit_bytes: config.output_limit_bytes,
        key_id: key_id.clone(),
        key: key.into_bytes(),
        key_env: key_env.clone(),
    }));
    Ok(())
}

impl AgentTaskAcceptanceVerifier for CommandAcceptanceVerifier {
    fn provenance(&self) -> AgentTaskAcceptanceVerifierProvenance {
        self.provenance.clone()
    }

    fn verify_acceptance(
        &self,
        request: &AgentTaskAcceptanceVerificationRequest,
    ) -> Result<AgentTaskAcceptanceAttestation> {
        validate_acceptance_requirement(&request.requirement)?;
        let input = serde_json::to_vec(request)
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
        if input.len() > self.output_limit_bytes {
            return Err(Error::validation_invalid_argument(
                "acceptance",
                "acceptance verifier request exceeds its configured I/O limit",
                None,
                None,
            ));
        }
        let mut command = Command::new(&self.command[0]);
        command
            .args(&self.command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The configured key is controller trust material, never verifier input.
            .env_remove(&self.key_env);
        configure_process_group(&mut command);
        let mut child = command.spawn().map_err(|error| {
            Error::validation_invalid_argument(
                "acceptance",
                format!("acceptance verifier could not start: {error}"),
                None,
                None,
            )
        })?;
        let mut stdout = child.stdout.take().expect("stdout piped");
        let mut stderr = child.stderr.take().expect("stderr piped");
        let stdin = child.stdin.take().expect("stdin piped");
        if let Err(error) = (|| -> Result<()> {
            configure_pipe_nonblocking(&stdout)?;
            configure_pipe_nonblocking(&stderr)?;
            configure_pipe_nonblocking(&stdin)
        })() {
            kill_process_group(&mut child);
            let _ = child.wait();
            return Err(error);
        }
        let io_deadline = Arc::new(Mutex::new(None));
        let limit = self.output_limit_bytes;
        let (stdout_sender, stdout_receiver) = mpsc::sync_channel(1);
        let stdout_deadline = io_deadline.clone();
        let _ = std::thread::spawn(move || {
            let _ = stdout_sender.send(read_bounded(&mut stdout, limit, &stdout_deadline));
        });
        let (stderr_sender, stderr_receiver) = mpsc::sync_channel(1);
        let stderr_deadline = io_deadline.clone();
        let _ = std::thread::spawn(move || {
            let _ = stderr_sender.send(read_bounded(&mut stderr, limit, &stderr_deadline));
        });
        let writer_deadline = io_deadline.clone();
        // The writer owns stdin and observes the same drain deadline. Dropping
        // its handle prevents a verifier that never reads stdin from extending
        // the parent lifecycle.
        let _ = std::thread::spawn(move || -> std::io::Result<()> {
            let mut stdin = stdin;
            write_all_bounded(&mut stdin, &input, &writer_deadline)
        });
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|error| Error::internal_io(error.to_string(), None))?
            {
                break status;
            }
            if started.elapsed() >= self.timeout {
                kill_process_group(&mut child);
                let _ = child.wait();
                finish_process_io(&io_deadline);
                return Err(Error::validation_invalid_argument(
                    "acceptance",
                    "acceptance verifier timed out",
                    None,
                    None,
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        // A verifier owns its complete process tree. Reap descendants before
        // waiting for pipe readers, but never join an escaped descendant's
        // reader indefinitely.
        kill_process_group(&mut child);
        finish_process_io(&io_deadline);
        let stdout = receive_process_io(&stdout_receiver, &io_deadline);
        let stderr = receive_process_io(&stderr_receiver, &io_deadline);
        if !stdout.complete || !stderr.complete {
            return Err(Error::validation_invalid_argument(
                "acceptance",
                "acceptance verifier output capture was incomplete",
                None,
                None,
            ));
        }
        if stdout.truncated || stderr.truncated || !status.success() {
            return Err(Error::validation_invalid_argument(
                "acceptance",
                format!(
                    "acceptance verifier failed: {}",
                    redact_verifier_output(&stderr.output, &request.token)
                ),
                None,
                None,
            ));
        }
        let verdict: CommandAcceptanceVerdict =
            serde_json::from_str(&stdout.output).map_err(|_| {
                Error::validation_invalid_argument(
                    "acceptance",
                    "acceptance verifier returned an invalid structured verdict",
                    None,
                    None,
                )
            })?;
        validate_attestation(&request.requirement, &verdict.attestation)?;
        if verdict.schema != ACCEPTANCE_VERDICT_SCHEMA
            || verdict.attestation.key_id != self.key_id
            || !verify_hmac(&self.key, request, &verdict.attestation)
        {
            return Err(Error::validation_invalid_argument(
                "acceptance",
                "acceptance verifier returned an unsigned or unsupported verdict",
                None,
                None,
            ));
        }
        Ok(verdict.attestation)
    }

    fn revalidate_attestation(
        &self,
        request: &AgentTaskAcceptanceVerificationRequest,
        attestation: &AgentTaskAcceptanceAttestation,
    ) -> Result<()> {
        validate_attestation(&request.requirement, attestation)?;
        if attestation.key_id != self.key_id || !verify_hmac(&self.key, request, attestation) {
            return Err(Error::validation_invalid_argument(
                "acceptance",
                "durable acceptance attestation failed trusted-key verification",
                None,
                None,
            ));
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct AcceptanceSignedPayload<'a> {
    schema: &'static str,
    requirement: &'a AgentTaskAcceptanceRequirement,
    verdict: AgentTaskAcceptanceVerdict,
    candidate: &'a crate::agent_task_promotion::AgentTaskCandidateFingerprint,
    base_sha: &'a str,
    evidence_refs: &'a [String],
    actor: &'a str,
    authority: &'a str,
    policy: &'a str,
    issued_at: &'a str,
    provider_ref: &'a str,
}

fn signature_payload(
    request: &AgentTaskAcceptanceVerificationRequest,
    attestation: &AgentTaskAcceptanceAttestation,
) -> Vec<u8> {
    serde_json::to_vec(&AcceptanceSignedPayload {
        schema: ACCEPTANCE_VERDICT_SCHEMA,
        requirement: &request.requirement,
        verdict: request.verdict,
        candidate: &request.candidate,
        base_sha: &request.base_sha,
        evidence_refs: &request.evidence_refs,
        actor: &attestation.actor,
        authority: &attestation.authority,
        policy: &attestation.policy,
        issued_at: &attestation.issued_at,
        provider_ref: &attestation.provider_ref,
    })
    .expect("acceptance signature payload is serializable")
}

fn verify_hmac(
    key: &[u8],
    request: &AgentTaskAcceptanceVerificationRequest,
    attestation: &AgentTaskAcceptanceAttestation,
) -> bool {
    let Ok(signature) = BASE64.decode(&attestation.signature) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(key) else {
        return false;
    };
    mac.update(&signature_payload(request, attestation));
    mac.verify_slice(&signature).is_ok()
}

fn redact_verifier_output(value: &str, token: &str) -> String {
    homeboy_core::redaction::redact_string(&value.replace(token, "[REDACTED]"))
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn kill_process_group(child: &mut std::process::Child) {
    if child.id() <= i32::MAX as u32 {
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    } else {
        let _ = child.kill();
    }
}

#[cfg(not(unix))]
fn kill_process_group(child: &mut std::process::Child) {
    let _ = child.kill();
}

fn finish_process_io(deadline: &Mutex<Option<Instant>>) {
    *deadline
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) =
        Some(Instant::now() + PROCESS_IO_DRAIN_TIMEOUT);
}

fn process_io_finished(deadline: &Mutex<Option<Instant>>) -> bool {
    deadline
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .is_some_and(|deadline| Instant::now() >= deadline)
}

struct ProcessCapture {
    output: String,
    truncated: bool,
    complete: bool,
}

fn receive_process_io(
    receiver: &mpsc::Receiver<ProcessCapture>,
    deadline: &Mutex<Option<Instant>>,
) -> ProcessCapture {
    let deadline = deadline
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .expect("process I/O deadline is set before receiving capture");
    match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(capture) => capture,
        // The reader owns its pipe and will close it after seeing the deadline.
        // A detached capture cannot establish that its output was complete.
        Err(_) => ProcessCapture {
            output: String::new(),
            truncated: false,
            complete: false,
        },
    }
}

fn read_bounded(
    reader: &mut impl Read,
    limit: usize,
    deadline: &Mutex<Option<Instant>>,
) -> ProcessCapture {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    let complete = loop {
        match reader.read(&mut buffer) {
            Ok(0) => break true,
            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if process_io_finished(deadline) {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break false,
            Ok(count) => {
                let available = limit.saturating_sub(bytes.len());
                bytes.extend_from_slice(&buffer[..count.min(available)]);
                truncated |= count > available;
                if process_io_finished(deadline) {
                    break false;
                }
            }
        }
    };
    ProcessCapture {
        output: String::from_utf8_lossy(&bytes).to_string(),
        truncated,
        complete,
    }
}

fn write_all_bounded(
    writer: &mut impl Write,
    input: &[u8],
    deadline: &Mutex<Option<Instant>>,
) -> std::io::Result<()> {
    let mut written = 0;
    while written < input.len() {
        match writer.write(&input[written..]) {
            Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
            Ok(count) => written += count,
            Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if process_io_finished(deadline) {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn configure_pipe_nonblocking(pipe: &impl std::os::fd::AsRawFd) -> Result<()> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(Error::internal_io(
            std::io::Error::last_os_error().to_string(),
            None,
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn configure_pipe_nonblocking(_pipe: &impl std::any::Any) -> Result<()> {
    Ok(())
}

pub trait AgentTaskAcceptanceVerifier: Send + Sync {
    fn provenance(&self) -> AgentTaskAcceptanceVerifierProvenance;

    fn verify_acceptance(
        &self,
        request: &AgentTaskAcceptanceVerificationRequest,
    ) -> Result<AgentTaskAcceptanceAttestation>;

    /// Recheck a persisted signature without calling the external authority.
    fn revalidate_attestation(
        &self,
        request: &AgentTaskAcceptanceVerificationRequest,
        attestation: &AgentTaskAcceptanceAttestation,
    ) -> Result<()>;
}

struct UnconfiguredAcceptanceVerifier;
impl AgentTaskAcceptanceVerifier for UnconfiguredAcceptanceVerifier {
    fn provenance(&self) -> AgentTaskAcceptanceVerifierProvenance {
        AgentTaskAcceptanceVerifierProvenance {
            verifier: "unconfigured".to_string(),
            configuration: "none".to_string(),
        }
    }

    fn verify_acceptance(
        &self,
        _: &AgentTaskAcceptanceVerificationRequest,
    ) -> Result<AgentTaskAcceptanceAttestation> {
        Err(Error::validation_invalid_argument(
            "acceptance",
            "no acceptance authority verifier is configured for this runtime",
            None,
            None,
        ))
    }

    fn revalidate_attestation(
        &self,
        _: &AgentTaskAcceptanceVerificationRequest,
        _: &AgentTaskAcceptanceAttestation,
    ) -> Result<()> {
        Err(Error::validation_invalid_argument(
            "acceptance",
            "no acceptance authority verifier is configured for this runtime",
            None,
            None,
        ))
    }
}

/// Revalidate a persisted acceptance against the verifier registered from the
/// current trusted configuration. Tokens are deliberately not part of signed
/// payloads and are never persisted.
pub(crate) fn revalidate_durable_attestation(
    request: &AgentTaskAcceptanceVerificationRequest,
    attestation: &AgentTaskAcceptanceAttestation,
) -> Result<()> {
    validate_attestation(&request.requirement, attestation)?;
    with_acceptance_verifier(|verifier| verifier.revalidate_attestation(request, attestation))
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn AgentTaskAcceptanceVerifier,
    noop: UnconfiguredAcceptanceVerifier,
    register: pub fn register_acceptance_verifier,
    with: pub(crate) fn with_acceptance_verifier,
}

#[cfg(any(test, feature = "test-support"))]
pub fn clear_acceptance_verifier_for_test() {
    let mut slot = provider_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = None;
}

#[cfg(any(test, feature = "test-support"))]
impl AcceptanceVerifierTestGuard {
    pub fn install(verifier: Box<dyn AgentTaskAcceptanceVerifier>) -> Self {
        // The registry is process-global. Serialize test installation so one
        // fixture cannot clear another fixture's verifier mid-verification.
        let lock = test_verifier_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        register_acceptance_verifier(verifier);
        Self { _lock: lock }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for AcceptanceVerifierTestGuard {
    fn drop(&mut self) {
        clear_acceptance_verifier_for_test();
    }
}

#[cfg(any(test, feature = "test-support"))]
pub struct AcceptanceVerifierTestGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(any(test, feature = "test-support"))]
fn test_verifier_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &LOCK
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn request(token: &str) -> AgentTaskAcceptanceVerificationRequest {
        AgentTaskAcceptanceVerificationRequest {
            requirement: AgentTaskAcceptanceRequirement {
                authority: "review".into(),
                policy: "release".into(),
            },
            verdict: AgentTaskAcceptanceVerdict::Accepted,
            candidate: crate::agent_task_promotion::AgentTaskCandidateFingerprint {
                schema: "test".into(),
                target_path: "/repo".into(),
                head: "candidate".into(),
                base: "base".into(),
                changed_files: vec!["src/lib.rs".into()],
                sha256: "candidate-sha256".into(),
                tree: "candidate-tree".into(),
            },
            base_sha: "base".into(),
            evidence_refs: vec!["evidence://1".into()],
            token: token.into(),
        }
    }

    fn verifier(command: Vec<String>, timeout: u64) -> CommandAcceptanceVerifier {
        CommandAcceptanceVerifier {
            command,
            provenance: AgentTaskAcceptanceVerifierProvenance {
                verifier: "test".into(),
                configuration: "test".into(),
            },
            timeout: Duration::from_millis(timeout),
            output_limit_bytes: 1024 * 1024,
            key_id: "key-1".into(),
            key: b"controller-only-key".to_vec(),
            key_env: "HOMEBOY_TEST_ACCEPTANCE_KEY".into(),
        }
    }

    fn signed_output(request: &AgentTaskAcceptanceVerificationRequest) -> String {
        let mut attestation = AgentTaskAcceptanceAttestation {
            actor: "reviewer".into(),
            authority: "review".into(),
            policy: "release".into(),
            issued_at: "2026-07-30T00:00:00Z".into(),
            provider_ref: "review://1".into(),
            signature: String::new(),
            key_id: "key-1".into(),
        };
        let mut mac = Hmac::<Sha256>::new_from_slice(b"controller-only-key").unwrap();
        mac.update(&signature_payload(request, &attestation));
        attestation.signature = BASE64.encode(mac.finalize().into_bytes());
        serde_json::json!({ "schema": ACCEPTANCE_VERDICT_SCHEMA, "attestation": attestation })
            .to_string()
    }

    #[cfg(unix)]
    fn script(body: &str) -> NamedTempFile {
        use std::os::unix::fs::PermissionsExt;
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        file
    }

    #[cfg(unix)]
    #[test]
    fn command_verifier_rejects_forged_signature() {
        let request = request("token");
        let mut output = signed_output(&request);
        output = output.replacen("reviewer", "attacker", 1);
        let file = script(&format!("printf '%s' '{}'", output.replace('\'', "'\\''")));
        let error = verifier(vec![file.path().display().to_string()], 2_000)
            .verify_acceptance(&request)
            .unwrap_err();
        assert!(
            error.message.contains("unsigned or unsupported"),
            "unexpected verification error: {}",
            error.message
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_verifier_accepts_valid_signed_verdict() {
        let request = request("token");
        let output = signed_output(&request);
        let file = script(&format!("printf '%s' '{}'", output.replace('\'', "'\\''")));

        let attestation = verifier(vec![file.path().display().to_string()], 2_000)
            .verify_acceptance(&request)
            .expect("complete valid signed verifier output is accepted");

        assert_eq!(attestation.actor, "reviewer");
    }

    #[cfg(unix)]
    #[test]
    fn command_verifier_rejects_valid_signed_verdict_when_an_escaped_descendant_keeps_stderr_open()
    {
        let request = request("token");
        let output = signed_output(&request);
        let file = script(&format!(
            "perl -MPOSIX -e 'exit 0 if fork; POSIX::setsid() or die $!; sleep 1' &\nsleep 0.1\nprintf '%s' '{}'\nexit 0",
            output.replace('\'', "'\\''")
        ));
        let started = Instant::now();
        let error = verifier(vec![file.path().display().to_string()], 2_000)
            .verify_acceptance(&request)
            .unwrap_err();
        assert!(
            error.message.contains("output capture was incomplete"),
            "unexpected verification error: {}",
            error.message
        );
        assert!(
            started.elapsed() < Duration::from_millis(1_500),
            "escaped descendant retained stderr beyond the bounded drain"
        );
    }

    #[test]
    fn signed_attestation_rejects_dirty_replacement_at_the_same_head() {
        let request = request("token");
        let output: CommandAcceptanceVerdict =
            serde_json::from_str(&signed_output(&request)).unwrap();
        let mut replacement = request.clone();
        replacement.candidate.sha256 = "dirty-replacement-sha256".into();
        replacement.candidate.tree = "dirty-replacement-tree".into();

        let error = verifier(vec!["unused".into()], 500)
            .revalidate_attestation(&replacement, &output.attestation)
            .unwrap_err();
        assert!(error.message.contains("trusted-key verification"));
    }

    #[test]
    fn attestation_requires_the_configured_nonblank_policy_and_a_bounded_timestamp() {
        let request = request("token");
        let output: CommandAcceptanceVerdict =
            serde_json::from_str(&signed_output(&request)).unwrap();
        let mut blank_policy = output.attestation.clone();
        blank_policy.policy.clear();
        let error = validate_attestation(&request.requirement, &blank_policy)
            .expect_err("blank policy cannot satisfy the requirement");
        assert!(error.message.contains("configured acceptance policy"));

        let mut future = output.attestation;
        future.issued_at = (chrono::Utc::now() + chrono::Duration::minutes(6)).to_rfc3339();
        let error = validate_attestation(&request.requirement, &future)
            .expect_err("far-future signed timestamps are not durable evidence");
        assert!(error.message.contains("future clock skew"));
    }

    #[cfg(unix)]
    #[test]
    fn command_verifier_times_out_while_stdin_is_backpressured() {
        let file = script("sleep 5");
        let error = verifier(vec![file.path().display().to_string()], 50)
            .verify_acceptance(&request(&"x".repeat(512 * 1024)))
            .unwrap_err();
        assert!(error.message.contains("timed out"));
    }

    #[cfg(unix)]
    #[test]
    fn command_verifier_redacts_echoed_token_and_kills_descendants() {
        let marker = tempfile::NamedTempFile::new().unwrap();
        let marker_path = marker.path().display().to_string();
        let file = script(&format!(
            "(sleep 1; printf escaped > '{marker_path}') & cat >&2; exit 1"
        ));
        let token = "literal-token-that-must-not-leak";
        let error = verifier(vec![file.path().display().to_string()], 500)
            .verify_acceptance(&request(token))
            .unwrap_err();
        assert!(!error.message.contains(token));
        std::thread::sleep(Duration::from_millis(1100));
        assert!(std::fs::read_to_string(marker.path())
            .unwrap_or_default()
            .is_empty());
    }
}
