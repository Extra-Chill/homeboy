use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use homeboy::core::{Error, Result};

const DEFAULT_BRANCH_RESOLUTION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Serialize)]
pub(crate) struct DefaultBranchResolution {
    pub base: String,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_path: Option<String>,
    pub precedence: Vec<&'static str>,
}

pub(crate) struct DefaultBranchRequest<'a> {
    pub explicit_base: Option<&'a str>,
    pub explicit_from: Option<&'a str>,
    pub workspace: Option<&'a Path>,
    pub component: Option<&'a Path>,
    pub destination: Option<&'a Path>,
    /// Preserve single-Cook's deferred provider behavior when repository
    /// evidence is not available until after materialization.
    pub compatibility_fallback: Option<&'a str>,
}

pub(crate) fn resolve_default_branch(
    request: DefaultBranchRequest<'_>,
) -> Result<DefaultBranchResolution> {
    let started = Instant::now();
    let candidates = [
        request.workspace.map(|path| (path, "workspace_upstream")),
        request.component.map(|path| (path, "repository_metadata")),
        request.destination.map(|path| (path, "remote_head")),
    ];
    let mut existing = Vec::<(PathBuf, &str)>::new();
    for (path, source) in candidates.into_iter().flatten() {
        if let Some(root) = bounded_git_read(
            path,
            &["rev-parse", "--show-toplevel"],
            "repository root",
            started,
        )? {
            existing.push((PathBuf::from(root), source));
        }
    }

    let (path, base, inferred_from, source) = if let Some(base) = request.explicit_base {
        let path = existing.first().map(|(path, _)| path.clone());
        if path.is_none() && request.compatibility_fallback.is_none() {
            return Err(missing_default_branch_error());
        }
        let remote = path
            .as_deref()
            .map(|path| resolve_default_remote(path, started))
            .transpose()?
            .unwrap_or_else(|| "origin".to_string());
        (
            path,
            base.to_string(),
            format!("{remote}/{base}"),
            "explicit",
        )
    } else {
        let mut resolved = None;
        for (path, candidate_source) in &existing {
            let remote_ref = if *candidate_source == "workspace_upstream" {
                match git_upstream_branch(path, started)? {
                    Some(upstream) => Some(upstream),
                    None => default_remote_branch(path, started)?,
                }
            } else {
                default_remote_branch(path, started)?
            };
            if let Some(remote_ref) = remote_ref {
                let base = remote_ref
                    .split_once('/')
                    .map(|(_, branch)| branch)
                    .unwrap_or(&remote_ref)
                    .to_string();
                resolved = Some((Some(path.clone()), base, remote_ref, *candidate_source));
                break;
            }
        }
        resolved.unwrap_or_else(|| {
            let base = request
                .compatibility_fallback
                .unwrap_or_default()
                .to_string();
            (
                None,
                base.clone(),
                format!("origin/{base}"),
                "compatibility_fallback",
            )
        })
    };

    if base.is_empty() {
        return Err(missing_default_branch_error());
    }

    let from = request.explicit_from.unwrap_or(&inferred_from).to_string();
    let (sha, evidence_path) = if let Some(path) = path {
        let sha = resolve_commit(&path, &from, started)?;
        let remote = resolve_default_remote(&path, started)?;
        let base_ref = format!("{remote}/{base}");
        let base_sha = match resolve_commit(&path, &base_ref, started)? {
            Some(sha) => Some(sha),
            None => resolve_commit(&path, &base, started)?,
        };
        if request.compatibility_fallback.is_none() {
            let sha = sha.ok_or_else(|| unavailable_ref_error("from", &from, &remote))?;
            let base_sha = base_sha.ok_or_else(|| unavailable_ref_error("base", &base, &remote))?;
            if sha != base_sha {
                return Err(Error::validation_invalid_argument(
                    "from",
                    format!("source ref `{from}` does not resolve to declared base `{base}`"),
                    Some(from),
                    Some(vec![format!(
                        "Use the declared base ref: --from {base_ref} --base {base}"
                    )]),
                ));
            }
            (Some(sha), Some(path.display().to_string()))
        } else {
            if sha.is_some() && base_sha.is_some() && sha != base_sha {
                return Err(Error::validation_invalid_argument(
                    "from",
                    format!("source ref `{from}` does not resolve to declared base `{base}`"),
                    Some(from),
                    Some(vec![format!(
                        "Use the declared base ref: --from {base_ref} --base {base}"
                    )]),
                ));
            }
            (sha, Some(path.display().to_string()))
        }
    } else {
        (None, None)
    };

    Ok(DefaultBranchResolution {
        base,
        from,
        sha,
        source: if request.explicit_from.is_some() {
            format!("{source}+explicit_from")
        } else {
            source.to_string()
        },
        evidence_path,
        precedence: vec![
            "explicit_from",
            "explicit_base",
            "workspace_upstream",
            "repository_metadata",
            "remote_head",
        ],
    })
}

fn git_upstream_branch(path: &Path, started: Instant) -> Result<Option<String>> {
    Ok(bounded_git_read(
        path,
        &["rev-parse", "--abbrev-ref", "@{upstream}"],
        "workspace upstream",
        started,
    )?
    .map(|value| value.trim().to_string())
    .filter(|value| value.contains('/')))
}

fn resolve_commit(path: &Path, reference: &str, started: Instant) -> Result<Option<String>> {
    Ok(bounded_git_read(
        path,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ],
        "commit resolution",
        started,
    )?
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty()))
}

fn default_remote_branch(path: &Path, started: Instant) -> Result<Option<String>> {
    homeboy::core::git::default_remote_branch_until(
        path,
        started + DEFAULT_BRANCH_RESOLUTION_TIMEOUT,
    )
    .map_err(|error| {
        if started.elapsed() >= DEFAULT_BRANCH_RESOLUTION_TIMEOUT {
            default_branch_timeout_error(path, "remote default branch", started.elapsed())
        } else {
            error
        }
    })
}

fn resolve_default_remote(path: &Path, started: Instant) -> Result<String> {
    homeboy::core::git::resolve_default_remote_until(
        path,
        started + DEFAULT_BRANCH_RESOLUTION_TIMEOUT,
    )
    .map_err(|error| {
        if started.elapsed() >= DEFAULT_BRANCH_RESOLUTION_TIMEOUT {
            default_branch_timeout_error(path, "default remote", started.elapsed())
        } else {
            error
        }
    })
}

fn bounded_git_read(
    path: &Path,
    args: &[&str],
    operation: &str,
    started: Instant,
) -> Result<Option<String>> {
    let elapsed = started.elapsed();
    let remaining = DEFAULT_BRANCH_RESOLUTION_TIMEOUT.saturating_sub(elapsed);
    if remaining.is_zero() {
        return Err(default_branch_timeout_error(path, operation, elapsed));
    }
    match homeboy::core::git::output_optional_within(path, args, remaining) {
        homeboy::core::git::BoundedGitRead::Resolved(value) => Ok(Some(value)),
        homeboy::core::git::BoundedGitRead::Unresolved => Ok(None),
        homeboy::core::git::BoundedGitRead::TimedOut => Err(default_branch_timeout_error(
            path,
            operation,
            started.elapsed(),
        )),
    }
}

fn default_branch_timeout_error(path: &Path, operation: &str, elapsed: Duration) -> Error {
    let mut error = Error::validation_invalid_argument(
        "base",
        format!(
            "Cook destination resolution timed out during {operation} after {}ms",
            elapsed.as_millis()
        ),
        Some(path.display().to_string()),
        Some(vec![format!(
            "Inspect Git responsiveness at {}; then retry Cook.",
            path.display()
        )]),
    );
    error.details["destination_resolution"] = serde_json::json!({
        "dependency": "git",
        "operation": operation,
        "path": path.display().to_string(),
        "elapsed_ms": elapsed.as_millis(),
        "timeout_ms": DEFAULT_BRANCH_RESOLUTION_TIMEOUT.as_millis(),
    });
    error
}

fn missing_default_branch_error() -> Error {
    Error::validation_invalid_argument(
        "base",
        "repository default branch could not be inferred from configured or remote evidence",
        None,
        Some(vec![
            "Set the remote default explicitly: git remote set-head origin --auto".to_string(),
        ]),
    )
}

fn unavailable_ref_error(field: &str, reference: &str, remote: &str) -> Error {
    let fetch_ref = reference
        .strip_prefix(&format!("{remote}/"))
        .unwrap_or(reference);
    Error::validation_invalid_argument(
        field,
        format!("resolved {field} ref `{reference}` is unavailable"),
        Some(reference.to_string()),
        Some(vec![format!(
            "Fetch the ref before retrying: git fetch {remote} {fetch_ref}"
        )]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn resolves_main_trunk_and_nonstandard_remote_defaults_while_detached() {
        for branch in ["main", "trunk", "release/2026"] {
            let (fixture, checkout) = default_branch_checkout(branch);
            git(&checkout, &["checkout", "--detach"]);

            let resolution = resolve_default_branch(DefaultBranchRequest {
                explicit_base: None,
                explicit_from: None,
                workspace: None,
                component: Some(&checkout),
                destination: None,
                compatibility_fallback: None,
            })
            .expect("resolve remote default while detached");

            assert_eq!(resolution.base, branch);
            assert_eq!(resolution.from, format!("origin/{branch}"));
            assert_eq!(resolution.source, "repository_metadata");
            assert_eq!(resolution.sha.as_deref().map(str::len), Some(40));
            drop(fixture);
        }
    }

    #[test]
    fn explicit_overrides_win_and_conflicting_or_missing_refs_fail() {
        let (_fixture, checkout) = default_branch_checkout("trunk");
        let explicit = resolve_default_branch(DefaultBranchRequest {
            explicit_base: Some("trunk"),
            explicit_from: Some("origin/trunk"),
            workspace: None,
            component: Some(&checkout),
            destination: None,
            compatibility_fallback: None,
        })
        .expect("explicit refs resolve");
        assert_eq!(explicit.base, "trunk");
        assert_eq!(explicit.from, "origin/trunk");
        assert_eq!(explicit.source, "explicit+explicit_from");

        git(&checkout, &["branch", "other"]);
        git(&checkout, &["commit", "--allow-empty", "-m", "other"]);
        let conflict = resolve_default_branch(DefaultBranchRequest {
            explicit_base: Some("trunk"),
            explicit_from: Some("HEAD"),
            workspace: None,
            component: Some(&checkout),
            destination: None,
            compatibility_fallback: None,
        })
        .expect_err("conflicting refs fail");
        assert_eq!(conflict.details["field"], "from");
        assert_eq!(conflict.details["tried"].as_array().map(Vec::len), Some(1));

        let missing = resolve_default_branch(DefaultBranchRequest {
            explicit_base: Some("missing"),
            explicit_from: None,
            workspace: None,
            component: Some(&checkout),
            destination: None,
            compatibility_fallback: None,
        })
        .expect_err("missing base fails");
        assert_eq!(missing.details["field"], "from");
    }

    #[test]
    fn compatibility_mode_preserves_deferred_cook_base_resolution() {
        let fixture = tempfile::tempdir().expect("fixture");
        let resolution = resolve_default_branch(DefaultBranchRequest {
            explicit_base: None,
            explicit_from: None,
            workspace: None,
            component: Some(fixture.path()),
            destination: None,
            compatibility_fallback: Some("main"),
        })
        .expect("defer Cook base validation until materialization");

        assert_eq!(resolution.base, "main");
        assert_eq!(resolution.from, "origin/main");
        assert_eq!(resolution.source, "compatibility_fallback");
        assert_eq!(resolution.sha, None);
        assert_eq!(resolution.evidence_path, None);
    }

    #[test]
    fn resolves_default_branch_from_a_component_subdirectory() {
        let (_fixture, checkout) = default_branch_checkout("main");
        let component = checkout.join("components").join("database");
        std::fs::create_dir_all(&component).expect("create component subdirectory");

        let resolution = resolve_default_branch(DefaultBranchRequest {
            explicit_base: None,
            explicit_from: None,
            workspace: None,
            component: Some(&component),
            destination: None,
            compatibility_fallback: None,
        })
        .expect("resolve default branch from component subdirectory");

        assert_eq!(resolution.base, "main");
        assert_eq!(
            resolution.evidence_path.as_deref(),
            Some(checkout.to_str().unwrap())
        );
    }

    #[cfg(unix)]
    #[test]
    fn delayed_git_probe_returns_bounded_destination_resolution_evidence() {
        let test_name = homeboy::core::test_support::harness_test_name(
            module_path!(),
            "delayed_git_probe_returns_bounded_destination_resolution_evidence_child",
        );
        let output = homeboy::core::test_support::run_child_test(
            Command::new(std::env::current_exe().expect("current test executable"))
                .arg("--exact")
                .arg(&test_name)
                .arg("--ignored"),
            &test_name,
        );
        assert!(
            output.status.success(),
            "delayed probe child failed: {output:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "runs under the parent test with an isolated delayed Git probe"]
    fn delayed_git_probe_returns_bounded_destination_resolution_evidence_child() {
        use std::os::unix::fs::PermissionsExt;

        let (_fixture, checkout) = default_branch_checkout("main");
        let bin = tempfile::tempdir().expect("fake git bin");
        let git = bin.path().join("git");
        std::fs::write(&git, "#!/bin/sh\nsleep 30\n").expect("write delayed git");
        let mut permissions = std::fs::metadata(&git).expect("git metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&git, permissions).expect("make fake git executable");
        let _path = homeboy::core::test_support::EnvVarGuard::set(
            "PATH",
            format!(
                "{}:{}",
                bin.path().display(),
                std::env::var_os("PATH")
                    .as_deref()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
        );

        let started = Instant::now();
        let error = resolve_default_branch(DefaultBranchRequest {
            explicit_base: None,
            explicit_from: None,
            workspace: None,
            component: Some(&checkout),
            destination: None,
            compatibility_fallback: Some("main"),
        })
        .expect_err("a delayed Git resolver must not block Cook indefinitely");

        assert!(
            started.elapsed() < Duration::from_secs(7),
            "default-base resolution exceeded its bounded budget"
        );
        assert_eq!(error.details["destination_resolution"]["dependency"], "git");
        assert_eq!(
            error.details["destination_resolution"]["operation"],
            "repository root"
        );
        assert_eq!(
            error.details["destination_resolution"]["timeout_ms"],
            serde_json::json!(DEFAULT_BRANCH_RESOLUTION_TIMEOUT.as_millis())
        );
    }

    #[cfg(unix)]
    #[test]
    fn delayed_remote_lookup_returns_bounded_destination_resolution_evidence() {
        let test_name = homeboy::core::test_support::harness_test_name(
            module_path!(),
            "delayed_remote_lookup_returns_bounded_destination_resolution_evidence_child",
        );
        let output = homeboy::core::test_support::run_child_test(
            Command::new(std::env::current_exe().expect("current test executable"))
                .arg("--exact")
                .arg(&test_name)
                .arg("--ignored"),
            &test_name,
        );
        assert!(
            output.status.success(),
            "delayed remote child failed: {output:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "runs under the parent test with an isolated delayed Git remote"]
    fn delayed_remote_lookup_returns_bounded_destination_resolution_evidence_child() {
        use std::os::unix::fs::PermissionsExt;

        let (_fixture, checkout) = default_branch_checkout("main");
        let bin = tempfile::tempdir().expect("fake git bin");
        let git = bin.path().join("git");
        std::fs::write(
            &git,
            "#!/bin/sh\nif [ \"$1\" = rev-parse ] && [ \"$2\" = --show-toplevel ]; then printf '%s\\n' \"$PWD\"; exit 0; fi\nif [ \"$1\" = remote ]; then sleep 30; exit 0; fi\nPATH=/usr/bin:/bin git \"$@\"\n",
        )
        .expect("write delayed remote git");
        let mut permissions = std::fs::metadata(&git).expect("git metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&git, permissions).expect("make fake git executable");
        let _path = homeboy::core::test_support::EnvVarGuard::set(
            "PATH",
            format!(
                "{}:{}",
                bin.path().display(),
                std::env::var_os("PATH")
                    .as_deref()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
        );

        let started = Instant::now();
        let error = resolve_default_branch(DefaultBranchRequest {
            explicit_base: None,
            explicit_from: None,
            workspace: None,
            component: Some(&checkout),
            destination: None,
            compatibility_fallback: Some("main"),
        })
        .expect_err("a delayed remote lookup must not block Cook indefinitely");

        assert!(
            started.elapsed() < Duration::from_secs(7),
            "remote resolution exceeded its bounded budget"
        );
        assert_eq!(
            error.details["destination_resolution"]["operation"],
            "remote default branch"
        );
    }

    fn default_branch_checkout(branch: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let fixture = tempfile::tempdir().expect("fixture");
        let remote = fixture.path().join("remote.git");
        let seed = fixture.path().join("seed");
        let checkout = fixture.path().join("checkout");
        git(
            fixture.path(),
            &[
                "init",
                "--bare",
                "--initial-branch",
                branch,
                remote.to_str().unwrap(),
            ],
        );
        git(
            fixture.path(),
            &["init", "--initial-branch", branch, seed.to_str().unwrap()],
        );
        git(&seed, &["config", "user.email", "test@example.com"]);
        git(&seed, &["config", "user.name", "Test"]);
        git(&seed, &["commit", "--allow-empty", "-m", "initial"]);
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "origin", branch]);
        git(
            fixture.path(),
            &[
                "clone",
                remote.to_str().unwrap(),
                checkout.to_str().unwrap(),
            ],
        );
        git(&checkout, &["config", "user.email", "test@example.com"]);
        git(&checkout, &["config", "user.name", "Test"]);
        (fixture, checkout)
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
