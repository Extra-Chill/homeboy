//! Runner-side component-build freshness detection for Lab runtime overlays.
//!
//! A runtime overlay (see [`super::lab_workspaces`]) syncs a *built* runtime
//! artifact directory (e.g. a packaged CLI `dist/`) from the controller to the
//! runner. The artifact is produced outside Homeboy (an opaque build step such
//! as `npm run build`), so a built dist can drift behind the source it was
//! compiled from without any signal — an offload then runs *old code* even
//! though the fix is on the source branch (#6965).
//!
//! This module derives generic, ecosystem-agnostic build provenance for an
//! artifact directory by comparing the source checkout that contains it against
//! when the artifact was last built:
//!
//! - `source_sha` — `git HEAD` of the checkout containing the artifact dir.
//! - `built_from_sha` — the most recent commit at or before the artifact's
//!   newest file mtime (the commit the dist most plausibly reflects).
//! - `commits_behind` — `git rev-list --count built_from_sha..HEAD`.
//!
//! When `built_from_sha` differs from `source_sha` the build is stale: the
//! source has commits the dist predates. The check is computed on the
//! controller-local artifact directory (real mtimes) before it is snapshotted
//! to the runner.
//!
//! # Why this is a heuristic and cannot become a content comparison
//!
//! This module answers a *derivation* question — "is this built output behind
//! the source it was compiled from?" — not an *identity* question. An artifact
//! and its source are different bytes by construction, so no digest computed
//! over either one can be compared against the other. A derivation can only be
//! bound by evidence recorded at build time by the build step, and the build
//! step here is opaque and outside this product's control: there is nothing to
//! ask for a source identity, and nowhere to have asked it.
//!
//! That leaves timestamps, whose weakness is inherent rather than incidental:
//! transport and checkout both write mtimes, so a freshly created checkout
//! makes every artifact in it look newly built. The shared currency contract
//! records this evidence class as `CurrencyEvidence::BuildTimestamp`, one that
//! should prefer an unknown verdict over a confident one.
//!
//! # Traversal caveat
//!
//! The mtime walk takes the *newest* file time under the artifact directory, so
//! anything written into that directory after the build — a dependency tree
//! installed in place, a cache, a scratch area — drags the apparent build time
//! forward and can mask a stale build. The skip list below covers the common
//! case by name only; it is not a general solution, and a directory it does not
//! name will still hide staleness.

use std::path::Path;
use std::time::SystemTime;

use homeboy_core::materialization_currency::{Currency, CurrencyEvidence};
use serde::Serialize;

use super::workspace::git_output;

/// Directory names skipped by the mtime walk because they are written by
/// something other than the build and would otherwise drag the apparent build
/// time forward. Matched by exact name at any depth.
///
/// Named by convention, not derived from any declared project model, so this
/// list is a mitigation rather than a rule. See the traversal caveat above.
const MTIME_WALK_SKIPPED_DIRECTORY_NAMES: &[&str] = &[".git", "node_modules"];

/// Env var that escalates a stale runtime-overlay build from a warning to a hard
/// error for a single run. Accepts the usual truthy spellings (`1`, `true`,
/// `yes`, `on`). Mirrors the opt-in gate style already used for the
/// controller↔runner version check.
pub(super) const REQUIRE_FRESH_RUNTIME_OVERLAY_ENV: &str =
    concat!("HOME", "BOY_REQUIRE_FRESH_RUNTIME_OVERLAY");

/// Build provenance for a runtime overlay's artifact directory. Folded into the
/// synced-overlay record so a stale build is auditable from `homeboy runs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct RuntimeOverlayBuildProvenance {
    /// Coarse verdict for the artifact's freshness relative to its source.
    pub(super) status: RuntimeOverlayBuildStatus,
    /// The same verdict in the shared currency vocabulary, so a reader of a run
    /// record can compare this against every other materialization's verdict
    /// without knowing this module's private status names. Derived from
    /// `status`; see [`RuntimeOverlayBuildStatus::currency`].
    pub(super) currency: Currency,
    /// `true` only when the build is provably behind its source checkout.
    pub(super) stale: bool,
    /// Path to the git checkout that contains the artifact directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) source_checkout: Option<String>,
    /// `git HEAD` of the containing checkout — the source the dist *should*
    /// reflect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) source_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) source_branch: Option<String>,
    /// `true` when the source checkout has uncommitted changes, so the built
    /// dist cannot be pinned to a single committed SHA.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) source_dirty: Option<bool>,
    /// The most recent commit at or before the artifact's newest mtime — the
    /// SHA the built dist most plausibly was compiled from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) built_from_sha: Option<String>,
    /// RFC3339 timestamp of the artifact's newest file (its build time).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) built_at: Option<String>,
    /// `git rev-list --count built_from_sha..HEAD` — how far the build is behind
    /// the source HEAD.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) commits_behind: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RuntimeOverlayBuildStatus {
    /// Built dist reflects the current source HEAD.
    Fresh,
    /// Built dist predates the current source HEAD (commits behind).
    Stale,
    /// The artifact directory is not inside a git checkout, so freshness cannot
    /// be verified from source provenance.
    UnknownNoGit,
    /// The artifact directory holds no files to date, so there is no build to
    /// compare.
    UnknownNoArtifacts,
    /// The artifact directory is in a git checkout with files in it, but a git
    /// probe needed for the comparison did not return a usable answer, so
    /// neither `Fresh` nor `Stale` was established.
    ///
    /// This case previously reported `Fresh`: the probe results were carried in
    /// `Option`s, every failure collapsed to `None`, and `None` fell through
    /// the `commits_behind > 0` test to the fresh branch. A broken git
    /// invocation therefore certified a stale build as current.
    UnknownProbeFailed,
}

impl RuntimeOverlayBuildStatus {
    /// This status as a shared currency verdict.
    ///
    /// The evidence is [`CurrencyEvidence::BuildTimestamp`], so a `Current`
    /// verdict here is an indication and not a proof — see this module's header
    /// for why no stronger evidence is obtainable.
    pub(super) fn currency(self) -> Currency {
        match self {
            Self::Fresh => Currency::Current,
            Self::Stale => Currency::stale(format!(
                "built artifact predates the source checkout HEAD (evidence: {})",
                CurrencyEvidence::BuildTimestamp.as_str()
            )),
            Self::UnknownNoGit => Currency::unknown(
                "artifact directory is not inside a source checkout, so it has no build provenance to compare".to_string(),
            ),
            Self::UnknownNoArtifacts => Currency::unknown(
                "artifact directory holds no files, so there is no build to compare".to_string(),
            ),
            Self::UnknownProbeFailed => Currency::unknown(
                "a source-history probe did not return a usable answer, so build currency was not established".to_string(),
            ),
        }
    }
}

impl RuntimeOverlayBuildProvenance {
    /// A provenance record with no source comparison available (e.g. for test
    /// fixtures or artifact dirs outside git). Never flagged stale.
    pub(super) fn unverifiable() -> Self {
        Self {
            status: RuntimeOverlayBuildStatus::UnknownNoGit,
            currency: RuntimeOverlayBuildStatus::UnknownNoGit.currency(),
            stale: false,
            source_checkout: None,
            source_sha: None,
            source_branch: None,
            source_dirty: None,
            built_from_sha: None,
            built_at: None,
            commits_behind: None,
        }
    }
}

/// Whether a stale runtime-overlay build should hard-fail this run instead of
/// warning, per the [`REQUIRE_FRESH_RUNTIME_OVERLAY_ENV`] opt-in.
pub(super) fn require_fresh_runtime_overlay() -> bool {
    std::env::var(REQUIRE_FRESH_RUNTIME_OVERLAY_ENV)
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

/// Assess the build freshness of a controller-local runtime-overlay artifact
/// directory. Pure apart from reading git state and filesystem mtimes, so it is
/// unit-testable against a temp git fixture.
pub(super) fn assess_runtime_overlay_build_freshness(
    artifact_dir: &Path,
) -> RuntimeOverlayBuildProvenance {
    // Resolve the containing git checkout. A non-git artifact dir has no source
    // provenance to compare, so freshness is unverifiable (not a failure).
    let Ok(checkout) = git_output(artifact_dir, &["rev-parse", "--show-toplevel"]) else {
        return RuntimeOverlayBuildProvenance::unverifiable();
    };
    let checkout_path = Path::new(&checkout);

    let source_sha = git_output(checkout_path, &["rev-parse", "HEAD"])
        .ok()
        .filter(|value| !value.is_empty());
    let source_branch = git_output(checkout_path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|value| !value.is_empty() && value != "HEAD");
    let source_dirty = git_output(checkout_path, &["status", "--porcelain=v1"])
        .ok()
        .map(|status| !status.trim().is_empty());

    let newest_mtime = newest_artifact_mtime(artifact_dir);
    let Some(newest_mtime) = newest_mtime else {
        return RuntimeOverlayBuildProvenance {
            status: RuntimeOverlayBuildStatus::UnknownNoArtifacts,
            currency: RuntimeOverlayBuildStatus::UnknownNoArtifacts.currency(),
            stale: false,
            source_checkout: Some(checkout.clone()),
            source_sha,
            source_branch,
            source_dirty,
            built_from_sha: None,
            built_at: None,
            commits_behind: None,
        };
    };
    let built_at = chrono::DateTime::<chrono::Utc>::from(newest_mtime);
    let built_at_rfc3339 = built_at.to_rfc3339();

    // The most recent commit reachable from HEAD at or before the artifact's
    // build time approximates the source the dist was compiled from.
    let before_arg = format!("--before={built_at_rfc3339}");
    let built_from_sha = git_output(
        checkout_path,
        &["rev-list", "-1", before_arg.as_str(), "HEAD"],
    )
    .ok()
    .filter(|value| !value.is_empty());

    let commits_behind = match (built_from_sha.as_deref(), source_sha.as_deref()) {
        (Some(built), Some(head)) if built == head => Some(0),
        (Some(built), Some(_)) => git_output(
            checkout_path,
            &["rev-list", "--count", &format!("{built}..HEAD")],
        )
        .ok()
        .and_then(|count| count.trim().parse::<u64>().ok()),
        _ => None,
    };

    // Fail closed. `commits_behind` is `None` for every way the comparison can
    // fail to produce an answer: HEAD unreadable, the `--before` walk empty or
    // errored, the count unparseable. Treating that absence as "not behind"
    // certified a stale build as fresh, which is the exact failure this module
    // exists to prevent.
    let (status, stale) = match commits_behind {
        Some(0) => (RuntimeOverlayBuildStatus::Fresh, false),
        Some(_) => (RuntimeOverlayBuildStatus::Stale, true),
        // Unknown is reported, not escalated: `stale` stays false so this does
        // not turn every unprobeable overlay into a warning or, under the
        // require-fresh opt-in, a hard failure.
        None => (RuntimeOverlayBuildStatus::UnknownProbeFailed, false),
    };

    RuntimeOverlayBuildProvenance {
        status,
        currency: status.currency(),
        stale,
        source_checkout: Some(checkout),
        source_sha,
        source_branch,
        source_dirty,
        built_from_sha,
        built_at: Some(built_at_rfc3339),
        commits_behind,
    }
}

/// Human-readable warning describing a stale runtime-overlay build, or `None`
/// when the build is fresh / unverifiable. Generic wording — no ecosystem or
/// product specifics.
pub(super) fn stale_runtime_overlay_warning(
    role: &str,
    local_path: &str,
    provenance: &RuntimeOverlayBuildProvenance,
) -> Option<String> {
    if !provenance.stale {
        return None;
    }
    let behind = provenance
        .commits_behind
        .map(|count| format!("{count} commit(s) behind"))
        .unwrap_or_else(|| "behind".to_string());
    let head = provenance
        .source_sha
        .as_deref()
        .map(short_sha)
        .unwrap_or_else(|| "<unknown>".to_string());
    let built = provenance
        .built_from_sha
        .as_deref()
        .map(short_sha)
        .unwrap_or_else(|| "<unknown>".to_string());
    Some(format!(
        "Lab offload: runtime overlay `{role}` build at `{local_path}` is STALE ({behind}): \
         built dist reflects source `{built}` but the source checkout is at `{head}`. \
         The offload will run the old build. Rebuild the overlay artifact (re-run its build step) \
         before retrying, or export `{REQUIRE_FRESH_RUNTIME_OVERLAY_ENV}=1` to fail instead of warn."
    ))
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// Walk the artifact directory and return the newest file mtime, skipping the
/// directory names in [`MTIME_WALK_SKIPPED_DIRECTORY_NAMES`] so their churn does
/// not mask a stale build. Returns `None` when there are no files.
fn newest_artifact_mtime(dir: &Path) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            if file_name
                .to_str()
                .is_some_and(|name| MTIME_WALK_SKIPPED_DIRECTORY_NAMES.contains(&name))
            {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(entry.path());
                continue;
            }
            if let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) {
                newest = Some(match newest {
                    Some(current_newest) if current_newest >= modified => current_newest,
                    _ => modified,
                });
            }
        }
    }
    newest
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use chrono::{Duration, Utc};

    use homeboy_core::materialization_currency::{Currency, CurrencyEvidence};

    use super::{
        assess_runtime_overlay_build_freshness, require_fresh_runtime_overlay,
        stale_runtime_overlay_warning, RuntimeOverlayBuildStatus,
        REQUIRE_FRESH_RUNTIME_OVERLAY_ENV,
    };

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

    fn git_out(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn init_repo(path: &Path) {
        git(path, &["init", "-b", "main"]);
        git(path, &["config", "user.email", "homeboy@example.test"]);
        git(path, &["config", "user.name", "Homeboy Test"]);
        git(path, &["config", "commit.gpgsign", "false"]);
    }

    /// Commit a source file at a controlled author/committer date so the
    /// dist-mtime-vs-commit-date comparison is deterministic. The freshness
    /// check resolves the build SHA from the artifact's real mtime (≈ now), so
    /// commits dated in the past land before the build and commits dated in the
    /// future land after it.
    fn commit_at(path: &Path, name: &str, contents: &str, iso_date: &str) {
        fs::write(path.join(name), contents).expect("write source");
        git(path, &["add", name]);
        let output = Command::new("git")
            .args(["commit", "-m", name])
            .current_dir(path)
            .env("GIT_AUTHOR_DATE", iso_date)
            .env("GIT_COMMITTER_DATE", iso_date)
            .output()
            .expect("commit");
        assert!(
            output.status.success(),
            "git commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_dist(repo: &Path) -> std::path::PathBuf {
        let dist = repo.join("dist");
        fs::create_dir_all(&dist).expect("dist dir");
        fs::write(dist.join("index.js"), "built").expect("write dist");
        dist
    }

    fn iso(offset_days: i64) -> String {
        (Utc::now() + Duration::days(offset_days)).to_rfc3339()
    }

    #[test]
    fn stale_when_dist_predates_newer_commits() {
        let repo = tempfile::tempdir().expect("repo");
        init_repo(repo.path());
        // Build (mtime ≈ now) was produced after this commit (dated in the past)
        // but before the later commits (dated in the future).
        commit_at(repo.path(), "a.txt", "a", &iso(-3));
        let dist = write_dist(repo.path());
        commit_at(repo.path(), "b.txt", "b", &iso(2));
        commit_at(repo.path(), "c.txt", "c", &iso(3));

        let provenance = assess_runtime_overlay_build_freshness(&dist);

        assert_eq!(provenance.status, RuntimeOverlayBuildStatus::Stale);
        assert!(provenance.stale);
        assert_eq!(provenance.commits_behind, Some(2));
        assert_eq!(
            provenance.source_sha.as_deref(),
            Some(git_out(repo.path(), &["rev-parse", "HEAD"]).as_str())
        );
        assert_ne!(provenance.built_from_sha, provenance.source_sha);

        let warning = stale_runtime_overlay_warning("cli", "/local/dist", &provenance)
            .expect("stale warning");
        assert!(warning.contains("STALE"));
        assert!(warning.contains("2 commit(s) behind"));
    }

    #[test]
    fn fresh_when_dist_built_after_head() {
        let repo = tempfile::tempdir().expect("repo");
        init_repo(repo.path());
        // All commits are dated in the past; the build (mtime ≈ now) reflects
        // the current HEAD.
        commit_at(repo.path(), "a.txt", "a", &iso(-3));
        commit_at(repo.path(), "b.txt", "b", &iso(-2));
        let dist = write_dist(repo.path());

        let provenance = assess_runtime_overlay_build_freshness(&dist);

        assert_eq!(provenance.status, RuntimeOverlayBuildStatus::Fresh);
        assert!(!provenance.stale);
        assert_eq!(provenance.commits_behind, Some(0));
        assert_eq!(provenance.built_from_sha, provenance.source_sha);
        assert!(stale_runtime_overlay_warning("cli", "/local/dist", &provenance).is_none());
    }

    #[test]
    fn unknown_when_artifact_dir_is_not_in_git() {
        let dir = tempfile::tempdir().expect("dir");
        fs::write(dir.path().join("index.js"), "built").expect("write file");

        let provenance = assess_runtime_overlay_build_freshness(dir.path());

        assert_eq!(provenance.status, RuntimeOverlayBuildStatus::UnknownNoGit);
        assert!(!provenance.stale);
        assert!(provenance.source_sha.is_none());
    }

    /// A checkout whose history cannot be read is unknown, not fresh.
    ///
    /// A repository with no commits reaches every probe this module makes:
    /// `--show-toplevel` succeeds, artifacts exist, and both `rev-parse HEAD`
    /// and the `--before` walk fail. Those failures used to collapse into
    /// `commits_behind: None`, which fell through the `> 0` test onto the fresh
    /// branch — so a build whose currency was entirely unestablished was
    /// certified as reflecting the current source.
    #[test]
    fn unknown_when_a_history_probe_cannot_answer() {
        let repo = tempfile::tempdir().expect("repo");
        init_repo(repo.path());
        let dist = write_dist(repo.path());

        let provenance = assess_runtime_overlay_build_freshness(&dist);

        assert_eq!(
            provenance.status,
            RuntimeOverlayBuildStatus::UnknownProbeFailed
        );
        assert!(provenance.currency.is_unknown());
        assert!(!provenance.currency.is_current());
        assert!(provenance.source_sha.is_none());
        assert!(provenance.built_from_sha.is_none());
        assert!(provenance.commits_behind.is_none());
        // Unknown is reported, not escalated: no warning, so the require-fresh
        // opt-in cannot turn an unprobeable overlay into a hard failure.
        assert!(!provenance.stale);
        assert!(stale_runtime_overlay_warning("cli", "/local/dist", &provenance).is_none());
    }

    #[test]
    fn every_status_carries_the_shared_currency_verdict() {
        assert_eq!(
            RuntimeOverlayBuildStatus::Fresh.currency(),
            Currency::Current
        );
        assert!(RuntimeOverlayBuildStatus::Stale.currency().is_stale());
        for status in [
            RuntimeOverlayBuildStatus::UnknownNoGit,
            RuntimeOverlayBuildStatus::UnknownNoArtifacts,
            RuntimeOverlayBuildStatus::UnknownProbeFailed,
        ] {
            assert!(
                status.currency().is_unknown(),
                "expected unknown currency for {status:?}"
            );
        }
        // This module's verdict rests on filesystem timestamps, which the
        // contract records as evidence that cannot prove content equality.
        assert!(!CurrencyEvidence::BuildTimestamp.proves_content_equality());
    }

    #[test]
    fn require_fresh_env_opt_in_parses_truthy_values() {
        temp_env_var(REQUIRE_FRESH_RUNTIME_OVERLAY_ENV, Some("1"), || {
            assert!(require_fresh_runtime_overlay());
        });
        temp_env_var(REQUIRE_FRESH_RUNTIME_OVERLAY_ENV, Some("false"), || {
            assert!(!require_fresh_runtime_overlay());
        });
        temp_env_var(REQUIRE_FRESH_RUNTIME_OVERLAY_ENV, None, || {
            assert!(!require_fresh_runtime_overlay());
        });
    }

    fn temp_env_var<F: FnOnce()>(key: &str, value: Option<&str>, run: F) {
        let previous = std::env::var(key).ok();
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        run();
        match previous {
            Some(previous) => std::env::set_var(key, previous),
            None => std::env::remove_var(key),
        }
    }
}
