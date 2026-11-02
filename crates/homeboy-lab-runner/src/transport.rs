use homeboy_engine_primitives::content_hash;
use std::fs;
use std::io::{Read, Write};

use serde_json::json;
use sha2::Digest;

use homeboy_core::engine::shell;
use homeboy_core::error::{Error, ErrorCode, Result};
use homeboy_core::server::{self, SshClient};
use homeboy_lab_contract::lab::transport_failure::provider_evidence_io_error;

use super::session::{RunnerSession, RunnerStatusReport};
use super::{Runner, RunnerKind};

const MAX_PROVIDER_EVIDENCE_BYTES: u64 = 64 * 1024 * 1024;
const FILE_TRANSFER_TRANSPORT: &str = "direct_ssh";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunnerSessionHandle {
    pub(crate) session: RunnerSession,
    endpoint_url: String,
}

impl RunnerSessionHandle {
    fn new(session: RunnerSession, endpoint_url: String) -> Self {
        Self {
            session,
            endpoint_url,
        }
    }

    pub(crate) fn endpoint_url(&self) -> &str {
        &self.endpoint_url
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunnerTransport {
    DirectDaemon(RunnerSessionHandle),
    Local,
    DiagnosticSsh,
    Unavailable,
}

/// Lab runner file transfer has one channel: direct SSH to the runner's
/// configured `server_id`. A connected daemon session does not change it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunnerFileTransferCapability {
    DirectSsh {
        server_id: String,
    },
    Unsupported {
        transport: &'static str,
        reason: &'static str,
    },
}

/// Controller-side file operations against a remote Lab runner over SSH.
pub(crate) struct RunnerFileTransfer {
    runner_id: String,
    client: SshClient,
}

impl RunnerFileTransferCapability {
    pub(crate) fn for_runner(runner: &Runner) -> RunnerFileTransferCapability {
        match (&runner.kind, &runner.server_id) {
            (RunnerKind::Ssh, Some(server_id)) => RunnerFileTransferCapability::DirectSsh {
                server_id: server_id.clone(),
            },
            (RunnerKind::Ssh, None) => RunnerFileTransferCapability::Unsupported {
                transport: "ssh",
                reason:
                    "Lab runner file transfer uses direct SSH and no SSH server_id is configured",
            },
            (RunnerKind::Local, _) => RunnerFileTransferCapability::Unsupported {
                transport: "local",
                reason: "Lab runner file transfer requires a remote runner transport",
            },
        }
    }

    pub(crate) fn ensure_supported(&self, runner_id: &str) -> Result<()> {
        match self {
            RunnerFileTransferCapability::DirectSsh { .. } => Ok(()),
            RunnerFileTransferCapability::Unsupported { transport, reason } => Err(
                unsupported_file_transfer_error(runner_id, transport, reason),
            ),
        }
    }
}

impl RunnerFileTransfer {
    pub(crate) fn for_runner(runner: &Runner) -> Result<RunnerFileTransfer> {
        let capability = RunnerFileTransferCapability::for_runner(runner);
        capability.ensure_supported(&runner.id)?;
        let RunnerFileTransferCapability::DirectSsh { server_id } = capability else {
            unreachable!("checked above");
        };
        let server = server::load(&server_id)?;
        let mut client = SshClient::from_server(&server, &server_id)?;
        client.env.extend(runner.env.clone());
        Ok(RunnerFileTransfer {
            runner_id: runner.id.clone(),
            client,
        })
    }

    pub(crate) fn ensure_directory(&self, remote_dir: &str) -> Result<()> {
        let mkdir = self
            .client
            .execute(&format!("mkdir -p {}", shell::quote_arg(remote_dir)));
        if !mkdir.success {
            return Err(self.operation_error("mkdir", remote_dir, mkdir.stderr));
        }
        Ok(())
    }

    pub(crate) fn ensure_private_directory(&self, remote_dir: &str) -> Result<()> {
        let command = format!(
            "umask 077; dir={}; mkdir -p -- \"$dir\" && test ! -L \"$dir\" && test -O \"$dir\" && mode=$(stat -c %a -- \"$dir\" 2>/dev/null || stat -f %Lp \"$dir\") && test \"$mode\" = 700",
            shell::quote_arg(remote_dir),
        );
        let result = self.client.execute(&command);
        if !result.success {
            return Err(self.operation_error("private mkdir", remote_dir, result.stderr));
        }
        Ok(())
    }

    /// Upload a file and verify the remote copy's SHA-256 and size match the
    /// local bytes before reporting success. A mismatched copy is removed.
    pub(crate) fn upload_file(&self, local_path: &str, remote_path: &str) -> Result<()> {
        let (expected_sha256, expected_size) = local_file_digest(local_path)?;
        let upload = self.client.upload_file(local_path, remote_path);
        if !upload.success {
            return Err(self.operation_error("upload", remote_path, upload.stderr));
        }
        let verify = self.client.execute(&remote_digest_check(
            remote_path,
            &expected_sha256,
            expected_size,
        ));
        if !verify.success {
            let _ = self
                .client
                .execute(&format!("rm -f {}", shell::quote_arg(remote_path)));
            return Err(self.operation_error("upload verify", remote_path, verify.stderr));
        }
        Ok(())
    }

    /// Publish a private file only after its content hash and owner-only mode
    /// are established. The verified bytes are snapshotted before transfer and
    /// the temporary file is never visible at the final path.
    pub(crate) fn upload_private_file_atomic(
        &self,
        local_path: &str,
        remote_path: &str,
        expected_sha256: &str,
    ) -> Result<()> {
        let content =
            private_file_bytes_with_expected_digest(local_path, expected_sha256, remote_path)?;
        let snapshot = private_bytes_snapshot(&content)?;
        self.publish_private_snapshot(
            snapshot.path(),
            remote_path,
            expected_sha256,
            content.len() as u64,
            "private upload",
            "private publish",
        )
    }

    /// Transfer a declared evidence blob from one verified, bounded snapshot.
    pub(crate) fn upload_private_evidence_atomic(
        &self,
        local_path: &str,
        remote_path: &str,
        expected_sha256: &str,
        expected_size: u64,
    ) -> Result<()> {
        let snapshot = private_evidence_snapshot(local_path, expected_sha256, expected_size)?;
        self.publish_private_snapshot(
            snapshot.path(),
            remote_path,
            expected_sha256,
            expected_size,
            "private evidence upload",
            "private evidence publish",
        )
    }

    fn publish_private_snapshot(
        &self,
        snapshot: &std::path::Path,
        remote_path: &str,
        expected_sha256: &str,
        expected_size: u64,
        upload_operation: &str,
        publish_operation: &str,
    ) -> Result<()> {
        let snapshot = snapshot.to_str().ok_or_else(|| {
            Error::internal_unexpected(format!(
                "private snapshot path is not UTF-8: {}",
                snapshot.display()
            ))
        })?;
        let temp_path = format!("{remote_path}.{}.tmp", uuid::Uuid::new_v4());
        let upload = self.client.upload_file_private(snapshot, &temp_path);
        let result = if !upload.success {
            Err(self.operation_error(upload_operation, remote_path, upload.stderr))
        } else {
            let publish = self.client.execute(&format!(
                "{} && chmod 600 {} && mv -f {} {}",
                remote_digest_check(&temp_path, expected_sha256, expected_size),
                shell::quote_arg(&temp_path),
                shell::quote_arg(&temp_path),
                shell::quote_arg(remote_path),
            ));
            if publish.success {
                Ok(())
            } else {
                Err(self.operation_error(publish_operation, remote_path, publish.stderr))
            }
        };
        if result.is_err() {
            let _ = self
                .client
                .execute(&format!("rm -f {}", shell::quote_arg(&temp_path)));
        }
        result
    }

    pub(crate) fn download_file(&self, remote_path: &str, local_path: &str) -> Result<()> {
        let download = self.client.download_file(remote_path, local_path);
        if !download.success {
            return Err(self.operation_error("download", remote_path, download.stderr));
        }
        Ok(())
    }

    fn operation_error(&self, operation: &str, remote_path: &str, stderr: String) -> Error {
        file_transfer_operation_error(
            &self.runner_id,
            operation,
            remote_path,
            stderr,
            FILE_TRANSFER_TRANSPORT,
        )
    }
}

/// Remote shell check that `path` holds exactly `size` bytes hashing to
/// `sha256`.
fn remote_digest_check(path: &str, sha256: &str, size: u64) -> String {
    let path = shell::quote_arg(path);
    format!(
        "test \"$( (sha256sum {path} 2>/dev/null || shasum -a 256 {path}) | awk '{{print $1}}')\" = {} && test \"$(wc -c < {path})\" -eq {size}",
        shell::quote_arg(sha256),
    )
}

fn local_file_digest(local_path: &str) -> Result<(String, u64)> {
    let mut input = fs::File::open(local_path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(format!("open {local_path}"))))?;
    let mut digest = sha2::Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer).map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("read {local_path}")))
        })?;
        if read == 0 {
            break;
        }
        total += read as u64;
        digest.update(&buffer[..read]);
    }
    Ok((format!("{:x}", digest.finalize()), total))
}

fn private_bytes_snapshot(content: &[u8]) -> Result<tempfile::NamedTempFile> {
    let mut snapshot = tempfile::NamedTempFile::new().map_err(|err| {
        Error::internal_io(err.to_string(), Some("create private snapshot".into()))
    })?;
    snapshot
        .write_all(content)
        .and_then(|()| snapshot.as_file().sync_all())
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("write private snapshot".into()))
        })?;
    Ok(snapshot)
}

fn private_evidence_snapshot(
    local_path: &str,
    expected_sha256: &str,
    expected_size: u64,
) -> Result<tempfile::NamedTempFile> {
    if expected_size > MAX_PROVIDER_EVIDENCE_BYTES {
        return Err(Error::validation_invalid_argument(
            "provider_evidence",
            format!(
                "declared evidence exceeds the {MAX_PROVIDER_EVIDENCE_BYTES}-byte transport limit"
            ),
            Some(local_path.to_string()),
            None,
        ));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut input = options.open(local_path).map_err(|error| {
        provider_evidence_io_error(error, format!("open provider evidence {local_path}"))
    })?;
    let metadata = input.metadata().map_err(|error| {
        provider_evidence_io_error(error, format!("inspect provider evidence {local_path}"))
    })?;
    if !metadata.is_file() || metadata.len() != expected_size {
        return Err(Error::validation_invalid_argument(
            "provider_evidence",
            "declared evidence size no longer matches its projection",
            Some(local_path.to_string()),
            None,
        ));
    }
    let mut snapshot = tempfile::NamedTempFile::new().map_err(|error| {
        provider_evidence_io_error(error, "create evidence snapshot".to_string())
    })?;
    let mut digest = sha2::Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer).map_err(|error| {
            provider_evidence_io_error(error, format!("read provider evidence {local_path}"))
        })?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > expected_size {
            return Err(Error::validation_invalid_argument(
                "provider_evidence",
                "declared evidence grew after projection",
                Some(local_path.to_string()),
                None,
            ));
        }
        snapshot.write_all(&buffer[..read]).map_err(|error| {
            provider_evidence_io_error(error, "write evidence snapshot".to_string())
        })?;
        sha2::Digest::update(&mut digest, &buffer[..read]);
    }
    if total != expected_size || format!("{:x}", sha2::Digest::finalize(digest)) != expected_sha256
    {
        return Err(Error::validation_invalid_argument(
            "provider_evidence",
            "declared evidence no longer matches its digest or size",
            Some(local_path.to_string()),
            None,
        ));
    }
    snapshot
        .as_file()
        .sync_all()
        .map_err(|error| provider_evidence_io_error(error, "sync evidence snapshot".to_string()))?;
    Ok(snapshot)
}

fn private_file_bytes_with_expected_digest(
    local_path: &str,
    expected_sha256: &str,
    remote_path: &str,
) -> Result<Vec<u8>> {
    let content = fs::read(local_path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(format!("read {local_path}"))))?;
    let actual_sha256 = content_hash::sha256_hex(&content);
    if actual_sha256 != expected_sha256 {
        return Err(Error::validation_invalid_argument(
            "provider_evidence",
            "private runner file content does not match its declared SHA-256",
            Some(remote_path.to_string()),
            None,
        ));
    }
    Ok(content)
}

#[cfg(test)]
mod content_addressed_evidence_tests {
    use super::*;

    #[test]
    fn rejects_mutated_content_addressed_evidence_before_lab_handoff() {
        let temp = tempfile::NamedTempFile::new().expect("temporary evidence");
        std::fs::write(temp.path(), b"mutated").expect("write mutated evidence");

        let error = private_file_bytes_with_expected_digest(
            temp.path().to_str().expect("path"),
            "5e5d4d3f5d16a48a9d0e8c306cea55f2472d6f6d94393ee9347f0af3545831a6",
            "/runner/workspace-homeboy-artifacts/provider-evidence/files/abc/input",
        )
        .expect_err("digest mismatch blocks transfer");
        assert_eq!(error.details["field"], "provider_evidence");
        assert!(error
            .message
            .contains("does not match its declared SHA-256"));
    }

    #[test]
    fn rejects_evidence_that_grew_after_its_projection() {
        let temp = tempfile::NamedTempFile::new().expect("temporary evidence");
        std::fs::write(temp.path(), b"declared").expect("write declared evidence");
        std::fs::write(temp.path(), b"declared and grown").expect("grow projected evidence");

        let error = private_evidence_snapshot(
            temp.path().to_str().expect("path"),
            "d7914fe546b68468821c0e51f92b7c6d1a7a5c83ef67f5009e8604f4f408d53d",
            8,
        )
        .expect_err("post-projection growth is rejected");
        assert_eq!(error.details["field"], "provider_evidence");
        assert!(error.message.contains("size no longer matches"));
    }

    #[test]
    fn snapshot_remains_the_verified_bytes_when_the_source_path_is_replaced_before_upload() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let source = directory.path().join("fixture.bin");
        std::fs::write(&source, b"verified bytes").expect("write source");
        let snapshot = private_evidence_snapshot(
            source.to_str().expect("path"),
            &content_hash::sha256_hex(b"verified bytes"),
            14,
        )
        .expect("snapshot verified bytes");
        std::fs::remove_file(&source).expect("remove source");
        std::fs::write(&source, b"replacement bytes").expect("replace source path");
        assert_eq!(
            std::fs::read(snapshot.path()).expect("read snapshot"),
            b"verified bytes"
        );
    }

    #[test]
    fn streams_the_64_mib_boundary_through_a_fixed_buffer() {
        let temp = tempfile::NamedTempFile::new().expect("temporary evidence");
        let chunk = [0x5au8; 64 * 1024];
        let mut file = std::fs::File::create(temp.path()).expect("create boundary fixture");
        let mut hasher = sha2::Sha256::new();
        for _ in 0..1024 {
            file.write_all(&chunk).expect("write boundary chunk");
            hasher.update(chunk);
        }
        file.sync_all().expect("sync boundary fixture");
        let digest = format!("{:x}", hasher.finalize());
        let snapshot = private_evidence_snapshot(
            temp.path().to_str().expect("path"),
            &digest,
            MAX_PROVIDER_EVIDENCE_BYTES,
        )
        .expect("64 MiB boundary is admitted");
        assert_eq!(
            snapshot
                .as_file()
                .metadata()
                .expect("snapshot metadata")
                .len(),
            MAX_PROVIDER_EVIDENCE_BYTES
        );
    }
}

pub(crate) fn select_runner_transport(
    runner: &Runner,
    status: Option<&RunnerStatusReport>,
    allow_diagnostic_ssh: bool,
) -> RunnerTransport {
    if runner.kind == RunnerKind::Ssh && allow_diagnostic_ssh {
        return RunnerTransport::DiagnosticSsh;
    }

    if let Some(status) = status {
        if status.connected {
            if let Some(session) = status.session.as_ref() {
                if let Some(local_url) = session.local_url.as_ref() {
                    return RunnerTransport::DirectDaemon(RunnerSessionHandle::new(
                        session.clone(),
                        local_url.clone(),
                    ));
                }
            }
        }
    }

    if runner.kind == RunnerKind::Local {
        RunnerTransport::Local
    } else {
        RunnerTransport::Unavailable
    }
}

fn unsupported_file_transfer_error(runner_id: &str, transport: &str, reason: &str) -> Error {
    Error::new(
        ErrorCode::RunnerLabTransportFailure,
        format!(
            "Lab offload runner `{runner_id}` does not support controller file transfer over `{transport}`: {reason}"
        ),
        json!({
            "runner_id": runner_id,
            "transport": transport,
            "missing_capability": "runner_file_transfer",
            "supported_transports": [FILE_TRANSFER_TRANSPORT],
        }),
    )
    .with_retryable(false)
    .with_hint("Lab @file arguments and structured-output download use direct SSH; configure an SSH runner with a server_id.".to_string())
}

fn file_transfer_operation_error(
    runner_id: &str,
    operation: &str,
    remote_path: &str,
    stderr: String,
    transport: &str,
) -> Error {
    let stderr = homeboy_core::redaction::redact_string(stderr.trim());
    let remote_path = homeboy_core::redaction::redact_string(remote_path);
    let summary = format!(
        "{stderr} (Lab runner file transfer `{operation}` failed on runner `{runner_id}` for `{remote_path}`)"
    );
    Error::new(
        ErrorCode::RunnerLabTransportFailure,
        bounded_redacted_text(&summary, 512),
        json!({
            "runner_id": runner_id,
            "operation": operation,
            "remote_path": remote_path,
            "stderr": stderr,
            "transport": transport,
        }),
    )
    .with_retryable(true)
}

fn bounded_redacted_text(value: &str, limit: usize) -> String {
    homeboy_core::redaction::redact_string(value)
        .chars()
        .take(limit)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{
        RunnerActiveJobState, RunnerSessionRole, RunnerSessionState, RunnerTunnelMode,
    };

    fn runner(kind: RunnerKind) -> Runner {
        Runner {
            id: "test-runner".to_string(),
            kind,
            server_id: None,
            workspace_root: None,
            settings: Default::default(),
            env: Default::default(),
            secret_env: Default::default(),
            resources: Default::default(),
            policy: Default::default(),
        }
    }

    fn status(session: RunnerSession) -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: "test-runner".to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: Some(session),
            stale_daemon: None,
            configured_job_binary_build_identity: None,
            daemon_freshness: None,
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_jobs: Vec::new(),
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::NotQueried,
            active_job_source: None,
            active_job_error: None,
            active_job_recovery_evidence: None,
            session_path: "/tmp/session.json".to_string(),
        }
    }

    fn session(mode: RunnerTunnelMode) -> RunnerSession {
        RunnerSession {
            runner_id: "test-runner".to_string(),
            mode,
            role: RunnerSessionRole::Controller,
            server_id: None,
            controller_id: None,
            broker_url: None,
            remote_daemon_address: None,
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            tunnel_process_start_identity: None,
            proxy_forward: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: None,
            homeboy_version: "test".to_string(),
            homeboy_build_identity: None,
            connected_at: "now".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }
    }

    #[test]
    fn selects_diagnostic_ssh_before_session_transport() {
        assert_eq!(
            select_runner_transport(&runner(RunnerKind::Ssh), None, true),
            RunnerTransport::DiagnosticSsh
        );
    }

    #[test]
    fn selects_direct_daemon_from_connected_local_url() {
        let mut session = session(RunnerTunnelMode::DirectSsh);
        session.local_url = Some("http://127.0.0.1:1234".to_string());

        match select_runner_transport(&runner(RunnerKind::Ssh), Some(&status(session)), false) {
            RunnerTransport::DirectDaemon(handle) => {
                assert_eq!(handle.endpoint_url(), "http://127.0.0.1:1234");
            }
            transport => panic!("expected direct daemon, got {transport:?}"),
        }
    }

    #[test]
    fn file_transfer_selects_direct_ssh_when_server_id_exists() {
        let mut runner = runner(RunnerKind::Ssh);
        runner.server_id = Some("srv".to_string());

        assert_eq!(
            RunnerFileTransferCapability::for_runner(&runner),
            RunnerFileTransferCapability::DirectSsh {
                server_id: "srv".to_string(),
            }
        );
    }

    #[test]
    fn runner_with_a_live_daemon_session_still_selects_ssh_for_files() {
        let mut runner = runner(RunnerKind::Ssh);
        runner.server_id = Some("srv".to_string());
        let mut session = session(RunnerTunnelMode::DirectSsh);
        session.local_url = Some("http://127.0.0.1:1234".to_string());
        assert!(matches!(
            select_runner_transport(&runner, Some(&status(session)), false),
            RunnerTransport::DirectDaemon(_)
        ));

        assert_eq!(
            RunnerFileTransferCapability::for_runner(&runner),
            RunnerFileTransferCapability::DirectSsh {
                server_id: "srv".to_string(),
            }
        );
    }

    #[test]
    fn ssh_runner_without_server_id_cannot_transfer_files() {
        let runner = runner(RunnerKind::Ssh);
        let capability = RunnerFileTransferCapability::for_runner(&runner);
        let error = capability
            .ensure_supported(&runner.id)
            .expect_err("no SSH authority");

        assert_eq!(error.code, ErrorCode::RunnerLabTransportFailure);
        assert_eq!(error.retryable, Some(false));
        assert_eq!(error.details["supported_transports"], json!(["direct_ssh"]));
    }

    #[cfg(unix)]
    fn local_ssh_transfer(server_id: &str) -> RunnerFileTransfer {
        server::create(
            &json!({"id": server_id, "host": "localhost", "user": "test"}).to_string(),
            false,
        )
        .unwrap();
        let mut runner = runner(RunnerKind::Ssh);
        runner.server_id = Some(server_id.to_string());
        RunnerFileTransfer::for_runner(&runner).expect("SSH file transfer")
    }

    #[cfg(unix)]
    #[test]
    fn ssh_private_directory_creation_preserves_existing_shared_permissions() {
        use std::os::unix::fs::PermissionsExt;
        crate::test_support::with_isolated_home(|_| {
            let transfer = local_ssh_transfer("private-mkdir");
            let root = tempfile::tempdir().unwrap();
            let private = root.path().join("evidence/nested");
            transfer
                .ensure_private_directory(private.to_str().unwrap())
                .expect("private SSH mkdir");
            assert_eq!(
                fs::metadata(&private).unwrap().permissions().mode() & 0o777,
                0o700
            );
            let shared = root.path().join("shared");
            fs::create_dir(&shared).unwrap();
            fs::set_permissions(&shared, fs::Permissions::from_mode(0o775)).unwrap();
            transfer
                .ensure_private_directory(shared.to_str().unwrap())
                .expect_err("shared parent refused");
            assert_eq!(
                fs::metadata(&shared).unwrap().permissions().mode() & 0o777,
                0o775
            );
            transfer
                .ensure_directory(shared.to_str().unwrap())
                .expect("ordinary SSH mkdir");
        });
    }

    #[cfg(unix)]
    #[test]
    fn ssh_upload_verifies_remote_digest_and_size_before_success() {
        crate::test_support::with_isolated_home(|_| {
            let transfer = local_ssh_transfer("verified-upload");
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source.bin");
            let remote = root.path().join("remote.bin");
            fs::write(&source, b"verified upload bytes").unwrap();

            transfer
                .upload_file(source.to_str().unwrap(), remote.to_str().unwrap())
                .expect("verified upload");
            assert_eq!(fs::read(&remote).unwrap(), b"verified upload bytes");

            let (sha256, size) = local_file_digest(source.to_str().unwrap()).unwrap();
            assert_eq!(size, 21);
            let check = transfer.client.execute(&remote_digest_check(
                remote.to_str().unwrap(),
                &sha256,
                size,
            ));
            assert!(check.success, "matching remote copy verifies");
            let mismatch = transfer.client.execute(&remote_digest_check(
                remote.to_str().unwrap(),
                &sha256,
                size + 1,
            ));
            assert!(!mismatch.success, "size mismatch is refused");
            let wrong_digest = transfer.client.execute(&remote_digest_check(
                remote.to_str().unwrap(),
                &"0".repeat(64),
                size,
            ));
            assert!(!wrong_digest.success, "digest mismatch is refused");
        });
    }

    #[cfg(unix)]
    #[test]
    fn ssh_private_upload_publishes_owner_only_verified_bytes() {
        use std::os::unix::fs::PermissionsExt;
        crate::test_support::with_isolated_home(|_| {
            let transfer = local_ssh_transfer("private-upload");
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("evidence.json");
            let remote = root.path().join("published.json");
            fs::write(&source, b"private evidence").unwrap();
            let sha256 = content_hash::sha256_hex(b"private evidence");

            transfer
                .upload_private_evidence_atomic(
                    source.to_str().unwrap(),
                    remote.to_str().unwrap(),
                    &sha256,
                    16,
                )
                .expect("private evidence upload");
            assert_eq!(fs::read(&remote).unwrap(), b"private evidence");
            assert_eq!(
                fs::metadata(&remote).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let leftovers = fs::read_dir(root.path())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
                .count();
            assert_eq!(leftovers, 0, "temporary upload is renamed into place");
        });
    }

    #[test]
    fn direct_transfer_keeps_complete_redacted_source_facts() {
        let stderr = format!(
            "Authorization: Bearer ssh-fixture-secret; {}",
            "remote upload rejection detail ".repeat(80)
        );
        let remote_path = format!("/{}", "long-path-segment/".repeat(40));
        assert!(stderr.len() > 512);
        assert!(remote_path.len() > 512);
        assert!(stderr.contains("ssh-fixture-secret"));

        let error = file_transfer_operation_error(
            "test-runner",
            "private evidence upload",
            &remote_path,
            stderr,
            "direct_ssh",
        );
        let redacted_stderr = homeboy_core::redaction::redact_string(
            &error.details["stderr"]
                .as_str()
                .expect("complete stderr detail"),
        );
        let redacted_path = homeboy_core::redaction::redact_string(&remote_path);
        assert!(error.message.chars().count() <= 512);
        assert_eq!(error.details["stderr"], redacted_stderr);
        assert_eq!(error.details["remote_path"], redacted_path);

        let persisted_candidate = homeboy_lab_contract::lab::transport_failure::preacceptance_transport_error(
            "attempt-direct-private-upload",
            "test-runner",
            homeboy_lab_contract::lab::transport_failure::LabTransportOperation::DispatchCookAttempt,
            homeboy_lab_contract::lab::transport_failure::LabJobAcceptanceDisposition::NoJobAccepted,
            error,
        );
        assert_eq!(
            persisted_candidate.details["source_error"]["details"]["stderr"],
            redacted_stderr
        );
        assert_eq!(
            persisted_candidate.details["source_error"]["details"]["remote_path"],
            redacted_path
        );
        assert!(!serde_json::to_string(&persisted_candidate.details)
            .expect("serialize full SSH evidence")
            .contains("ssh-fixture-secret"));
    }

    #[test]
    fn missing_provider_evidence_retains_the_original_filesystem_cause() {
        let home = tempfile::tempdir().expect("temporary directory");
        let missing = home.path().join("missing-evidence.json");

        let error =
            private_evidence_snapshot(missing.to_str().unwrap(), "0".repeat(64).as_str(), 0)
                .expect_err("missing evidence is an actual filesystem error");

        assert_eq!(error.code, ErrorCode::InternalIoError);
        assert_eq!(
            error.details["context"],
            format!("open provider evidence {}", missing.display())
        );
        assert!(error.message.contains("open provider evidence"));
        assert!(error.message.contains("No such file") || error.message.contains("not found"));
        let source = std::error::Error::source(&error).expect("original I/O source");
        let source = source
            .downcast_ref::<std::io::Error>()
            .expect("source remains an io::Error");
        assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        assert!(
            source.to_string().contains("No such file") || source.to_string().contains("not found")
        );
    }

    #[cfg(unix)]
    #[test]
    fn provider_evidence_disk_full_keeps_canonical_storage_classification() {
        let error = provider_evidence_io_error(
            std::io::Error::from_raw_os_error(28),
            "write evidence snapshot".to_string(),
        );

        assert_eq!(error.code, ErrorCode::StorageExhausted);
        assert_eq!(error.retryable, Some(false));
        assert_eq!(error.details["context"], "write evidence snapshot");
        assert!(error.message.contains("write evidence snapshot"));
        assert!(error.message.contains("No space left") || error.message.contains("storage full"));
        let source = std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<std::io::Error>())
            .expect("original disk-full I/O source");
        assert_eq!(source.raw_os_error(), Some(28));
    }
}
