//! The Lab handback of a Cook candidate as one immutable commit (#16303): the
//! runner bundles `refs/homeboy/candidates/<run_id>` (see harvest), and the
//! controller fetches and verifies it here before promotion materializes it.

use std::path::Path;
use std::process::Command;

use serde_json::json;

use crate::agent_task::AgentTaskCandidate;
use homeboy_core::{Error, Result};

/// Artifact kind of the runner-produced candidate bundle.
pub const CANDIDATE_BUNDLE_ARTIFACT_KIND: &str = "git_bundle";

/// Why a fetched candidate is not the recorded one. Never falls back to a patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateBundleError {
    /// The bundle is truncated, corrupt, or lacks the candidate objects.
    MissingObject { expected: String, observed: String },
    /// The bundle's candidate ref names a different commit.
    CommitMismatch { expected: String, observed: String },
    /// The fetched commit's tree is not the recorded candidate tree.
    TreeMismatch { expected: String, observed: String },
}

impl std::fmt::Display for CandidateBundleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (what, expected, observed) = match self {
            Self::MissingObject { expected, observed } => ("object", expected, observed),
            Self::CommitMismatch { expected, observed } => ("commit", expected, observed),
            Self::TreeMismatch { expected, observed } => ("tree", expected, observed),
        };
        write!(
            formatter,
            "candidate bundle {what} mismatch: expected {expected}, observed {observed}"
        )
    }
}

impl std::error::Error for CandidateBundleError {}

impl From<CandidateBundleError> for Error {
    fn from(error: CandidateBundleError) -> Self {
        Error::new(
            homeboy_core::ErrorCode::ValidationInvalidArgument,
            error.to_string(),
            json!({ "field": "promotion.candidate_bundle" }),
        )
        .with_source(error)
    }
}

fn git(repo: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Fetch the runner's candidate bundle into `repo` at
/// `refs/homeboy/candidates/<run_id>` and prove it is exactly `candidate`.
pub fn fetch_candidate_bundle(
    repo: &Path,
    bundle: &Path,
    run_id: &str,
    candidate: &AgentTaskCandidate,
) -> std::result::Result<String, CandidateBundleError> {
    let local_ref = format!(
        "refs/homeboy/candidates/{}",
        homeboy_core::paths::sanitize_path_segment(run_id)
    );
    let bundle = bundle.display().to_string();
    let missing = |observed: String| CandidateBundleError::MissingObject {
        expected: candidate.candidate_sha.clone(),
        observed,
    };
    git(repo, &["bundle", "verify", "--quiet", &bundle]).map_err(missing)?;
    let heads = git(repo, &["bundle", "list-heads", &bundle]).map_err(missing)?;
    let Some((_, bundle_ref)) = heads.lines().next().and_then(|line| line.split_once(' ')) else {
        return Err(missing(format!("bundle {bundle} has no candidate ref")));
    };
    let refspec = format!("+{bundle_ref}:{local_ref}");
    git(repo, &["fetch", "--no-tags", "--quiet", &bundle, &refspec]).map_err(missing)?;
    let verified = git(
        repo,
        &["rev-parse", "--verify", &format!("{local_ref}^{{commit}}")],
    )
    .map_err(missing)
    .and_then(|observed| match observed == candidate.candidate_sha {
        true => git(repo, &["rev-parse", &format!("{observed}^{{tree}}")]).map_err(missing),
        false => Err(CandidateBundleError::CommitMismatch {
            expected: candidate.candidate_sha.clone(),
            observed,
        }),
    })
    .and_then(|tree| match tree == candidate.tree_sha {
        true => Ok(local_ref.clone()),
        false => Err(CandidateBundleError::TreeMismatch {
            expected: candidate.tree_sha.clone(),
            observed: tree,
        }),
    });
    if verified.is_err() {
        // An unverified candidate must never stay pinned for a later promotion.
        let _ = git(repo, &["update-ref", "-d", &local_ref]);
    }
    verified
}

/// Make the destination worktree exactly the candidate tree, staying on its
/// branch: no patch is applied and no commit is created.
pub(crate) fn materialize_candidate_tree(
    worktree: &Path,
    candidate: &AgentTaskCandidate,
) -> Result<()> {
    let failed = |message: String| {
        Error::validation_invalid_argument(
            "promotion.candidate",
            format!(
                "cannot materialize candidate {} in {}: {message}",
                candidate.candidate_sha,
                worktree.display()
            ),
            Some(worktree.display().to_string()),
            None,
        )
    };
    let head = git(worktree, &["rev-parse", "--verify", "HEAD^{commit}"]).map_err(failed)?;
    git(
        worktree,
        &[
            "merge-base",
            "--is-ancestor",
            &head,
            &candidate.candidate_sha,
        ],
    )
    .map_err(|_| {
        failed(format!(
            "destination HEAD {head} does not precede the candidate"
        ))
    })?;
    let status = git(
        worktree,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .map_err(failed)?;
    if !status.is_empty() {
        return Err(failed(format!("destination is not clean:\n{status}")));
    }
    git(
        worktree,
        &["read-tree", "-u", "--reset", &candidate.candidate_sha],
    )
    .map_err(failed)?;
    // Leave the candidate as working-tree changes on the destination branch.
    git(worktree, &["reset", "--quiet"]).map_err(failed)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(repo: &Path, args: &[&str]) -> String {
        git(repo, args).unwrap_or_else(|error| panic!("git {args:?}: {error}"))
    }

    /// A runner clone holding a harvested candidate bundle, and a controller
    /// clone holding only the base.
    struct Fixture {
        _temp: tempfile::TempDir,
        runner: std::path::PathBuf,
        controller: std::path::PathBuf,
        bundle: std::path::PathBuf,
        candidate: AgentTaskCandidate,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let origin = temp.path().join("origin");
        std::fs::create_dir(&origin).unwrap();
        run(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("file.txt"), "base\n").unwrap();
        run(&origin, &["add", "."]);
        run(
            &origin,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "base",
            ],
        );
        let [runner, controller] = ["runner", "controller"].map(|name| {
            let path = temp.path().join(name);
            run(
                temp.path(),
                &["clone", "-q", origin.to_str().unwrap(), name],
            );
            path
        });
        let base_sha = run(&runner, &["rev-parse", "HEAD"]);
        let candidate = commit_candidate(&runner, &base_sha, "agent change\n");
        let bundle = temp.path().join("candidate.bundle");
        bundle_candidate(&runner, &bundle, &base_sha);
        Fixture {
            _temp: temp,
            runner,
            controller,
            bundle,
            candidate,
        }
    }

    fn commit_candidate(repo: &Path, base: &str, contents: &str) -> AgentTaskCandidate {
        std::fs::write(repo.join("file.txt"), contents).unwrap();
        std::fs::write(repo.join("new.txt"), "untracked\n").unwrap();
        run(repo, &["add", "--all"]);
        let tree_sha = run(repo, &["write-tree"]);
        let identity = ["-c", "user.name=Homeboy", "-c", "user.email=h@localhost"];
        let commit = [
            &identity[..],
            &["commit-tree", &tree_sha, "-p", base, "-m", "c"],
        ]
        .concat();
        let candidate_sha = run(repo, &commit);
        run(
            repo,
            &[
                "update-ref",
                "refs/homeboy/candidates/run-1",
                &candidate_sha,
            ],
        );
        AgentTaskCandidate {
            base_sha: base.to_string(),
            candidate_sha,
            tree_sha,
        }
    }

    fn bundle_candidate(repo: &Path, bundle: &Path, base: &str) {
        let bundle = bundle.display().to_string();
        let exclude = format!("^{base}");
        run(
            repo,
            &[
                "bundle",
                "create",
                "-q",
                &bundle,
                "refs/homeboy/candidates/run-1",
                &exclude,
            ],
        );
    }

    fn assert_unpinned(controller: &Path) {
        assert!(run(controller, &["for-each-ref", "refs/homeboy/candidates"]).is_empty());
    }

    #[test]
    fn controller_fetches_the_identical_candidate_commit_and_tree() {
        let fixture = fixture();
        let candidate = &fixture.candidate;
        assert!(git(
            &fixture.controller,
            &["cat-file", "-e", &candidate.candidate_sha]
        )
        .is_err());

        let local_ref =
            fetch_candidate_bundle(&fixture.controller, &fixture.bundle, "run-1", candidate)
                .unwrap();

        assert_eq!(local_ref, "refs/homeboy/candidates/run-1");
        assert_eq!(
            run(&fixture.controller, &["rev-parse", &local_ref]),
            candidate.candidate_sha
        );
        let tree = run(
            &fixture.controller,
            &["rev-parse", &format!("{local_ref}^{{tree}}")],
        );
        assert_eq!(tree, candidate.tree_sha);
    }

    #[test]
    fn tampered_bundle_fails_with_typed_commit_mismatch() {
        let fixture = fixture();
        // Replace the bundled commit under the same candidate ref.
        run(&fixture.runner, &["reset", "-q", "--hard"]);
        let base = &fixture.candidate.base_sha;
        let tampered = commit_candidate(&fixture.runner, base, "tampered\n");
        bundle_candidate(&fixture.runner, &fixture.bundle, base);

        let error = fetch_candidate_bundle(
            &fixture.controller,
            &fixture.bundle,
            "run-1",
            &fixture.candidate,
        )
        .unwrap_err();

        assert_eq!(
            error,
            CandidateBundleError::CommitMismatch {
                expected: fixture.candidate.candidate_sha.clone(),
                observed: tampered.candidate_sha,
            }
        );
        assert_unpinned(&fixture.controller);
    }

    #[test]
    fn truncated_bundle_fails_with_typed_missing_object() {
        let fixture = fixture();
        let bytes = std::fs::read(&fixture.bundle).unwrap();
        std::fs::write(&fixture.bundle, &bytes[..bytes.len() / 2]).unwrap();

        let error = fetch_candidate_bundle(
            &fixture.controller,
            &fixture.bundle,
            "run-1",
            &fixture.candidate,
        )
        .unwrap_err();

        let CandidateBundleError::MissingObject { expected, .. } = &error else {
            panic!("expected a missing-object error, got {error:?}");
        };
        assert_eq!(expected, &fixture.candidate.candidate_sha);
        let error = Error::from(error.clone());
        assert!(
            error.message.contains(&fixture.candidate.candidate_sha),
            "{}",
            error.message
        );
        assert_unpinned(&fixture.controller);
    }

    #[test]
    fn recorded_tree_mismatch_fails_with_typed_tree_mismatch() {
        let fixture = fixture();
        let mut recorded = fixture.candidate.clone();
        recorded.tree_sha = run(&fixture.controller, &["rev-parse", "HEAD^{tree}"]);

        let error =
            fetch_candidate_bundle(&fixture.controller, &fixture.bundle, "run-1", &recorded)
                .unwrap_err();

        assert_eq!(
            error,
            CandidateBundleError::TreeMismatch {
                expected: recorded.tree_sha.clone(),
                observed: fixture.candidate.tree_sha.clone(),
            }
        );
        let message = Error::from(error).message;
        assert!(
            message.contains(&recorded.tree_sha) && message.contains(&fixture.candidate.tree_sha)
        );
        assert_unpinned(&fixture.controller);
    }
}
