use std::collections::HashMap;

use crate::release::version;
use homeboy_core::component::Component;
use homeboy_core::error::{Error, Result};
use homeboy_core::git;

use super::super::execution::{release_artifact_plan, ReleaseArtifactPlan};
use super::super::generated_artifacts::uncommitted_file_report_excluding_known_generated;
use super::super::types::{compare_deployed_versions, ComponentStatus, DeployConfig};

/// Return the components whose payload depends on local source or a local artifact.
/// Downloaded release assets are immutable remote inputs and intentionally bypass
/// local-checkout safety guards.
pub(super) fn local_build_components(
    components: &[Component],
    config: &DeployConfig,
) -> Vec<Component> {
    components
        .iter()
        .filter(|component| {
            let is_git_deploy = component.deploy_strategy.as_deref() == Some("git");
            let is_file_deploy = component.deploy_strategy.as_deref() == Some("file");
            matches!(
                release_artifact_plan(component, config, is_git_deploy, is_file_deploy),
                ReleaseArtifactPlan::LocalBuild { .. }
            )
        })
        .cloned()
        .collect()
}

/// Refuse known-stale local source checkouts unless the operator explicitly
/// accepts that source identity. A failed freshness probe is not treated as stale.
pub(super) fn guard_local_build_source_freshness(
    components: &[Component],
    config: &DeployConfig,
) -> Result<()> {
    if config.allow_stale_source {
        return Ok(());
    }

    let stale = components
        .iter()
        .filter_map(|component| {
            git::fetch_and_get_behind_count(&component.local_path)
                .ok()
                .flatten()
                .map(|behind| format!("{} ({behind} commit(s) behind upstream)", component.id))
        })
        .collect::<Vec<_>>();

    if stale.is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "source",
        format!(
            "Refusing local-build deploy from stale source checkout(s): {}",
            stale.join(", ")
        ),
        None,
        Some(vec![
            "Refresh the source checkout, then deploy again".to_string(),
            "Use --allow-stale-source only when deploying the known stale checkout is intentional"
                .to_string(),
        ]),
    ))
}

/// Refuse only proven semantic-version downgrades. Missing and non-semantic
/// version values remain observable in check/dry-run output but cannot cause a
/// false deployment refusal.
pub(super) fn guard_local_build_downgrades(
    components: &[Component],
    local_versions: &HashMap<String, String>,
    remote_versions: &HashMap<String, String>,
    config: &DeployConfig,
) -> Result<()> {
    if config.allow_downgrade {
        return Ok(());
    }

    let downgrades = components
        .iter()
        .filter_map(|component| {
            let local = local_versions.get(&component.id)?;
            let remote = remote_versions.get(&component.id)?;
            (compare_deployed_versions(Some(local), Some(remote)) == ComponentStatus::BehindRemote)
                .then(|| format!("{} (remote {remote} > local {local})", component.id))
        })
        .collect::<Vec<_>>();

    if downgrades.is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "version",
        format!(
            "Refusing local-build version downgrade: {}",
            downgrades.join(", ")
        ),
        None,
        Some(vec![
            "Refresh the local source or select a version newer than the deployed remote version"
                .to_string(),
            "Use --allow-downgrade only when replacing a newer deployed version is intentional"
                .to_string(),
        ]),
    ))
}

/// Warn when `--head` would deploy from a non-default branch.
///
/// Detects the current branch for each component and compares it against the
/// default branch (via [`git::default_branch_name`], falling back to "main").
/// If a component is on a feature branch, this is likely
/// unintentional — the user probably meant to deploy the default branch.
///
/// With `--force`, this emits a log warning but proceeds. Without `--force`,
/// it returns an error so the user can switch branches or confirm intent.
pub(super) fn warn_non_default_branch(
    components: &[Component],
    config: &DeployConfig,
) -> Result<()> {
    for component in components {
        if component.is_file_component() {
            continue;
        }

        let path = &component.local_path;

        // Get current branch
        let current_branch = match homeboy_core::engine::command::run_in_optional(
            path,
            "git",
            &["rev-parse", "--abbrev-ref", "HEAD"],
        ) {
            Some(branch) if branch != "HEAD" => branch, // "HEAD" means detached
            _ => continue,                              // detached or error — skip
        };

        // Detect default branch from the resolved remote HEAD symref, fallback to "main"
        let default_branch = git::default_branch_name(std::path::Path::new(path))
            .unwrap_or_else(|| "main".to_string());

        if current_branch != default_branch {
            let message = format!(
                "Component '{}' is on branch '{}', not '{}' (default)",
                component.id, current_branch, default_branch
            );

            if config.force {
                homeboy_core::log_status!("deploy", "Warning: {}", message);
            } else {
                return Err(Error::validation_invalid_argument(
                    "head",
                    message,
                    None,
                    Some(vec![
                        format!(
                            "Switch to the default branch: git -C {} checkout {}",
                            component.local_path, default_branch
                        ),
                        "Use --force to deploy from the current branch anyway".to_string(),
                    ]),
                ));
            }
        }
    }
    Ok(())
}

/// Read a repository's current HEAD commit, or `None` if `dir` is not a git
/// checkout or the command fails.
fn head_commit(dir: &str) -> Option<String> {
    homeboy_core::engine::command::run_in_optional(dir, "git", &["rev-parse", "HEAD"])
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The absolute `--git-common-dir` for `dir`, which is shared across all linked
/// worktrees of a repository. Two checkouts with the same common dir are the
/// same repository (a primary and its worktrees); differing common dirs are
/// unrelated repositories.
fn git_common_dir(dir: &str) -> Option<std::path::PathBuf> {
    let raw = homeboy_core::engine::command::run_in_optional(
        dir,
        "git",
        &["rev-parse", "--git-common-dir"],
    )?;
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let path = std::path::PathBuf::from(raw);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::path::Path::new(dir).join(path)
    };
    absolute.canonicalize().ok()
}

/// Fail closed when a `--head` deploy would build from the registered checkout
/// while the operator is standing in a *different* checkout of the same
/// repository at a *different* commit (#7599).
///
/// Deploy resolves each component to its registered `local_path` (the primary),
/// and every `--head` provenance/build read (`built_from_commit`, the deployed
/// ref) is taken from there. When `homeboy deploy --head` is invoked from a
/// worktree that has been advanced past the primary, the deploy silently ships
/// the primary's older SHA while reporting `main (HEAD)` — the wrong artifact
/// with misleading provenance. Rather than silently choose a checkout, refuse
/// and tell the operator exactly how to reconcile.
pub(super) fn guard_head_matches_invocation_checkout(
    components: &[Component],
    config: &DeployConfig,
) -> Result<()> {
    if config.force {
        return Ok(());
    }
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(());
    };
    let cwd = cwd.to_string_lossy().to_string();
    // If the invocation directory is not itself a git checkout there is nothing
    // to reconcile against — the registered checkout is the only source.
    let Some(cwd_common_dir) = git_common_dir(&cwd) else {
        return Ok(());
    };
    let Some(cwd_head) = head_commit(&cwd) else {
        return Ok(());
    };

    for component in components {
        if component.is_file_component() {
            continue;
        }
        let registered_path = &component.local_path;
        // Only compare within the same repository: the invocation checkout must
        // share the registered checkout's git-common-dir (i.e. be a worktree or
        // sibling checkout of the same repo), otherwise the operator is simply
        // standing somewhere unrelated and the registered path is authoritative.
        let Some(registered_common_dir) = git_common_dir(registered_path) else {
            continue;
        };
        if registered_common_dir != cwd_common_dir {
            continue;
        }
        let Some(registered_head) = head_commit(registered_path) else {
            continue;
        };
        if registered_head == cwd_head {
            continue;
        }

        return Err(Error::validation_invalid_argument(
            "head",
            format!(
                "Refusing --head deploy of '{}': the current directory is a different checkout of the same repository at commit {} \
                 than the registered source at {} (commit {}). --head would build and report the registered checkout's commit, not the one you are standing in.",
                component.id,
                short_sha(&cwd_head),
                registered_path,
                short_sha(&registered_head),
            ),
            Some(component.id.clone()),
            Some(vec![
                format!(
                    "Deploy from the registered checkout: git -C {} checkout <ref> (or point it at this worktree with `homeboy component set {} --local-path {}`).",
                    registered_path, component.id, cwd
                ),
                format!(
                    "Or fast-forward the registered checkout to this commit: git -C {} merge --ff-only {}.",
                    registered_path, short_sha(&cwd_head)
                ),
                "Use --force to deploy the registered checkout's commit anyway.".to_string(),
            ]),
        ));
    }
    Ok(())
}

fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

pub(super) fn check_uncommitted_changes(components: &[Component]) -> Result<()> {
    // Partition components into "non-git local_path" vs "dirty git repo" so we can
    // emit the right diagnostic. Conflating the two leaves users chasing a
    // nonexistent uncommitted-changes problem when the real issue is that
    // local_path doesn't point at a git checkout. (#1141)
    let mut non_git: Vec<&Component> = Vec::new();
    let mut dirty: Vec<DirtyComponent> = Vec::new();

    for component in components {
        if component.is_file_component() {
            continue;
        }
        if !git::is_git_repo(&component.local_path) {
            non_git.push(component);
            continue;
        }
        match uncommitted_file_report_excluding_known_generated(component) {
            Ok(report) if report.unexpected.is_empty() => {}
            Ok(report) => dirty.push(DirtyComponent {
                id: component.id.clone(),
                unexpected_paths: report.unexpected,
                known_generated_paths: report.known_generated,
            }),
            Err(_) => dirty.push(DirtyComponent {
                id: component.id.clone(),
                unexpected_paths: Vec::new(),
                known_generated_paths: Vec::new(),
            }),
        }
    }

    if !non_git.is_empty() {
        let ids: Vec<&str> = non_git.iter().map(|c| c.id.as_str()).collect();
        let mut hints: Vec<String> = non_git
            .iter()
            .map(|c| {
                format!(
                    "Repoint '{}' at a git checkout: homeboy component set {} --local-path <path-to-git-workspace>",
                    c.id, c.id
                )
            })
            .collect();
        hints.push(
            "Initialize a git repo at the existing local_path if the contents are the source of truth: git -C <local_path> init && git add . && git commit -m 'initial'"
                .to_string(),
        );
        hints.push("Or deploy with --force to bypass the git-clean check".to_string());
        let paths: Vec<String> = non_git
            .iter()
            .map(|c| format!("{} ({})", c.id, c.local_path))
            .collect();
        return Err(Error::validation_invalid_argument(
            "components",
            format!(
                "local_path is not a git repository for: {}. The uncommitted-changes check requires a git checkout.",
                paths.join(", ")
            ),
            Some(ids.join(",")),
            Some(hints),
        ));
    }

    if !dirty.is_empty() {
        let dirty_ids: Vec<&str> = dirty.iter().map(|row| row.id.as_str()).collect();
        return Err(Error::validation_invalid_argument(
            "components",
            format!(
                "Components have uncommitted changes: {}. {}",
                dirty_ids.join(", "),
                dirty_worktree_path_summary(&dirty)
            ),
            None,
            Some(vec![
                "Commit your changes before deploying to ensure deployed code is tracked"
                    .to_string(),
                "Known generated artifacts are ignored by the deploy dirty gate; remove them with `homeboy cleanup --apply` if you want a clean worktree"
                    .to_string(),
                "Use --force to deploy anyway".to_string(),
            ]),
        ));
    }
    Ok(())
}

struct DirtyComponent {
    id: String,
    unexpected_paths: Vec<String>,
    known_generated_paths: Vec<String>,
}

fn dirty_worktree_path_summary(dirty: &[DirtyComponent]) -> String {
    dirty
        .iter()
        .map(|row| {
            let unexpected = if row.unexpected_paths.is_empty() {
                "<unable to read git status>".to_string()
            } else {
                row.unexpected_paths.join(", ")
            };
            let known_generated = if row.known_generated_paths.is_empty() {
                String::new()
            } else {
                format!(
                    "; known generated artifacts ignored: {}",
                    row.known_generated_paths.join(", ")
                )
            };
            format!(
                "{} unexpected paths: {}{}",
                row.id, unexpected, known_generated
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;
    use std::process::Command;

    use super::{
        guard_head_matches_invocation_checkout, guard_local_build_downgrades,
        guard_local_build_source_freshness, local_build_components,
    };
    use crate::deploy::DeployConfig;
    use homeboy_core::component::Component;

    fn config() -> DeployConfig {
        DeployConfig {
            component_ids: vec![],
            all: false,
            outdated: false,
            behind_upstream: false,
            dry_run: false,
            check: false,
            force: false,
            skip_build: false,
            keep_deps: false,
            skip_deps_hydration: false,
            expected_version: None,
            no_pull: true,
            allow_stale_source: false,
            allow_downgrade: false,
            head: false,
            requested_ref: None,
            requested_refs: Default::default(),
            resolved_refs: Default::default(),
            preflighted_source_paths: Default::default(),
            preflighted_component_identities: Default::default(),
            tagged: false,
            prepared_artifact: None,
            resume_run_id: None,
        }
    }

    fn component(path: &Path) -> Component {
        Component {
            id: "example".to_string(),
            local_path: path.to_string_lossy().to_string(),
            ..Component::default()
        }
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn stale_local_source_refuses_until_explicitly_allowed() {
        let temp = tempfile::tempdir().expect("temp dir");
        let remote = temp.path().join("remote.git");
        let source = temp.path().join("source");
        let updater = temp.path().join("updater");
        std::fs::create_dir(&source).expect("source dir");
        git(
            temp.path(),
            &["init", "--bare", remote.to_str().expect("remote path")],
        );
        git(&source, &["init"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        git(&source, &["config", "user.name", "Test"]);
        std::fs::write(source.join("version.txt"), "1.0.0").expect("version");
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "initial"]);
        git(&source, &["branch", "-M", "main"]);
        git(
            &source,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        git(&source, &["push", "-u", "origin", "main"]);
        git(
            temp.path(),
            &[
                "clone",
                "-b",
                "main",
                remote.to_str().expect("remote path"),
                updater.to_str().expect("updater path"),
            ],
        );
        git(&updater, &["config", "user.email", "test@example.com"]);
        git(&updater, &["config", "user.name", "Test"]);
        std::fs::write(updater.join("version.txt"), "1.1.0").expect("updated version");
        git(&updater, &["add", "."]);
        git(&updater, &["commit", "-m", "update"]);
        git(&updater, &["push"]);

        let component = component(&source);
        let error = guard_local_build_source_freshness(&[component.clone()], &config())
            .expect_err("stale source must refuse");
        assert!(error.message.contains("1 commit(s) behind upstream"));
        assert!(error.details.to_string().contains("--allow-stale-source"));

        let mut allowed = config();
        allowed.allow_stale_source = true;
        guard_local_build_source_freshness(&[component], &allowed)
            .expect("explicit stale-source override must proceed");
    }

    #[test]
    fn semantic_downgrade_refuses_until_explicitly_allowed() {
        let component = component(Path::new("."));
        let local = HashMap::from([("example".to_string(), "1.2.3".to_string())]);
        let remote = HashMap::from([("example".to_string(), "1.3.0".to_string())]);
        let error = guard_local_build_downgrades(&[component.clone()], &local, &remote, &config())
            .expect_err("remote-newer version must refuse");
        assert!(error.message.contains("remote 1.3.0 > local 1.2.3"));
        assert!(error.details.to_string().contains("--allow-downgrade"));

        let mut allowed = config();
        allowed.allow_downgrade = true;
        guard_local_build_downgrades(&[component], &local, &remote, &allowed)
            .expect("explicit downgrade override must proceed");
    }

    #[test]
    fn equal_or_newer_local_semantic_versions_are_allowed() {
        let component = component(Path::new("."));
        for (local_version, remote_version) in [("1.3.0", "1.3.0"), ("1.4.0", "1.3.0")] {
            let local = HashMap::from([("example".to_string(), local_version.to_string())]);
            let remote = HashMap::from([("example".to_string(), remote_version.to_string())]);
            guard_local_build_downgrades(&[component.clone()], &local, &remote, &config())
                .expect("equal or newer local version must proceed");
        }
    }

    #[test]
    fn unavailable_or_invalid_versions_do_not_create_false_downgrade_refusals() {
        let component = component(Path::new("."));
        for (local, remote) in [
            (
                HashMap::new(),
                HashMap::from([("example".to_string(), "1.3.0".to_string())]),
            ),
            (
                HashMap::from([("example".to_string(), "dev".to_string())]),
                HashMap::from([("example".to_string(), "1.3.0".to_string())]),
            ),
        ] {
            guard_local_build_downgrades(&[component.clone()], &local, &remote, &config())
                .expect("unavailable or invalid versions are not proven downgrades");
        }
    }

    #[test]
    fn immutable_release_asset_bypasses_local_source_guards() {
        let component = Component {
            id: "example".to_string(),
            local_path: "/not/a/checkout".to_string(),
            remote_url: Some("https://github.com/example/example".to_string()),
            build_artifact: Some("example.zip".to_string()),
            ..Component::default()
        };
        let mut config = config();
        config.expected_version = Some("1.2.3".to_string());

        assert!(local_build_components(&[component], &config).is_empty());
    }

    // Serialize the #7599 tests: they mutate the process CWD, which is global.
    fn cwd_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn with_cwd<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = cwd_lock().lock().unwrap_or_else(|p| p.into_inner());
        let previous = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(dir).expect("set cwd");
        let result = f();
        std::env::set_current_dir(previous).expect("restore cwd");
        result
    }

    fn init_repo_with_commit(path: &Path) {
        std::fs::create_dir_all(path).expect("repo dir");
        git(path, &["init"]);
        git(path, &["config", "user.email", "test@example.com"]);
        git(path, &["config", "user.name", "Test"]);
        std::fs::write(path.join("file.txt"), "a").expect("seed file");
        git(path, &["add", "."]);
        git(path, &["commit", "-m", "initial"]);
        git(path, &["branch", "-M", "main"]);
    }

    #[test]
    fn head_deploy_fails_closed_when_invocation_worktree_advanced_past_registered_checkout() {
        // #7599: registered checkout (primary) is behind the worktree the
        // operator is standing in. --head would build/report the primary's SHA,
        // so refuse with an actionable message rather than ship the wrong commit.
        let temp = tempfile::tempdir().expect("temp dir");
        let primary = temp.path().join("primary");
        init_repo_with_commit(&primary);
        let worktree = temp.path().join("component@feature");
        git(
            &primary,
            &["worktree", "add", worktree.to_str().expect("worktree path")],
        );
        // Advance the worktree past the primary.
        std::fs::write(worktree.join("file.txt"), "b").expect("edit");
        git(&worktree, &["add", "."]);
        git(&worktree, &["commit", "-m", "advance worktree"]);

        let mut cfg = config();
        cfg.head = true;

        let error = with_cwd(&worktree, || {
            guard_head_matches_invocation_checkout(&[component(&primary)], &cfg)
                .expect_err("advanced invocation worktree must fail closed")
        });
        assert_eq!(error.details["field"], "head");
        assert!(error.to_string().contains("different checkout"));
    }

    #[test]
    fn head_deploy_passes_when_invocation_checkout_matches_registered_head() {
        // Same repo, same HEAD (e.g. invoked from the registered checkout, or a
        // worktree that has not advanced): nothing to reconcile.
        let temp = tempfile::tempdir().expect("temp dir");
        let primary = temp.path().join("primary");
        init_repo_with_commit(&primary);
        let worktree = temp.path().join("component@even");
        git(
            &primary,
            &["worktree", "add", worktree.to_str().expect("worktree path")],
        );

        let mut cfg = config();
        cfg.head = true;

        with_cwd(&worktree, || {
            guard_head_matches_invocation_checkout(&[component(&primary)], &cfg)
                .expect("matching HEAD passes the guard")
        });
    }

    #[test]
    fn head_deploy_ignores_unrelated_invocation_checkout() {
        // The operator is standing in a *different* repository (different
        // git-common-dir). The registered checkout is authoritative; do not
        // false-positive on an unrelated advanced repo.
        let temp = tempfile::tempdir().expect("temp dir");
        let primary = temp.path().join("primary");
        init_repo_with_commit(&primary);
        let unrelated = temp.path().join("unrelated");
        init_repo_with_commit(&unrelated);
        std::fs::write(unrelated.join("file.txt"), "z").expect("edit");
        git(&unrelated, &["add", "."]);
        git(&unrelated, &["commit", "-m", "unrelated advance"]);

        let mut cfg = config();
        cfg.head = true;

        with_cwd(&unrelated, || {
            guard_head_matches_invocation_checkout(&[component(&primary)], &cfg)
                .expect("an unrelated repo must not trip the same-repo guard")
        });
    }

    #[test]
    fn head_deploy_force_bypasses_the_invocation_checkout_guard() {
        let temp = tempfile::tempdir().expect("temp dir");
        let primary = temp.path().join("primary");
        init_repo_with_commit(&primary);
        let worktree = temp.path().join("component@forced");
        git(
            &primary,
            &["worktree", "add", worktree.to_str().expect("worktree path")],
        );
        std::fs::write(worktree.join("file.txt"), "b").expect("edit");
        git(&worktree, &["add", "."]);
        git(&worktree, &["commit", "-m", "advance worktree"]);

        let mut cfg = config();
        cfg.head = true;
        cfg.force = true;

        with_cwd(&worktree, || {
            guard_head_matches_invocation_checkout(&[component(&primary)], &cfg)
                .expect("--force bypasses the guard")
        });
    }
}

/// Fetch and pull latest changes for each component before deploying.
///
/// Prevents deploying stale code when the local clone is behind remote.
/// Runs `git fetch` + `git pull` for each component that has an upstream.
/// Aborts if pull fails (e.g., merge conflicts).
pub(super) fn sync_components(components: &[Component]) -> Result<()> {
    for component in components {
        // File components are not git repos — skip sync
        if component.is_file_component() {
            continue;
        }

        let version_before_pull = version::get_component_version(component);

        // Check if behind remote
        match git::fetch_and_get_behind_count(&component.local_path) {
            Ok(Some(behind)) => {
                homeboy_core::log_status!(
                    "deploy",
                    "'{}' is {} commit(s) behind remote — pulling...",
                    component.id,
                    behind
                );
                let pull_result = git::pull(Some(&component.id))?;
                if !pull_result.success {
                    return Err(Error::git_command_failed(format!(
                        "Failed to pull '{}': {}",
                        component.id,
                        pull_result.stderr.lines().next().unwrap_or("unknown error")
                    )));
                }
                if let Some(message) = auto_pull_version_drift_message(
                    component,
                    version_before_pull.as_deref(),
                    version::get_component_version(component).as_deref(),
                ) {
                    homeboy_core::log_status!("deploy", "{}", message);
                }
                homeboy_core::log_status!("deploy", "'{}' is now up to date", component.id);
            }
            Ok(None) => {
                // Not behind or no upstream — nothing to do
            }
            Err(_) => {
                // git fetch failed — warn but don't block (might be offline)
                homeboy_core::log_status!(
                    "deploy",
                    "Warning: could not check remote status for '{}' — deploying local state",
                    component.id
                );
            }
        }
    }
    Ok(())
}

pub(super) fn auto_pull_version_drift_message(
    component: &Component,
    before: Option<&str>,
    after: Option<&str>,
) -> Option<String> {
    if before == after {
        return None;
    }

    Some(format!(
        "Warning: auto-pull changed '{}' deploy version from {} to {}",
        component.id,
        before.unwrap_or("unknown"),
        after.unwrap_or("unknown")
    ))
}

/// Check for unreleased commits ahead of the latest tag.
///
/// Checks each component for commits between the latest tag and HEAD.
/// When found and `--force` is not set, returns an error to prevent
/// silently deploying stale code. Use `deploy --head` to deploy
/// unreleased commits, or `homeboy release` to tag them first.
pub(super) fn check_unreleased_commits(
    components: &[Component],
    config: &DeployConfig,
) -> homeboy_core::Result<()> {
    let mut gaps = Vec::new();

    for component in components {
        if let Some(gap) = super::super::provenance::detect_tag_gap(component) {
            super::super::provenance::warn_tag_gap(&component.id, &gap, "deploy");
            gaps.push((component.id.clone(), gap));
        }
    }

    if gaps.is_empty() {
        return Ok(());
    }

    if config.tagged {
        homeboy_core::log_status!(
            "deploy",
            "Deploying from tagged releases (--tagged). Use `deploy --head` to include unreleased commits, or `homeboy release` to tag them."
        );
        return Ok(());
    }

    if config.force {
        homeboy_core::log_status!(
            "deploy",
            "Deploying from tagged releases (--force). Use `deploy --head` to include unreleased commits, or `homeboy release` to tag them."
        );
        return Ok(());
    }

    let component_list: Vec<String> = gaps
        .iter()
        .map(|(id, gap)| format!("{} ({} commits ahead of {})", id, gap.ahead, gap.tag))
        .collect();

    Err(homeboy_core::Error::validation_invalid_argument(
        "deploy",
        format!(
            "Refusing to deploy: HEAD has unreleased commits for: {}",
            component_list.join(", ")
        ),
        None,
        Some(vec![
            "Run `homeboy release` to tag the commits first".to_string(),
            "Use `deploy --head` to deploy unreleased commits directly".to_string(),
            "Use `deploy --force` to deploy the stale tag anyway".to_string(),
        ]),
    ))
}

/// Verify that component versions match the expected version.
///
/// When `--version` is used, ensures the local version of each component
/// matches the asserted version. This catches cases where the local copy
/// has a different version than what was just released.
pub(super) fn verify_expected_version(components: &[Component], expected: &str) -> Result<()> {
    let mut mismatches = Vec::new();

    for component in components {
        match version::get_component_version(component) {
            Some(local_version) if local_version == expected => {}
            Some(local_version) => mismatches.push(format!(
                "'{}': local version is {} (expected {})",
                component.id, local_version, expected
            )),
            None => mismatches.push(format!(
                "'{}': local version could not be read (expected {})",
                component.id, expected
            )),
        }
    }

    if !mismatches.is_empty() {
        return Err(Error::validation_invalid_argument(
            "version",
            format!("Version mismatch: {}", mismatches.join("; ")),
            None,
            Some(vec![
                "Pull latest changes: git pull".to_string(),
                "Or remove --version to deploy the current local version".to_string(),
            ]),
        ));
    }
    Ok(())
}
