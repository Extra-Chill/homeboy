//! Git submodule materialization for detached checkouts (#15355).
//!
//! `git worktree add --detach` never initializes submodules, so a repository
//! that vendors workspace packages as submodules cannot install, build, or
//! test in a freshly materialized Cook checkout. This step initializes them
//! before any provider or gate runs there, and reports the result through the
//! same dependency hydration evidence as package-manager installs.
//!
//! Submodules that are already checked out at the same path in the source
//! checkout are cloned from that local copy first (offline, and it includes
//! pinned commits that were never pushed). Anything that cannot be satisfied
//! locally falls back to the submodule's configured URL.

use super::{
    hydration_outcome, DependencyHydrationOutcome, DependencyHydrationStatus,
    DependencyHydrationTermination, DEPENDENCY_HYDRATION_OUTPUT_LIMIT_BYTES,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Provider identity recorded on submodule hydration evidence.
pub const SUBMODULE_HYDRATION_PROVIDER_ID: &str = "git-submodules";

/// Default deadline for one submodule initialization pass.
pub const SUBMODULE_HYDRATION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// A submodule declared in `.gitmodules`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeclaredSubmodule {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) url: Option<String>,
}

/// Initialize `checkout`'s submodules recursively.
///
/// Returns `None` when the checkout declares no submodules. Otherwise returns
/// one outcome describing the pass that ran; a failure is reported as a
/// `Failed` outcome rather than an error, so callers decide whether a missing
/// submodule should block their work.
pub fn hydrate_git_submodules(
    checkout: &Path,
    source_root: Option<&Path>,
    workspace: &str,
    timeout: Duration,
) -> Option<DependencyHydrationOutcome> {
    if !checkout.join(".gitmodules").is_file() {
        return None;
    }
    let started = Instant::now();
    let submodules = match declared_submodules(checkout) {
        Ok(submodules) if submodules.is_empty() => return None,
        Ok(submodules) => submodules,
        Err(detail) => {
            return Some(outcome(
                workspace,
                checkout,
                list_command(),
                "read .gitmodules".to_string(),
                started,
                DependencyHydrationTermination::ExitFailure,
                DependencyHydrationStatus::Failed,
                None,
                String::new(),
                detail,
            ));
        }
    };

    let mut sources: Vec<PathBuf> = source_root.map(Path::to_path_buf).into_iter().collect();
    if let Some(primary) = primary_checkout(checkout) {
        if !sources.contains(&primary) {
            sources.push(primary);
        }
    }
    let local_overrides = local_source_overrides(&sources, &submodules);
    let deadline = started + timeout;

    if !local_overrides.is_empty() {
        let args = update_args(&local_overrides);
        let attempt = run_update(checkout, &args, deadline);
        if attempt.success {
            return Some(attempt.into_outcome(
                workspace,
                checkout,
                args,
                format!(
                    "initialized {} submodule(s); {} from the source checkout",
                    submodules.len(),
                    local_overrides.len()
                ),
                started,
            ));
        }
    }

    let args = update_args(&[]);
    let attempt = run_update(checkout, &args, deadline);
    let reason = if attempt.success {
        format!(
            "initialized {} submodule(s) from their remotes",
            submodules.len()
        )
    } else {
        format!("failed to initialize {} submodule(s)", submodules.len())
    };
    Some(attempt.into_outcome(workspace, checkout, args, reason, started))
}

/// Read the submodules declared in `checkout/.gitmodules`.
pub(crate) fn declared_submodules(checkout: &Path) -> Result<Vec<DeclaredSubmodule>, String> {
    let output = std::process::Command::new("git")
        .args(list_command().iter().skip(1))
        .current_dir(checkout)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    // `git config --get-regexp` exits 1 when nothing matches.
    if !output.status.success() && output.status.code() != Some(1) {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let mut submodules = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Some((key, path)) = line.split_once(' ') else {
            continue;
        };
        let Some(name) = key
            .strip_prefix("submodule.")
            .and_then(|rest| rest.strip_suffix(".path"))
        else {
            continue;
        };
        // `git submodule update` fetches from the URL in repository config
        // when one is already registered there, so the rewrite must match it.
        let key = format!("submodule.{name}.url");
        let url = config_value(checkout, &["config", "--get", &key]).or_else(|| {
            config_value(
                checkout,
                &["config", "--file", ".gitmodules", "--get", &key],
            )
        });
        submodules.push(DeclaredSubmodule {
            name: name.to_string(),
            path: path.trim().to_string(),
            url,
        });
    }
    Ok(submodules)
}

fn config_value(checkout: &Path, args: &[&str]) -> Option<String> {
    std::process::Command::new("git")
        .args(args)
        .current_dir(checkout)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

/// `url.<local>.insteadOf=<remote>` rewrites for submodules that one of the
/// local `sources` already has checked out (first match wins). Relative URLs
/// are left to Git's normal resolution.
pub(crate) fn local_source_overrides(
    sources: &[PathBuf],
    submodules: &[DeclaredSubmodule],
) -> Vec<(PathBuf, String)> {
    submodules
        .iter()
        .filter_map(|submodule| {
            let url = submodule.url.as_deref()?;
            if url.starts_with("./") || url.starts_with("../") {
                return None;
            }
            sources
                .iter()
                .map(|source| source.join(&submodule.path))
                .find(|local| local.join(".git").exists())
                .map(|local| (local, url.to_string()))
        })
        .collect()
}

/// The primary (non-linked) checkout that owns `checkout`'s repository, when
/// it is a normal working tree. Linked worktrees, including detached Cook
/// checkouts, share its object store and usually its initialized submodules.
fn primary_checkout(checkout: &Path) -> Option<PathBuf> {
    let common_dir = config_value(
        checkout,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common_dir = PathBuf::from(common_dir);
    if common_dir.file_name()? != ".git" {
        return None;
    }
    let primary = common_dir.parent()?.to_path_buf();
    (primary != checkout).then_some(primary)
}

fn list_command() -> Vec<String> {
    [
        "git",
        "config",
        "--file",
        ".gitmodules",
        "--get-regexp",
        r"^submodule\..*\.path$",
    ]
    .iter()
    .map(|part| part.to_string())
    .collect()
}

pub(crate) fn update_args(local_overrides: &[(PathBuf, String)]) -> Vec<String> {
    let mut args = Vec::new();
    if !local_overrides.is_empty() {
        // Local clones use the file transport, which Git restricts for
        // submodules by default.
        args.push("-c".to_string());
        args.push("protocol.file.allow=always".to_string());
        for (local, url) in local_overrides {
            args.push("-c".to_string());
            args.push(format!("url.{}.insteadOf={url}", local.display()));
        }
    }
    for part in ["submodule", "update", "--init", "--recursive"] {
        args.push(part.to_string());
    }
    args
}

struct UpdateAttempt {
    success: bool,
    exit_code: Option<i32>,
    termination: DependencyHydrationTermination,
    stdout: String,
    stderr: String,
}

impl UpdateAttempt {
    fn into_outcome(
        self,
        workspace: &str,
        checkout: &Path,
        args: Vec<String>,
        reason: String,
        started: Instant,
    ) -> DependencyHydrationOutcome {
        let mut command = vec!["git".to_string()];
        command.extend(args);
        let status = if self.success {
            DependencyHydrationStatus::Succeeded
        } else {
            DependencyHydrationStatus::Failed
        };
        outcome(
            workspace,
            checkout,
            command,
            reason,
            started,
            self.termination,
            status,
            self.exit_code,
            self.stdout,
            self.stderr,
        )
    }
}

fn run_update(checkout: &Path, args: &[String], deadline: Instant) -> UpdateAttempt {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return UpdateAttempt {
            success: false,
            exit_code: None,
            termination: DependencyHydrationTermination::TimedOut,
            stdout: String::new(),
            stderr: String::new(),
        };
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    // Never block on an interactive credential prompt.
    let env = [("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())];
    match crate::git::run_git_output_with_env_timeout(
        checkout,
        &arg_refs,
        "initialize checkout submodules",
        &env,
        remaining,
    ) {
        Ok(output) => UpdateAttempt {
            success: output.status.success(),
            exit_code: output.status.code(),
            termination: if output.status.success() {
                DependencyHydrationTermination::Completed
            } else {
                DependencyHydrationTermination::ExitFailure
            },
            stdout: bounded(&output.stdout),
            stderr: bounded(&output.stderr),
        },
        Err(error) => UpdateAttempt {
            success: false,
            exit_code: None,
            termination: if Instant::now() >= deadline {
                DependencyHydrationTermination::TimedOut
            } else {
                DependencyHydrationTermination::SpawnFailed
            },
            stdout: String::new(),
            stderr: crate::redaction::redact_string(&error.message),
        },
    }
}

fn bounded(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    if text.len() <= DEPENDENCY_HYDRATION_OUTPUT_LIMIT_BYTES {
        return crate::redaction::redact_string(text);
    }
    let mut end = DEPENDENCY_HYDRATION_OUTPUT_LIMIT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    crate::redaction::redact_string(&text[..end])
}

#[allow(clippy::too_many_arguments)]
fn outcome(
    workspace: &str,
    checkout: &Path,
    command: Vec<String>,
    reason: String,
    started: Instant,
    termination: DependencyHydrationTermination,
    status: DependencyHydrationStatus,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
) -> DependencyHydrationOutcome {
    hydration_outcome(
        workspace,
        ".",
        SUBMODULE_HYDRATION_PROVIDER_ID.to_string(),
        command,
        checkout.display().to_string(),
        reason,
        started.elapsed(),
        termination,
        status,
        exit_code,
        stdout,
        stderr,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "protocol.file.allow=always",
            ])
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A superproject with one submodule whose remote URL is unreachable, so
    /// only the local source checkout can satisfy it.
    fn fixture(root: &Path) -> (PathBuf, String) {
        let library = root.join("library");
        std::fs::create_dir_all(&library).unwrap();
        git(&library, &["init", "-q"]);
        std::fs::write(library.join("lib.txt"), "library\n").unwrap();
        git(&library, &["add", "."]);
        git(&library, &["commit", "-q", "-m", "library"]);

        let source = root.join("source");
        std::fs::create_dir_all(&source).unwrap();
        git(&source, &["init", "-q"]);
        git(
            &source,
            &[
                "submodule",
                "add",
                "-q",
                &library.display().to_string(),
                "vendor/library",
            ],
        );
        // Point the declared URL somewhere that cannot be fetched.
        let unreachable = "https://example.invalid/library.git".to_string();
        git(
            &source,
            &[
                "config",
                "--file",
                ".gitmodules",
                "submodule.vendor/library.url",
                &unreachable,
            ],
        );
        // Register the unreachable URL in repository config too, as a real
        // clone of the superproject would have it.
        git(&source, &["submodule", "sync", "-q"]);
        git(&source, &["add", "."]);
        git(&source, &["commit", "-q", "-m", "superproject"]);
        (source, unreachable)
    }

    #[test]
    fn checkout_without_gitmodules_has_nothing_to_hydrate() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q"]);
        assert!(
            hydrate_git_submodules(root.path(), None, "test", SUBMODULE_HYDRATION_TIMEOUT)
                .is_none()
        );
    }

    #[test]
    fn reads_declared_submodules() {
        let root = tempfile::tempdir().unwrap();
        let (source, url) = fixture(root.path());
        let declared = declared_submodules(&source).unwrap();
        assert_eq!(
            declared,
            vec![DeclaredSubmodule {
                name: "vendor/library".to_string(),
                path: "vendor/library".to_string(),
                url: Some(url),
            }]
        );
    }

    #[test]
    fn relative_urls_are_never_rewritten() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("sub/.git")).unwrap();
        let declared = vec![DeclaredSubmodule {
            name: "sub".to_string(),
            path: "sub".to_string(),
            url: Some("../sub.git".to_string()),
        }];
        assert!(local_source_overrides(&[root.path().to_path_buf()], &declared).is_empty());
    }

    #[test]
    fn detached_worktree_initializes_submodules_from_the_source_checkout() {
        let root = tempfile::tempdir().unwrap();
        let (source, _) = fixture(root.path());
        let checkout = root.path().join("attempt");
        git(
            &source,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &checkout.display().to_string(),
                "HEAD",
            ],
        );
        assert!(!checkout.join("vendor/library/lib.txt").exists());

        let outcome = hydrate_git_submodules(
            &checkout,
            Some(&source),
            "test",
            SUBMODULE_HYDRATION_TIMEOUT,
        )
        .expect("submodules declared");

        assert_eq!(
            outcome.status,
            DependencyHydrationStatus::Succeeded,
            "{outcome:?}"
        );
        assert_eq!(outcome.provider_id, SUBMODULE_HYDRATION_PROVIDER_ID);
        assert!(checkout.join("vendor/library/lib.txt").is_file());
        assert!(outcome.command.iter().any(|arg| arg.starts_with("url.")));
    }

    #[test]
    fn primary_checkout_is_used_when_no_source_is_given() {
        let root = tempfile::tempdir().unwrap();
        let (source, _) = fixture(root.path());
        let checkout = root.path().join("attempt");
        git(
            &source,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                &checkout.display().to_string(),
                "HEAD",
            ],
        );

        let outcome = hydrate_git_submodules(&checkout, None, "test", SUBMODULE_HYDRATION_TIMEOUT)
            .expect("submodules declared");

        assert_eq!(
            outcome.status,
            DependencyHydrationStatus::Succeeded,
            "{outcome:?}"
        );
        assert!(checkout.join("vendor/library/lib.txt").is_file());
    }

    #[test]
    fn unreachable_submodule_without_local_source_reports_failure() {
        let root = tempfile::tempdir().unwrap();
        let (source, _) = fixture(root.path());
        // A plain clone shares nothing with the source, so the submodule can
        // only come from its unreachable remote.
        let checkout = root.path().join("clone");
        git(
            root.path(),
            &[
                "clone",
                "-q",
                &source.display().to_string(),
                &checkout.display().to_string(),
            ],
        );

        let outcome = hydrate_git_submodules(&checkout, None, "test", SUBMODULE_HYDRATION_TIMEOUT)
            .expect("submodules declared");

        assert_eq!(outcome.status, DependencyHydrationStatus::Failed);
        assert!(!checkout.join("vendor/library/lib.txt").exists());
    }
}
