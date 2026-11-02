use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use reqwest::blocking::Client;
use reqwest::header;
use sha2::{Digest, Sha256};

use homeboy_core::error::{Error, Result};
use homeboy_core::execution_contract::{encode_uri_component, EXECUTION_CONTRACT};
use homeboy_core::paths;
use homeboy_core::runner_download_cache::{
    record_download_intent, resolve_runner_download_target, sanitize_artifact_file_name,
    RunnerDownloadIntent,
};

use super::super::{load, status, RunnerArtifactRef};
use super::tokens::RemoteArtifactToken;

#[derive(Debug)]
pub struct RemoteArtifactDownload {
    pub output_path: PathBuf,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub sha256: Option<String>,
    pub artifact_ref: RunnerArtifactRef,
}

/// Fetch a runner artifact on an operator's behalf.
///
/// The default location is tagged [`RunnerDownloadIntent::OperatorPull`], which
/// is the fail-closed reading of an untyped call: `runs artifact get`,
/// `runs artifacts --pull`, the HTTP artifact endpoint, and every provider-trait
/// consumer land here, and all of them hand the resulting path back to a human.
/// Homeboy's own fetches must say so explicitly via
/// [`download_remote_artifact_with_intent`].
pub fn download_remote_artifact(
    path: &str,
    output: Option<PathBuf>,
) -> Result<RemoteArtifactDownload> {
    download_remote_artifact_with_intent(path, output, RunnerDownloadIntent::OperatorPull)
}

/// Fetch a runner artifact and record why (#10585).
///
/// The intent is persisted beside the bytes so `cleanup --include
/// runner-downloads` can tell an operator's review copy from a transient
/// internal fetch. It is only recorded for the default cache layout: a caller
/// supplying an explicit `output` owns that location and homeboy does not
/// reclaim it.
pub fn download_remote_artifact_with_intent(
    path: &str,
    output: Option<PathBuf>,
    intent: RunnerDownloadIntent,
) -> Result<RemoteArtifactDownload> {
    let token = RemoteArtifactToken::parse(path)?;
    let artifact_root = paths::PathRoots::from_environment()?
        .artifacts()
        .to_path_buf();
    download_direct_runner_artifact(&artifact_root, &token, output, intent)
}

/// Where one download's bytes may be written, and what to tag afterwards.
///
/// Produced only by [`resolve_placement`], so no transport can build an output
/// path without going through the containment check.
#[derive(Debug)]
pub(super) struct DownloadPlacement {
    pub(super) output_path: PathBuf,
    /// The cache directory to tag with the caller's intent, or `None` when the
    /// caller supplied an explicit `output` and owns that location itself.
    pub(super) cache_dir: Option<PathBuf>,
    /// The name actually written. Reported back as the artifact-ref name so
    /// reported metadata can never disagree with the bytes on disk — a
    /// downstream consumer that rebuilds a path from the name inherits the
    /// sanitized form, not the remote's.
    pub(super) file_name: String,
}

/// Decide where one download's bytes go, against an explicitly injected
/// artifact root. Containment is checked against that same root, so a caller
/// cannot validate one home and write into another.
pub(super) fn resolve_placement(
    artifact_root: &Path,
    token: &RemoteArtifactToken,
    output: Option<PathBuf>,
    remote_file_name: Option<&str>,
) -> Result<DownloadPlacement> {
    if let Some(output_path) = output {
        // An explicit destination is the caller's own; it never enters the
        // cache layout and is never tagged.
        let file_name = output_path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| sanitize_artifact_file_name(&token.artifact_id));
        return Ok(DownloadPlacement {
            output_path,
            cache_dir: None,
            file_name,
        });
    }
    // Fail closed. The root used to be resolved here as
    // `paths::artifact_root()?`, itself replacing an
    // `.unwrap_or_else(|_| PathBuf::from("."))` that silently relocated the
    // whole cache into the process working directory. It is now supplied by the
    // caller, which resolves it once per download.
    let target = resolve_runner_download_target(
        artifact_root,
        &token.runner_id,
        &token.run_id,
        remote_file_name,
        &token.artifact_id,
    )?;
    Ok(DownloadPlacement {
        output_path: target.file_path,
        cache_dir: Some(target.cache_dir),
        file_name: target.file_name,
    })
}

const ARTIFACT_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const ARTIFACT_TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// A body read that fails part-way is a transport failure, not proof the
/// artifact is gone: name the timeout when that is the cause and mark the
/// error retryable so callers can re-acquire instead of giving up.
pub(super) fn artifact_body_read_error(detail: &str, timed_out: bool) -> Error {
    let message = if timed_out {
        format!(
            "read runner artifact response: timed out after {}s: {detail}",
            ARTIFACT_TRANSFER_TIMEOUT.as_secs()
        )
    } else {
        format!("read runner artifact response: {detail}")
    };
    Error::internal_unexpected(message).with_retryable(true)
}

fn download_direct_runner_artifact(
    artifact_root: &Path,
    token: &RemoteArtifactToken,
    output: Option<PathBuf>,
    intent: RunnerDownloadIntent,
) -> Result<RemoteArtifactDownload> {
    let runner = load(&token.runner_id)?;
    let connected = status(&token.runner_id)?;
    let Some(session) = connected.session.filter(|_| connected.connected) else {
        return Err(Error::validation_invalid_argument(
            "runner",
            "runner is not connected to a daemon; run `homeboy runner connect <runner-id>` first",
            Some(runner.id),
            Some(vec![
                "Read/query integrations use the connected daemon so results come from the runner machine.".to_string(),
            ]),
        ));
    };
    let Some(local_url) = session.local_url.as_deref() else {
        return Err(Error::validation_invalid_argument(
            "runner",
            "runner session has no direct daemon URL; run `homeboy runner connect <runner-id>` to re-establish it",
            Some(runner.id),
            Some(vec![
                "Runner artifacts are streamed from the connected daemon over the direct SSH session.".to_string(),
            ]),
        ));
    };
    fetch_daemon_artifact(local_url, artifact_root, token, output, intent)
}

/// Stream one artifact from a connected daemon into place, verified.
///
/// The body is copied into a temp file beside the destination while it is
/// hashed, checked against the daemon's `x-homeboy-artifact-sha256` and
/// `Content-Length` when present, and only then renamed into place, so a
/// truncated or corrupted transfer never appears at the output path.
pub(super) fn fetch_daemon_artifact(
    local_url: &str,
    artifact_root: &Path,
    token: &RemoteArtifactToken,
    output: Option<PathBuf>,
    intent: RunnerDownloadIntent,
) -> Result<RemoteArtifactDownload> {
    // Artifact content (candidate git bundles in particular) can be large on
    // monorepos. A 30s whole-request budget cut the body read off mid-stream
    // and surfaced as reqwest's opaque "error decoding response body",
    // stranding a finished Lab candidate (Extra-Chill/homeboy#16842). Bound
    // connection setup tightly and give the transfer itself room.
    let client = Client::builder()
        .connect_timeout(ARTIFACT_CONNECT_TIMEOUT)
        .timeout(ARTIFACT_TRANSFER_TIMEOUT)
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build daemon HTTP client: {err}")))?;
    let path = format!(
        "/runs/{}/artifacts/{}/content",
        encode_uri_component(&token.run_id),
        encode_uri_component(&token.artifact_id)
    );
    let mut response = client
        .get(format!("{}{}", local_url.trim_end_matches('/'), path))
        .send()
        .map_err(|err| {
            Error::internal_unexpected(format!("query runner daemon: {err}")).with_retryable(true)
        })?;
    let status = response.status();
    let headers = response.headers().clone();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(Error::validation_invalid_argument(
            "artifact_id",
            format!(
                "runner artifact fetch failed with HTTP {}: {}",
                status.as_u16(),
                body
            ),
            Some(token.artifact_id.clone()),
            None,
        ));
    }
    let expected_sha256 = header_string(&headers, ARTIFACT_SHA256_HEADER);
    let expected_size = header_string(&headers, header::CONTENT_LENGTH.as_str())
        .map(|value| {
            value.trim().parse::<u64>().map_err(|err| {
                Error::internal_unexpected(format!(
                    "runner artifact Content-Length `{value}` is not a byte count: {err}"
                ))
            })
        })
        .transpose()?;

    // `Content-Disposition` is parsed by splitting on `;` and trimming quotes,
    // so the value reaching here is whatever the daemon sent, `../` included.
    let remote_file_name = content_disposition_filename(&headers).filter(|name| !name.is_empty());
    let placement = resolve_placement(artifact_root, token, output, remote_file_name.as_deref())?;
    let output_path = placement.output_path;
    let parent = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("create {}", parent.display())),
        )
    })?;
    let temp = tempfile::NamedTempFile::new_in(parent).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("create temp file in {}", parent.display())),
        )
    })?;

    let mut reader = HashingReader::new(&mut response);
    let mut writer = io::BufWriter::new(temp.as_file());
    let copied = io::copy(&mut reader, &mut writer).and_then(|copied| {
        writer.flush()?;
        Ok(copied)
    });
    drop(writer);
    let actual_size = match copied {
        Ok(copied) => copied,
        Err(err) if reader.read_failed => {
            return Err(artifact_body_read_error(
                &err.to_string(),
                io_error_is_timeout(&err),
            ));
        }
        Err(err) => {
            return Err(Error::internal_io(
                err.to_string(),
                Some(format!("write runner artifact {}", output_path.display())),
            ));
        }
    };
    let actual_sha256 = format!("{:x}", reader.hasher.finalize());
    // Dropping `temp` on the error path deletes it.
    verify_artifact_body(
        expected_sha256.as_deref(),
        expected_size,
        &actual_sha256,
        actual_size,
    )?;
    temp.as_file().sync_all().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some(format!("sync runner artifact {}", output_path.display())),
        )
    })?;
    temp.persist(&output_path).map_err(|err| {
        Error::internal_io(
            err.error.to_string(),
            Some(format!("write runner artifact {}", output_path.display())),
        )
    })?;
    if let Some(cache_dir) = &placement.cache_dir {
        record_download_intent(cache_dir, intent, &token.artifact_id);
    }

    let content_type = header_string(&headers, header::CONTENT_TYPE.as_str());
    Ok(RemoteArtifactDownload {
        output_path,
        content_type: content_type.clone(),
        size_bytes: i64::try_from(actual_size).ok(),
        sha256: Some(actual_sha256.clone()),
        artifact_ref: RunnerArtifactRef {
            artifact_id: token.artifact_id.clone(),
            name: Some(placement.file_name.clone()),
            path: Some(EXECUTION_CONTRACT.artifacts.runner_artifact_ref(
                &token.runner_id,
                &token.run_id,
                &token.artifact_id,
            )),
            url: None,
            mime: content_type,
            size_bytes: Some(actual_size),
            sha256: Some(actual_sha256),
            transport: Some("direct_daemon".to_string()),
        },
    })
}

const ARTIFACT_SHA256_HEADER: &str = "x-homeboy-artifact-sha256";

/// Check streamed bytes against what the daemon declared. A mismatch is a
/// transfer fault, not proof the artifact is bad, so it is retryable.
pub(super) fn verify_artifact_body(
    expected_sha256: Option<&str>,
    expected_size: Option<u64>,
    actual_sha256: &str,
    actual_size: u64,
) -> Result<()> {
    if let Some(expected) = expected_size {
        if expected != actual_size {
            return Err(Error::internal_unexpected(format!(
                "runner artifact size mismatch: expected {expected} bytes (Content-Length), received {actual_size} bytes"
            ))
            .with_retryable(true));
        }
    }
    if let Some(expected) = expected_sha256 {
        let expected = expected.trim();
        if !expected.eq_ignore_ascii_case(actual_sha256) {
            return Err(Error::internal_unexpected(format!(
                "runner artifact sha256 mismatch: expected {expected} ({ARTIFACT_SHA256_HEADER}), computed {actual_sha256}"
            ))
            .with_retryable(true));
        }
    }
    Ok(())
}

/// Hashes bytes as they are read and remembers whether the failure, if any,
/// came from the body read rather than the disk write.
struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
    read_failed: bool,
}

impl<R> HashingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            read_failed: false,
        }
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.inner.read(buf) {
            Ok(read) => {
                self.hasher.update(&buf[..read]);
                Ok(read)
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => Err(err),
            Err(err) => {
                self.read_failed = true;
                Err(err)
            }
        }
    }
}

fn io_error_is_timeout(err: &io::Error) -> bool {
    if err.kind() == io::ErrorKind::TimedOut {
        return true;
    }
    let mut source = err
        .get_ref()
        .map(|inner| inner as &(dyn std::error::Error + 'static));
    while let Some(inner) = source {
        if let Some(reqwest_err) = inner.downcast_ref::<reqwest::Error>() {
            if reqwest_err.is_timeout() {
                return true;
            }
        }
        if let Some(io_err) = inner.downcast_ref::<io::Error>() {
            if io_err.kind() == io::ErrorKind::TimedOut {
                return true;
            }
        }
        source = inner.source();
    }
    false
}

fn header_string(headers: &header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

pub(super) fn content_disposition_filename(headers: &header::HeaderMap) -> Option<String> {
    let value = header_string(headers, header::CONTENT_DISPOSITION.as_str())?;
    value.split(';').find_map(|part| {
        let part = part.trim();
        let filename = part.strip_prefix("filename=")?;
        Some(filename.trim_matches('"').to_string())
    })
}
