//! Portable repository checks evaluated from tracked Git objects.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// An operator-owned policy stored under the repository's Git common directory.
pub const OPERATOR_POLICY_FILE: &str = "homeboy.repository-integrity.json";
const OPERATOR_POLICY_SCHEMA: &str = "homeboy/repository-integrity-policy/v1";
const OPERATOR_POLICY_MAX_BYTES: u64 = 64 * 1024;
const OPERATOR_POLICY_MAX_EXCEPTIONS: usize = 128;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct SymlinkException {
    pub path: String,
    pub target_base64: String,
    pub reason: String,
}

pub use homeboy_source_snapshot_contract::source_snapshot::{
    RepositoryIntegrityEvidence, RepositoryIntegritySymlinkException,
};

#[derive(Deserialize)]
struct OperatorPolicy {
    schema: String,
    origin: String,
    #[serde(default)]
    symlink_exceptions: Vec<SymlinkException>,
}

#[derive(Serialize)]
struct CanonicalEvidence<'a> {
    origin: &'a str,
    symlink_exceptions: &'a [RepositoryIntegritySymlinkException],
}

/// Verify every tracked symlink in `revision` without consulting the checkout.
/// Candidate `homeboy.json` retains its path-and-reason exception contract. An
/// operator may additionally supply immutable admitted evidence with exact
/// path, target-byte, and reason exceptions.
pub fn verify_tracked_symlink_portability(
    path: &Path,
    revision: &str,
    evidence: Option<&RepositoryIntegrityEvidence>,
) -> Result<()> {
    let Some(tree) = git_optional(
        path,
        &["rev-parse", "--verify", &format!("{revision}^{{tree}}")],
    )?
    else {
        return Ok(());
    };
    let exceptions = tracked_exceptions(path, tree.trim())?;
    let operator_exceptions = validate_evidence(path, evidence)?;
    let entries = git_bytes(path, &["ls-tree", "-rz", "--full-tree", tree.trim()])?;
    for entry in entries
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let Some(separator) = entry.iter().position(|byte| *byte == b'\t') else {
            return Err(Error::git_command_failed(
                "malformed git ls-tree entry".to_string(),
            ));
        };
        let (header, raw_path) = entry.split_at(separator);
        let raw_path = &raw_path[1..];
        let mut fields = header.split(|byte| *byte == b' ');
        if fields.next() != Some(b"120000".as_slice()) {
            continue;
        }
        let Some(object) = fields.nth(1) else {
            return Err(Error::git_command_failed(
                "symlink entry has no blob object".to_string(),
            ));
        };
        let link_path = String::from_utf8_lossy(raw_path).to_string();
        let target = git_bytes(
            path,
            &[
                "cat-file",
                "blob",
                std::str::from_utf8(object).map_err(|_| {
                    Error::git_command_failed("non-UTF-8 Git object id".to_string())
                })?,
            ],
        )?;
        let violation = if is_absolute(&target) {
            Some("absolute")
        } else if lexically_escapes(&link_path, &target) {
            Some("repository-escaping relative")
        } else {
            None
        };
        if let Some(violation) = violation {
            if exceptions
                .iter()
                .any(|(exception_path, _)| exception_path == &link_path)
                || operator_exceptions.is_some_and(|exceptions| {
                    exceptions.iter().any(|exception| {
                        exception.path == link_path
                            && STANDARD.decode(&exception.target_base64).ok().as_deref()
                                == Some(target.as_slice())
                    })
                })
            {
                continue;
            }
            return Err(Error::validation_invalid_argument(
                "repository_integrity.symlink",
                format!(
                    "candidate revision {} tracks a {} symlink at `{}` with raw target bytes base64 `{}` under the default repository portability policy; replace it with an internal relative target, add an exact reviewed exception with a reason in candidate homeboy.json repository_integrity.symlink_exceptions, or admit an exact path, target_base64, and reason operator policy exception",
                    revision, violation, link_path, STANDARD.encode(&target)
                ),
                Some(link_path),
                None,
            ));
        }
    }
    Ok(())
}

fn tracked_exceptions(path: &Path, tree: &str) -> Result<Vec<(String, String)>> {
    let Some(raw) = git_optional(path, &["show", &format!("{tree}:homeboy.json")])? else {
        return Ok(Vec::new());
    };
    let config: Value = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(error, Some("candidate homeboy.json".to_string()), None)
    })?;
    let Some(entries) = config
        .pointer("/repository_integrity/symlink_exceptions")
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    entries
        .iter()
        .map(|entry| {
            let path = entry
                .get("path")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty());
            let reason = entry
                .get("reason")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty());
            match (path, reason) {
                (Some(path), Some(reason)) => Ok((path.to_string(), reason.to_string())),
                _ => Err(Error::validation_invalid_argument(
                    "repository_integrity.symlink_exceptions",
                    "each symlink exception requires exact `path` and reviewed `reason` fields",
                    Some("candidate homeboy.json".to_string()),
                    None,
                )),
            }
        })
        .collect()
}

/// Read and bind operator policy once, before a source snapshot crosses a
/// durable boundary. Validators intentionally never call this function.
pub fn collect_operator_policy_evidence(
    path: &Path,
) -> Result<Option<RepositoryIntegrityEvidence>> {
    // A path that does not exist on disk cannot be a Git repository. This is
    // "not a repo" (`Ok(None)`), the same answer as any other non-repo path,
    // not a `git` invocation failure worth propagating: spawning `git` with a
    // missing `current_dir` fails at the OS level before git itself ever
    // runs, and every other repository-integrity question here is only asked
    // once this same path is already known to exist.
    if !path.exists() {
        return Ok(None);
    }
    let Some(common_dir) = git_common_dir(path)? else {
        return Ok(None);
    };
    let policy_path = common_dir.join(OPERATOR_POLICY_FILE);
    if !policy_path.exists() {
        return Ok(None);
    }
    let metadata = fs::symlink_metadata(&policy_path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(policy_path.display().to_string()))
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(Error::validation_invalid_argument(
            "repository_integrity.operator_policy",
            format!("{OPERATOR_POLICY_FILE} in the Git common directory must be a regular non-symlink file"),
            Some(policy_path.display().to_string()),
            None,
        ));
    }
    if metadata.len() > OPERATOR_POLICY_MAX_BYTES {
        return Err(Error::validation_invalid_argument(
            "repository_integrity.operator_policy",
            format!("{OPERATOR_POLICY_FILE} exceeds {OPERATOR_POLICY_MAX_BYTES} bytes"),
            Some(policy_path.display().to_string()),
            None,
        ));
    }
    #[cfg(unix)]
    if std::os::unix::fs::MetadataExt::mode(&metadata) & 0o077 != 0
        || std::os::unix::fs::MetadataExt::uid(&metadata) != unsafe { libc::geteuid() }
    {
        return Err(Error::validation_invalid_argument(
            "repository_integrity.operator_policy",
            format!(
                "{OPERATOR_POLICY_FILE} must be owned by the current operator and mode 0600 or stricter"
            ),
            Some(policy_path.display().to_string()),
            None,
        ));
    }
    let raw = std::fs::read_to_string(&policy_path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(policy_path.display().to_string()))
    })?;
    let config: OperatorPolicy = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(error, Some(policy_path.display().to_string()), None)
    })?;
    if config.schema != OPERATOR_POLICY_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "repository_integrity.operator_policy.schema",
            format!("operator policy schema must be `{OPERATOR_POLICY_SCHEMA}`"),
            Some(policy_path.display().to_string()),
            None,
        ));
    }
    let origin = git_optional(path, &["remote", "get-url", "origin"])?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "repository_integrity.operator_policy",
            "operator policy requires a configured origin remote",
            Some(policy_path.display().to_string()),
            None,
        )
    })?;
    if config.origin.trim().is_empty() || config.origin != origin.trim() {
        return Err(Error::validation_invalid_argument(
            "repository_integrity.operator_policy.origin",
            "operator policy origin must exactly match this checkout's origin remote",
            Some(policy_path.display().to_string()),
            None,
        ));
    }
    if config.symlink_exceptions.len() > OPERATOR_POLICY_MAX_EXCEPTIONS {
        return Err(Error::validation_invalid_argument(
            "repository_integrity.operator_policy.symlink_exceptions",
            format!("operator policy supports at most {OPERATOR_POLICY_MAX_EXCEPTIONS} exceptions"),
            Some(policy_path.display().to_string()),
            None,
        ));
    }
    let mut symlink_exceptions = config.symlink_exceptions.into_iter().map(|exception| {
        if exception.path.is_empty() || exception.path.starts_with('/') || exception.path.split('/').any(|part| matches!(part, "" | "." | "..")) || exception.reason.trim().is_empty() || STANDARD.decode(&exception.target_base64).is_err() {
            return Err(Error::validation_invalid_argument("repository_integrity.operator_policy.symlink_exceptions", "each exception requires an exact repository-relative path, raw target bytes encoded as base64, and a nonempty reason", Some(policy_path.display().to_string()), None));
        }
        Ok(RepositoryIntegritySymlinkException { path: exception.path, target_base64: exception.target_base64, reason: exception.reason })
    }).collect::<Result<Vec<_>>>()?;
    symlink_exceptions.sort();
    if symlink_exceptions.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(Error::validation_invalid_argument(
            "repository_integrity.operator_policy.symlink_exceptions",
            "operator policy contains a duplicate exact exception",
            Some(policy_path.display().to_string()),
            None,
        ));
    }
    let sha256 = canonical_evidence_sha256(&config.origin, &symlink_exceptions)?;
    Ok(Some(RepositoryIntegrityEvidence {
        origin: config.origin,
        symlink_exceptions,
        sha256,
    }))
}

fn validate_evidence<'a>(
    path: &Path,
    evidence: Option<&'a RepositoryIntegrityEvidence>,
) -> Result<Option<&'a [RepositoryIntegritySymlinkException]>> {
    let Some(evidence) = evidence else {
        return Ok(None);
    };
    let origin = git_optional(path, &["remote", "get-url", "origin"])?;
    if evidence.origin.trim().is_empty()
        || origin.as_deref().map(str::trim) != Some(evidence.origin.as_str())
        || evidence.symlink_exceptions.len() > OPERATOR_POLICY_MAX_EXCEPTIONS
        || evidence
            .symlink_exceptions
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || evidence.symlink_exceptions.iter().any(|entry| {
            entry.path.is_empty()
                || entry.path.starts_with('/')
                || entry
                    .path
                    .split('/')
                    .any(|part| matches!(part, "" | "." | ".."))
                || entry.reason.trim().is_empty()
                || STANDARD.decode(&entry.target_base64).is_err()
        })
        || evidence.sha256
            != canonical_evidence_sha256(&evidence.origin, &evidence.symlink_exceptions)?
    {
        return Err(Error::validation_invalid_argument(
            "repository_integrity.evidence",
            "repository integrity evidence is not canonical or its sha256 does not match",
            None,
            None,
        ));
    }
    Ok(Some(&evidence.symlink_exceptions))
}

fn canonical_evidence_sha256(
    origin: &str,
    exceptions: &[RepositoryIntegritySymlinkException],
) -> Result<String> {
    let bytes = serde_json::to_vec(&CanonicalEvidence {
        origin,
        symlink_exceptions: exceptions,
    })
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("repository integrity evidence".to_string()),
        )
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn git_common_dir(path: &Path) -> Result<Option<PathBuf>> {
    let Some(common_dir) = git_optional(path, &["rev-parse", "--git-common-dir"])? else {
        return Ok(None);
    };
    let common_dir = PathBuf::from(common_dir.trim());
    Ok(Some(if common_dir.is_absolute() {
        common_dir
    } else {
        path.join(common_dir)
    }))
}

fn is_absolute(target: &[u8]) -> bool {
    target.starts_with(b"/")
        || target.starts_with(b"\\")
        || (target.len() >= 3
            && target[0].is_ascii_alphabetic()
            && target[1] == b':'
            && matches!(target[2], b'/' | b'\\'))
}

fn lexically_escapes(link_path: &str, target: &[u8]) -> bool {
    let mut depth = link_path.split('/').count().saturating_sub(1);
    for component in target.split(|byte| matches!(*byte, b'/' | b'\\')) {
        if component.is_empty() || component == b"." {
            continue;
        }
        if component == b".." {
            if depth == 0 {
                return true;
            }
            depth -= 1;
        } else {
            depth += 1;
        }
    }
    false
}

fn git_optional(path: &Path, args: &[&str]) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if output.status.success() {
        Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
    } else {
        Ok(None)
    }
}

fn git_bytes(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(Error::git_command_failed(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn repo(link: &str, config: Option<&str>) -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(repo.path())
            .status()
            .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(link, repo.path().join("link")).unwrap();
        if let Some(config) = config {
            fs::write(repo.path().join("homeboy.json"), config).unwrap();
        }
        Command::new("git")
            .args(["add", "."])
            .current_dir(repo.path())
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "-m",
                "fixture",
            ])
            .current_dir(repo.path())
            .status()
            .unwrap();
        repo
    }

    #[cfg(unix)]
    #[test]
    fn rejects_absolute_and_escaping_targets_but_preserves_internal_unresolved_links() {
        for target in ["/tmp/external", "../../external"] {
            let fixture = repo(target, None);
            let error = verify_tracked_symlink_portability(fixture.path(), "HEAD", None)
                .expect_err("non-portable link is rejected");
            assert!(error.message.contains("raw target bytes base64"));
            assert!(error.message.contains("homeboy.json"));
        }
        for target in ["missing", "dir/missing"] {
            let fixture = repo(target, None);
            verify_tracked_symlink_portability(fixture.path(), "HEAD", None).unwrap();
        }
        let fixture = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        fs::create_dir(fixture.path().join("dir")).unwrap();
        std::os::unix::fs::symlink("../missing", fixture.path().join("dir/link")).unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "-m",
                "nested fixture",
            ])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        verify_tracked_symlink_portability(fixture.path(), "HEAD", None).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn preserves_internal_relative_cycles() {
        let fixture = tempfile::tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        std::os::unix::fs::symlink("b", fixture.path().join("a")).unwrap();
        std::os::unix::fs::symlink("a", fixture.path().join("b")).unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "-m",
                "cycle fixture",
            ])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        verify_tracked_symlink_portability(fixture.path(), "HEAD", None).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn allows_reviewed_path_scoped_exception() {
        let fixture = repo(
            "/opt/shared",
            Some(
                r#"{"repository_integrity":{"symlink_exceptions":[{"path":"link","reason":"shared fixture dependency"}]}}"#,
            ),
        );
        verify_tracked_symlink_portability(fixture.path(), "HEAD", None).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn common_dir_policy_is_bound_and_evidence_survives_policy_mutation() {
        let fixture = repo("/srv/example-assets", None);
        Command::new("git")
            .args(["remote", "add", "origin", "ssh://example.test/homeboy.git"])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        fs::write(
            fixture.path().join(".git").join(OPERATOR_POLICY_FILE),
            r#"{"schema":"homeboy/repository-integrity-policy/v1","origin":"ssh://example.test/homeboy.git","symlink_exceptions":[{"path":"link","target_base64":"L3Nydi9leGFtcGxlLWFzc2V0cw==","reason":"Lab fixture asset mount"}]}"#,
        )
        .unwrap();
        fs::set_permissions(
            fixture.path().join(".git").join(OPERATOR_POLICY_FILE),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        let evidence = collect_operator_policy_evidence(fixture.path())
            .unwrap()
            .expect("policy evidence");

        verify_tracked_symlink_portability(fixture.path(), "HEAD", Some(&evidence)).unwrap();

        fs::write(
            fixture.path().join(".git").join(OPERATOR_POLICY_FILE),
            r#"{"schema":"homeboy/repository-integrity-policy/v1","origin":"ssh://example.test/homeboy.git","symlink_exceptions":[]}"#,
        )
        .unwrap();
        // Validation consumes the immutable captured bytes, not mutable policy.
        verify_tracked_symlink_portability(fixture.path(), "HEAD", Some(&evidence)).unwrap();
        std::os::unix::fs::symlink("/srv/other-assets", fixture.path().join("other-link")).unwrap();
        Command::new("git")
            .args(["add", "other-link"])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "-m",
                "add another link",
            ])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        assert!(
            verify_tracked_symlink_portability(fixture.path(), "HEAD", Some(&evidence)).is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_mutated_evidence_and_insecure_or_wrong_origin_common_dir_policy() {
        let fixture = repo("/srv/example-assets", None);
        fs::write(
            fixture.path().join(".git").join(OPERATOR_POLICY_FILE),
            r#"{"schema":"homeboy/repository-integrity-policy/v1","origin":"ssh://wrong.test/homeboy.git","symlink_exceptions":[]}"#,
        )
        .unwrap();
        fs::set_permissions(
            fixture.path().join(".git").join(OPERATOR_POLICY_FILE),
            std::os::unix::fs::PermissionsExt::from_mode(0o644),
        )
        .unwrap();
        let error = collect_operator_policy_evidence(fixture.path()).unwrap_err();
        assert!(error.message.contains("mode 0600"));

        Command::new("git")
            .args(["remote", "add", "origin", "ssh://example.test/homeboy.git"])
            .current_dir(fixture.path())
            .status()
            .unwrap();
        fs::set_permissions(
            fixture.path().join(".git").join(OPERATOR_POLICY_FILE),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .unwrap();
        let error = collect_operator_policy_evidence(fixture.path()).unwrap_err();
        assert!(error.message.contains("exactly match"));

        let mut evidence = RepositoryIntegrityEvidence {
            origin: "ssh://example.test/homeboy.git".to_string(),
            symlink_exceptions: vec![RepositoryIntegritySymlinkException {
                path: "link".to_string(),
                target_base64: "L3Nydi9leGFtcGxlLWFzc2V0cw==".to_string(),
                reason: "fixture".to_string(),
            }],
            sha256: canonical_evidence_sha256(
                "ssh://example.test/homeboy.git",
                &[RepositoryIntegritySymlinkException {
                    path: "link".to_string(),
                    target_base64: "L3Nydi9leGFtcGxlLWFzc2V0cw==".to_string(),
                    reason: "fixture".to_string(),
                }],
            )
            .unwrap(),
        };
        evidence.symlink_exceptions[0].reason = "tampered".to_string();
        assert!(
            verify_tracked_symlink_portability(fixture.path(), "HEAD", Some(&evidence)).is_err()
        );
    }
}
