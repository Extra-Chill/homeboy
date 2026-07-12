use std::path::{Path, PathBuf};

use crate::core::component::Component;
use crate::core::error::{Error, Result};
use crate::core::git;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExactRefIdentity {
    pub requested_ref: String,
    pub resolved_sha: String,
    pub source: String,
}

pub(super) struct ExactRefCheckout {
    pub component: Component,
    pub identity: ExactRefIdentity,
    source_root: PathBuf,
    worktree_path: PathBuf,
}

pub(super) fn resolve_exact_ref(
    component: &Component,
    requested_ref: &str,
) -> Result<ExactRefIdentity> {
    validate_component_source(component)?;
    let source_root = source_root(component)?;
    let resolved_sha = resolve_commit(&source_root, requested_ref, &component.id)?;
    Ok(ExactRefIdentity {
        requested_ref: requested_ref.to_string(),
        resolved_sha,
        source: source_root.to_string_lossy().to_string(),
    })
}

impl ExactRefCheckout {
    pub(super) fn materialize(component: &Component, requested_ref: &str) -> Result<Self> {
        let identity = resolve_exact_ref(component, requested_ref)?;
        let source_root = PathBuf::from(&identity.source);
        let component_prefix = git::get_component_path_prefix(&component.local_path);
        let parent = std::env::temp_dir().join("homeboy-deploy-ref");
        std::fs::create_dir_all(&parent).map_err(|err| {
            Error::internal_io(
                format!("Failed to create exact-ref deploy temp directory: {err}"),
                Some("deploy.ref.temp".to_string()),
            )
        })?;
        let worktree_path = parent.join(uuid::Uuid::new_v4().to_string());
        let worktree_arg = worktree_path.to_string_lossy().to_string();
        git::run_git(
            &source_root,
            &[
                "worktree",
                "add",
                "--detach",
                &worktree_arg,
                &identity.resolved_sha,
            ],
            "materialize exact deploy ref",
        )?;

        let materialized_path = component_prefix
            .as_deref()
            .map(|prefix| worktree_path.join(prefix))
            .unwrap_or_else(|| worktree_path.clone());
        if !materialized_path.exists() {
            let mut checkout = Self {
                component: component.clone(),
                identity,
                source_root,
                worktree_path,
            };
            checkout.cleanup();
            return Err(Error::validation_invalid_argument(
                "ref",
                format!(
                    "Resolved ref does not contain the declared component path for '{}'",
                    component.id
                ),
                None,
                None,
            ));
        }

        let mut materialized = component.clone();
        materialized.local_path = materialized_path.to_string_lossy().to_string();
        Ok(Self {
            component: materialized,
            identity,
            source_root,
            worktree_path,
        })
    }

    fn cleanup(&mut self) {
        let worktree = self.worktree_path.to_string_lossy().to_string();
        let _ = git::run_git(
            &self.source_root,
            &["worktree", "remove", "--force", &worktree],
            "remove exact deploy ref worktree",
        );
        let _ = std::fs::remove_dir_all(&self.worktree_path);
    }
}

impl Drop for ExactRefCheckout {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn validate_component_source(component: &Component) -> Result<()> {
    if component.is_file_component() {
        return Err(unsupported_source(
            component,
            "file deploy sources are not Git trees",
        ));
    }
    if component.deploy_config().is_git_deploy() {
        return Err(unsupported_source(
            component,
            "deploy_strategy 'git' updates a remote checkout instead of packaging the selected tree",
        ));
    }
    Ok(())
}

fn unsupported_source(component: &Component, reason: &str) -> Error {
    Error::validation_invalid_argument(
        "ref",
        format!(
            "Component '{}' does not support --ref: {reason}",
            component.id
        ),
        None,
        None,
    )
}

fn source_root(component: &Component) -> Result<PathBuf> {
    git::get_git_root(&component.local_path)
        .map(PathBuf::from)
        .map_err(|_| {
            Error::validation_invalid_argument(
                "ref",
                format!(
                    "Cannot use --ref for component '{}': declared source '{}' is not a Git repository",
                    component.id, component.local_path
                ),
                None,
                None,
            )
        })
}

fn resolve_commit(source_root: &Path, requested_ref: &str, component_id: &str) -> Result<String> {
    if requested_ref.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "ref",
            "--ref must not be empty",
            None,
            None,
        ));
    }
    let commit_ref = format!("{requested_ref}^{{commit}}");
    git::run_git(
        source_root,
        &["rev-parse", "--verify", &commit_ref],
        "resolve exact deploy ref",
    )
    .map(|sha| sha.trim().to_string())
    .map_err(|_| {
        Error::validation_invalid_argument(
            "ref",
            format!(
                "Cannot resolve --ref '{}' to an unambiguous commit in declared repository '{}' for component '{}'",
                requested_ref,
                source_root.display(),
                component_id
            ),
            None,
            None,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn exact_sha_branch_and_tag_resolve_to_immutable_commit() {
        let repo = fixture_repo();
        let component = fixture_component(repo.path());
        let sha = git_output(repo.path(), &["rev-parse", "HEAD"]);

        assert_eq!(
            resolve_exact_ref(&component, &sha)
                .expect("exact SHA")
                .resolved_sha,
            sha
        );
        assert_eq!(
            resolve_exact_ref(&component, "accepted")
                .expect("branch")
                .resolved_sha,
            sha
        );
        assert_eq!(
            resolve_exact_ref(&component, "v1.0.0")
                .expect("tag")
                .resolved_sha,
            sha
        );
    }

    #[test]
    fn materialized_ref_is_independent_of_stale_checkout_head_and_does_not_mutate_it() {
        let repo = fixture_repo();
        let component = fixture_component(repo.path());
        let accepted_sha = git_output(repo.path(), &["rev-parse", "accepted"]);
        std::fs::write(repo.path().join("payload.txt"), "newer checkout\n").expect("write");
        git(repo.path(), &["add", "payload.txt"]);
        commit(repo.path(), "new checkout head");
        let checkout_head = git_output(repo.path(), &["rev-parse", "HEAD"]);
        assert_ne!(accepted_sha, checkout_head);

        let worktree_path = {
            let checkout = ExactRefCheckout::materialize(&component, "accepted")
                .expect("materialize accepted branch");
            assert_eq!(checkout.identity.resolved_sha, accepted_sha);
            assert_eq!(
                std::fs::read_to_string(
                    Path::new(&checkout.component.local_path).join("payload.txt")
                )
                .expect("materialized payload"),
                "accepted\n"
            );
            PathBuf::from(&checkout.component.local_path)
        };

        assert!(
            !worktree_path.exists(),
            "temporary worktree should be removed"
        );
        assert_eq!(
            git_output(repo.path(), &["rev-parse", "HEAD"]),
            checkout_head
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("payload.txt")).expect("checkout payload"),
            "newer checkout\n"
        );
    }

    #[test]
    fn dry_resolution_is_read_only_and_unresolvable_refs_fail_clearly() {
        let repo = fixture_repo();
        let component = fixture_component(repo.path());
        let before = git_output(repo.path(), &["status", "--porcelain=v1"]);
        let identity = resolve_exact_ref(&component, "accepted").expect("resolve branch");

        assert_eq!(identity.requested_ref, "accepted");
        assert_eq!(
            Path::new(&identity.source)
                .canonicalize()
                .expect("canonical source"),
            repo.path().canonicalize().expect("canonical repo")
        );
        assert_eq!(
            git_output(repo.path(), &["status", "--porcelain=v1"]),
            before
        );
        let err =
            resolve_exact_ref(&component, "missing-ref").expect_err("missing ref should fail");
        assert!(err.message.contains("Cannot resolve --ref 'missing-ref'"));
        assert!(err.message.contains(&identity.source));
    }

    #[test]
    fn exact_ref_rejects_incompatible_deploy_source_types() {
        let repo = fixture_repo();
        let mut component = fixture_component(repo.path());
        component.deploy_strategy = Some("git".to_string());
        let git_error = resolve_exact_ref(&component, "accepted")
            .expect_err("git deploy strategy should be rejected");
        assert!(git_error
            .message
            .contains("deploy_strategy 'git' updates a remote checkout"));

        component.deploy_strategy = Some("file".to_string());
        component.local_path = repo
            .path()
            .join("payload.txt")
            .to_string_lossy()
            .to_string();
        let file_error = resolve_exact_ref(&component, "accepted")
            .expect_err("file deploy strategy should be rejected");
        assert!(file_error.message.contains("file deploy sources"));
    }

    fn fixture_repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("repo");
        git(repo.path(), &["init", "-q"]);
        git(repo.path(), &["config", "user.name", "Homeboy Test"]);
        git(
            repo.path(),
            &["config", "user.email", "homeboy@example.test"],
        );
        std::fs::write(repo.path().join("payload.txt"), "accepted\n").expect("payload");
        git(repo.path(), &["add", "payload.txt"]);
        commit(repo.path(), "accepted");
        git(repo.path(), &["branch", "accepted"]);
        git(repo.path(), &["tag", "v1.0.0"]);
        repo
    }

    fn fixture_component(path: &Path) -> Component {
        Component {
            id: "fixture".to_string(),
            local_path: path.to_string_lossy().to_string(),
            build_artifact: Some("build/fixture.zip".to_string()),
            ..Component::default()
        }
    }

    fn commit(path: &Path, message: &str) {
        git(path, &["commit", "-q", "-m", message]);
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_output(path: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("utf8")
            .trim()
            .to_string()
    }
}
