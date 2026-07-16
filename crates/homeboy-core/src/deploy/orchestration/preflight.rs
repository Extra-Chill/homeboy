use std::collections::HashMap;

use crate::component::Component;
use crate::error::{Error, Result};
use crate::git;
use crate::release::version;

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
        let current_branch = match crate::engine::command::run_in_optional(
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
                log_status!("deploy", "Warning: {}", message);
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
        guard_local_build_downgrades, guard_local_build_source_freshness, local_build_components,
    };
    use crate::component::Component;
    use crate::deploy::DeployConfig;

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
                log_status!(
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
                    log_status!("deploy", "{}", message);
                }
                log_status!("deploy", "'{}' is now up to date", component.id);
            }
            Ok(None) => {
                // Not behind or no upstream — nothing to do
            }
            Err(_) => {
                // git fetch failed — warn but don't block (might be offline)
                log_status!(
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
) -> crate::Result<()> {
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
        log_status!(
            "deploy",
            "Deploying from tagged releases (--tagged). Use `deploy --head` to include unreleased commits, or `homeboy release` to tag them."
        );
        return Ok(());
    }

    if config.force {
        log_status!(
            "deploy",
            "Deploying from tagged releases (--force). Use `deploy --head` to include unreleased commits, or `homeboy release` to tag them."
        );
        return Ok(());
    }

    let component_list: Vec<String> = gaps
        .iter()
        .map(|(id, gap)| format!("{} ({} commits ahead of {})", id, gap.ahead, gap.tag))
        .collect();

    Err(crate::Error::validation_invalid_argument(
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
