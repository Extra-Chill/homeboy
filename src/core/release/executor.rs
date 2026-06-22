//! Release step implementations.
//!
//! Each step is a free function that takes the component, the mutable
//! [`ReleaseState`] threaded through the release, and whatever step-specific
//! inputs it needs, then returns a [`ReleaseStepResult`]. The caller
//! ([`super::pipeline::run`], via the release plan dispatcher) runs them in
//! order and handles skip-on-failure logic for subsequent steps.
//!
//! This used to be a trait-object-dispatched `PipelineStepExecutor` driving a
//! generic DAG (`engine::pipeline`). In practice every release runs the same
//! linear sequence with a sequential `Mutex<ReleaseContext>` shared between
//! steps, so the DAG scaffolding bought nothing but indirection. The logic
//! inside each `run_*` function is unchanged; only the plumbing is different.

use crate::core::component::Component;
use crate::core::engine::local_files::FileSystem;
use crate::core::engine::validation;
use crate::core::error::{Error, Result};
use crate::core::release::{changelog as release_changelog, version};

use super::types::{ReleaseArtifact, ReleaseState, ReleaseStepResult, ReleaseStepStatus};
use super::utils::extract_latest_notes;

pub(crate) mod artifacts;
pub(crate) mod changelog;
mod git_push;
mod github_release;
pub(crate) mod lockfile_guard;
mod package;
pub(crate) mod package_preflight;
pub(crate) mod prepare;
mod publish;
mod tagging;
pub(crate) mod version_targets;

pub(crate) use git_push::run_git_push;
pub(crate) use github_release::run_github_release;
pub(crate) use package::{build_release_payload, run_extension_release_preflight, run_package};
pub(crate) use publish::{publish_response_output, run_publish};
pub(crate) use tagging::{
    github_release_exists_for_tag, run_git_tag, run_tag_availability_preflight,
};
use version_targets::collect_version_target_mismatches;

/// Build a successful step result with optional data and hints.
pub(crate) fn step_success(
    id: &str,
    step_type: &str,
    data: Option<serde_json::Value>,
    hints: Vec<crate::core::error::Hint>,
) -> ReleaseStepResult {
    ReleaseStepResult {
        id: id.to_string(),
        step_type: step_type.to_string(),
        status: ReleaseStepStatus::Success,
        missing: Vec::new(),
        warnings: Vec::new(),
        hints,
        data,
        error: None,
    }
}

/// Build a failed step result carrying error text and optional data.
pub(crate) fn step_failed(
    id: &str,
    step_type: &str,
    data: Option<serde_json::Value>,
    error: Option<String>,
    hints: Vec<crate::core::error::Hint>,
) -> ReleaseStepResult {
    ReleaseStepResult {
        id: id.to_string(),
        step_type: step_type.to_string(),
        status: ReleaseStepStatus::Failed,
        missing: Vec::new(),
        warnings: Vec::new(),
        hints,
        data,
        error,
    }
}

/// Build a skipped step result carrying an explanatory warning.
pub(crate) fn step_skipped(
    id: &str,
    step_type: &str,
    data: Option<serde_json::Value>,
    warning: impl Into<String>,
) -> ReleaseStepResult {
    ReleaseStepResult {
        id: id.to_string(),
        step_type: step_type.to_string(),
        status: ReleaseStepStatus::Skipped,
        missing: Vec::new(),
        warnings: vec![warning.into()],
        hints: Vec::new(),
        data,
        error: None,
    }
}

// ---------------------------------------------------------------------------
// Core steps
// ---------------------------------------------------------------------------

/// Bump the version file(s) on disk and, if any changelog entries were
/// auto-generated from commits, finalize them into the new version section.
///
/// Populates [`ReleaseState::version`], [`tag`][ReleaseState::tag] (default
/// `v{version}`), and [`notes`][ReleaseState::notes] from the just-written
/// changelog section.
///
/// After the bump, every version target is re-read from disk and compared to
/// the new version. If any target wasn't updated, the step is marked Failed
/// so downstream steps (commit, tag, push) bail out before producing the
/// orphan-tag pattern from issue #2234 — a tag pushed onto an unbumped
/// commit. Without this invariant a silent no-op bump (e.g. a regression in
/// the changelog finalization path that swallows the version write) leaves
/// `state.version` advanced in memory while the working tree stays clean,
/// `git.commit` skips, and `git.tag` lands on the wrong commit.
pub(crate) fn run_version(
    component: &Component,
    state: &mut ReleaseState,
    bump_type: &str,
) -> Result<ReleaseStepResult> {
    let result = version::bump_component_version_with_changelog(
        component,
        bump_type,
        None,
        state.changelog_validation.as_ref(),
    )?;
    let data = serde_json::to_value(&result)
        .map_err(|e| Error::internal_json(e.to_string(), Some("version output".to_string())))?;

    if let Some(mismatches) = collect_version_target_mismatches(component, &result.new_version) {
        let error_msg = format!(
            "Version bump verification failed: {} target(s) on disk are not at {} after bump_component_version: {}. \
             Refusing to continue — tagging now would create an orphan tag (no release: commit, no version-file bump). See issue #2234.",
            mismatches.len(),
            result.new_version,
            mismatches
                .iter()
                .map(|m| format!("{} = {}", m.file, m.found.as_deref().unwrap_or("<unreadable>")))
                .collect::<Vec<_>>()
                .join("; ")
        );
        let mut failure_data = data;
        failure_data["mismatches"] = serde_json::to_value(&mismatches).unwrap_or_default();
        failure_data["new_version"] = serde_json::Value::String(result.new_version.clone());
        return Ok(step_failed(
            "version",
            "version",
            Some(failure_data),
            Some(error_msg),
            vec![crate::core::error::Hint {
                message: "If a previous run partially bumped this component, run `homeboy release <component> --recover` to finish it cleanly.".to_string(),
            }],
        ));
    }

    state.version = Some(result.new_version.clone());
    state.tag = Some(format!("v{}", result.new_version));
    state.notes = Some(load_release_notes(component)?);

    Ok(step_success("version", "version", Some(data), Vec::new()))
}

/// Commit any staged release artifacts (changelog/version files). Amends the
/// HEAD commit when the last commit is already a release commit and the
/// branch is ahead of origin — matches the original amend heuristic.
pub(crate) fn run_git_commit(
    component: &Component,
    component_id: &str,
    state: &ReleaseState,
) -> Result<ReleaseStepResult> {
    let status_output =
        crate::core::git::status_at(Some(component_id), Some(&component.local_path))?;
    let is_clean = status_output.stdout.trim().is_empty();

    if is_clean {
        let data = serde_json::json!({
            "skipped": true,
            "reason": "working tree is clean, nothing to commit"
        });
        return Ok(step_success(
            "git.commit",
            "git.commit",
            Some(data),
            Vec::new(),
        ));
    }

    let should_amend = should_amend_release_commit(&component.local_path)?;
    let message = state
        .version
        .as_ref()
        .map(|v| format!("release: v{}", v))
        .unwrap_or_else(|| "release: unknown".to_string());

    let options = crate::core::git::CommitOptions {
        staged_only: false,
        files: None,
        exclude: None,
        amend: should_amend,
    };

    let output = crate::core::git::commit_at(
        Some(component_id),
        Some(&message),
        options,
        Some(&component.local_path),
    )?;
    let mut data = serde_json::to_value(&output)
        .map_err(|e| Error::internal_json(e.to_string(), Some("git commit output".to_string())))?;

    if should_amend {
        data["amended"] = serde_json::json!(true);
    }

    if output.success {
        Ok(step_success(
            "git.commit",
            "git.commit",
            Some(data),
            Vec::new(),
        ))
    } else {
        Ok(step_failed(
            "git.commit",
            "git.commit",
            Some(data),
            None,
            Vec::new(),
        ))
    }
}

/// Delete release-generated artifact directories. Skipped when the caller chose
/// `--deploy` so the deploy step can still find the artifact.
pub(crate) fn run_cleanup(
    component: &Component,
    state: &ReleaseState,
) -> Result<ReleaseStepResult> {
    let cleanup_paths = release_cleanup_paths(&component.local_path, &state.artifacts);
    let mut removed_paths = Vec::new();

    for path in &cleanup_paths {
        if !path.exists() {
            continue;
        }
        std::fs::remove_dir_all(path).map_err(|e| {
            Error::internal_io(
                format!("Failed to clean up {}: {}", path.display(), e),
                Some(path.display().to_string()),
            )
        })?;
        removed_paths.push(path.display().to_string());
    }

    let distrib_path = std::path::Path::new(&component.local_path).join("target/distrib");
    let data = serde_json::json!({
        "action": "cleanup",
        "path": distrib_path.display().to_string(),
        "paths": cleanup_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>(),
        "removed_paths": removed_paths,
        "removed": !removed_paths.is_empty(),
    });

    Ok(step_success("cleanup", "cleanup", Some(data), Vec::new()))
}

pub(crate) fn release_cleanup_paths(
    local_path: &str,
    artifacts: &[ReleaseArtifact],
) -> Vec<std::path::PathBuf> {
    let component_path = std::path::Path::new(local_path);
    let mut paths = vec![component_path.join("target/distrib")];

    let build_path = component_path.join("build");
    let has_build_artifact = artifacts.iter().any(|artifact| {
        let artifact_path = std::path::Path::new(&artifact.path);
        let artifact_path = if artifact_path.is_absolute() {
            artifact_path.to_path_buf()
        } else {
            component_path.join(artifact_path)
        };

        artifact_path.starts_with(&build_path)
    });

    if has_build_artifact {
        paths.push(build_path);
    }

    paths
}

/// Run the component's `post_release` hook commands. Failures are non-fatal —
/// the release has already been published, so the most we can do is log the
/// warning and surface it in the step result for the overall summary to pick
/// up.
pub(crate) fn run_post_release(
    component: &Component,
    commands: &[String],
) -> Result<ReleaseStepResult> {
    let hook_result = crate::core::engine::hooks::run_commands(
        commands,
        &component.local_path,
        crate::core::engine::hooks::events::POST_RELEASE,
        crate::core::engine::hooks::HookFailureMode::NonFatal,
    )?;

    if !hook_result.all_succeeded {
        for failed in hook_result.commands.iter().filter(|c| !c.success) {
            let error_text = if failed.stderr.trim().is_empty() {
                &failed.stdout
            } else {
                &failed.stderr
            };
            log_status!(
                "warning",
                "Post-release hook failed: '{}': {}",
                failed.command,
                error_text.trim()
            );
        }
    }

    let commands_summary: Vec<serde_json::Value> = hook_result
        .commands
        .iter()
        .map(|c| {
            serde_json::json!({
                "command": c.command,
                "success": c.success,
                "exit_code": c.exit_code,
            })
        })
        .collect();

    let data = serde_json::json!({
        "action": "post_release",
        "commands": commands_summary,
        "all_succeeded": hook_result.all_succeeded,
    });

    Ok(step_success(
        "post_release",
        "post_release",
        Some(data),
        Vec::new(),
    ))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn load_release_notes(component: &Component) -> Result<String> {
    let changelog_path = release_changelog::resolve_changelog_path(component)?;
    let changelog_content = crate::core::engine::local_files::local().read(&changelog_path)?;
    validation::require(
        extract_latest_notes(&changelog_content),
        "changelog",
        "No finalized changelog entries found for release notes",
    )
}

fn should_amend_release_commit(local_path: &str) -> Result<bool> {
    let log_output =
        crate::core::git::execute_git_for_release(local_path, &["log", "-1", "--format=%s"])
            .map_err(|e| Error::internal_io(e.to_string(), Some("git log".to_string())))?;
    if !log_output.status.success() {
        return Ok(false);
    }
    let last_message = String::from_utf8_lossy(&log_output.stdout)
        .trim()
        .to_string();

    if !last_message.starts_with("release: v") {
        return Ok(false);
    }

    let status_output =
        crate::core::git::execute_git_for_release(local_path, &["status", "-sb"])
            .map_err(|e| Error::internal_io(e.to_string(), Some("git status".to_string())))?;
    if !status_output.status.success() {
        return Ok(false);
    }
    let status_str = String::from_utf8_lossy(&status_output.stdout);
    Ok(status_str.contains("[ahead"))
}

/// Payload passed to extension actions — mirrors the pre-refactor shape so
/// extensions don't need to change.
/// Build the optional `config` map forwarded to the packaging extension for
/// `release.package`.
///
/// When `skip_build_validation` is set, the generic `skip_build_validation`
/// signal is forwarded so the extension can bypass its own build-structure
/// assertions while still producing an artifact. Core stays agnostic: it does
/// not know which structure assertions the extension enforces — it only relays
/// the operator's intent (issue #5425). Returns `None` when there is nothing to
/// forward so the default payload is unchanged.
#[cfg(test)]
mod tests {
    use super::git_push::is_non_fast_forward_rejection;
    use super::package::store_artifacts_from_output;
    use super::{github_release, run_cleanup, run_git_push, run_package};
    use crate::core::component::Component;
    use crate::core::deploy::release_download::GitHubRepo;
    use crate::core::extension::ExtensionManifest;
    use crate::core::release::types::ReleaseState;
    use crate::core::release::{ReleaseArtifact, ReleaseStepStatus};
    use std::process::Command;

    fn git(path: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn github_release_repair_commands_include_repo_asset_notes_and_enterprise_env() {
        let github = GitHubRepo {
            host: "github.example.com".to_string(),
            owner: "example-org".to_string(),
            repo: "studio-web".to_string(),
        };
        let artifacts = vec!["build/studio-web.zip".to_string()];

        let repair = github_release::github_release_repair_commands_with_proxy(
            "v0.10.5",
            &github,
            &artifacts,
            Some("v0.10.4"),
            Some("socks5://127.0.0.1:8080"),
        );

        assert_eq!(repair.notes_file, "build/v0.10.5-release-notes.md");
        assert!(repair.notes_guidance.contains("--notes-file"));
        assert!(repair
            .generate_notes_command
            .contains("GH_HOST=github.example.com HTTPS_PROXY=socks5://127.0.0.1:8080 gh api repos/example-org/studio-web/releases/generate-notes"));
        assert!(repair
            .generate_notes_command
            .contains("-f tag_name=v0.10.5"));
        assert!(repair
            .generate_notes_command
            .contains("-f previous_tag_name=v0.10.4"));
        assert!(repair
            .generate_notes_command
            .contains("> build/v0.10.5-release-notes.md"));
        assert!(repair.create_command.contains("gh release create v0.10.5"));
        assert!(repair
            .create_command
            .contains("--notes-file build/v0.10.5-release-notes.md"));
        assert!(repair.create_command.contains("build/studio-web.zip"));
        assert!(repair.create_command.contains("-R example-org/studio-web"));
        assert_eq!(
            repair.view_command,
            "GH_HOST=github.example.com HTTPS_PROXY=socks5://127.0.0.1:8080 gh release view v0.10.5 -R example-org/studio-web"
        );
        assert!(repair
            .env_hint
            .as_deref()
            .unwrap_or_default()
            .contains("GitHub Enterprise host detected"));
    }

    #[test]
    fn github_release_notes_link_full_changelog_to_changelog_file() {
        let notes = concat!(
            "## What's Changed\n",
            "* fix release notes by @example-user in https://github.com/Extra-Chill/homeboy/pull/1\n",
            "\n",
            "**Full Changelog**: https://github.com/Extra-Chill/homeboy/compare/v0.8.1...v0.9.0"
        );

        let rewritten = github_release::replace_full_changelog_footer(
            notes,
            "https://github.com/Extra-Chill/homeboy/blob/v0.9.0/CHANGELOG.md",
        );

        assert!(rewritten.contains(
            "**Full Changelog**: https://github.com/Extra-Chill/homeboy/blob/v0.9.0/CHANGELOG.md"
        ));
        assert!(!rewritten.contains("/compare/v0.8.1...v0.9.0"));
    }

    #[test]
    fn github_release_notes_append_changelog_link_when_footer_is_missing() {
        let rewritten = github_release::replace_full_changelog_footer(
            "## What's Changed\n* release note",
            "https://github.com/Extra-Chill/homeboy/blob/v0.9.0/docs/CHANGELOG.md",
        );

        assert_eq!(
            rewritten,
            concat!(
                "## What's Changed\n",
                "* release note\n\n",
                "**Full Changelog**: https://github.com/Extra-Chill/homeboy/blob/v0.9.0/docs/CHANGELOG.md"
            )
        );
    }

    #[test]
    fn cleanup_removes_build_dir_when_release_artifact_is_inside_build() {
        let temp = tempfile::tempdir().expect("tempdir");
        let build_dir = temp.path().join("build");
        let distrib_dir = temp.path().join("target/distrib");
        std::fs::create_dir_all(&build_dir).expect("build dir");
        std::fs::create_dir_all(&distrib_dir).expect("distrib dir");
        let artifact_path = build_dir.join("fixture.zip");
        std::fs::write(&artifact_path, "artifact").expect("artifact");

        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Component::default()
        };
        let state = ReleaseState {
            artifacts: vec![ReleaseArtifact {
                path: artifact_path.display().to_string(),
                artifact_type: None,
                platform: None,
            }],
            ..ReleaseState::default()
        };

        let result = run_cleanup(&component, &state).expect("cleanup");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert!(!build_dir.exists());
        assert!(!distrib_dir.exists());
    }

    #[test]
    fn cleanup_leaves_build_dir_without_release_artifact() {
        let temp = tempfile::tempdir().expect("tempdir");
        let build_dir = temp.path().join("build");
        std::fs::create_dir_all(&build_dir).expect("build dir");
        std::fs::write(build_dir.join("bundle.js"), "source build").expect("build file");

        let component = Component {
            id: "fixture".to_string(),
            local_path: temp.path().to_string_lossy().to_string(),
            ..Component::default()
        };
        let state = ReleaseState::default();

        let result = run_cleanup(&component, &state).expect("cleanup");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert!(build_dir.exists());
    }

    #[test]
    fn package_error_message_is_extension_generic() {
        let response = serde_json::json!({
            "success": false,
            "exitCode": 1,
            "stdout": "",
            "stderr": "",
        });
        let mut state = crate::core::release::types::ReleaseState::default();

        let err = store_artifacts_from_output(&mut state, &response)
            .expect_err("empty failing package output should fail");

        assert!(err.message.contains("required packaging tool"));
        assert!(!err.message.contains("example-package-manager"));
    }

    #[test]
    fn package_failure_surfaces_both_stdout_and_stderr() {
        // Issue #3238: when the build command fails mid-stream, stdout
        // contains partial progress (e.g. "Installing npm dependencies...")
        // and stderr contains the real error.  Both must appear in the
        // structured error payload so the operator can diagnose the failure.
        let response = serde_json::json!({
            "success": false,
            "exitCode": 1,
            "stdout": "[BUILD] Installing npm dependencies...\n",
            "stderr": "npm error: ERESOLVE unable to resolve dependency tree\n",
        });
        let mut state = crate::core::release::types::ReleaseState::default();

        let err = store_artifacts_from_output(&mut state, &response)
            .expect_err("failing package output should fail");

        // stderr (the npm error) is the primary message
        assert!(
            err.message.contains("npm error: ERESOLVE"),
            "error should surface npm stderr, got: {}",
            err.message
        );
        // stdout (the build progress) is appended as additional context
        assert!(
            err.message.contains("Installing npm dependencies"),
            "error should also include stdout context, got: {}",
            err.message
        );
        assert!(
            err.message.contains("--- stdout ---"),
            "error should label the stdout section"
        );
        assert!(err.message.contains("exit 1"));
    }

    #[test]
    fn package_failure_with_stdout_only_still_surfaces_output() {
        // When the build writes everything to stdout and then dies, the
        // error should include that stdout rather than a generic JSON-parse
        // failure.
        let response = serde_json::json!({
            "success": false,
            "exitCode": 42,
            "stdout": "[BUILD] Installing npm dependencies...\nnpm error: ENOTFOUND registry\n",
            "stderr": "",
        });
        let mut state = crate::core::release::types::ReleaseState::default();

        let err = store_artifacts_from_output(&mut state, &response)
            .expect_err("failing package with stdout-only should fail");

        assert!(
            err.message.contains("npm error: ENOTFOUND registry"),
            "error should surface stdout content, got: {}",
            err.message
        );
        assert!(err.message.contains("exit 42"));
    }

    #[test]
    fn package_failure_detects_nonzero_exit_without_success_field() {
        // Some extension responses omit the "success" field.  In that case
        // the exit code alone must trigger the failure path.
        let response = serde_json::json!({
            "exitCode": 1,
            "stdout": "[BUILD] Installing npm dependencies...\n",
            "stderr": "some build error\n",
        });
        let mut state = crate::core::release::types::ReleaseState::default();

        let err = store_artifacts_from_output(&mut state, &response)
            .expect_err("nonzero exit without success field should fail");

        assert!(err.message.contains("some build error"));
    }

    #[test]
    fn package_success_output_fails_when_frontend_assets_are_missing() {
        let response = serde_json::json!({
            "success": true,
            "exitCode": 0,
            "stdout": "[{\"path\":\"build/studio-native.zip\",\"type\":\"archive\"}]",
            "stderr": concat!(
                "[SUCCESS] All nested packages built successfully\n",
                "[WARNING] Build completed with frontend warnings\n",
                "[WARNING] Frontend assets were NOT included.\n",
                "[WARNING] Fix the frontend build to include JS/CSS.\n"
            ),
        });
        let mut state = crate::core::release::types::ReleaseState::default();

        let err = store_artifacts_from_output(&mut state, &response)
            .expect_err("missing required frontend assets should fail package validation");

        assert!(err.message.contains("required frontend assets"));
        assert!(err.message.contains("Frontend assets were NOT included"));
        assert!(state.artifacts.is_empty());
    }

    #[test]
    fn package_success_with_valid_json_still_works() {
        // Regression guard: the new success-first check must not break the
        // happy path.
        let response = serde_json::json!({
            "success": true,
            "exitCode": 0,
            "stdout": "[{\"path\":\"build/artifact.zip\",\"type\":\"archive\"}]",
            "stderr": "",
        });
        let mut state = crate::core::release::types::ReleaseState::default();

        store_artifacts_from_output(&mut state, &response).expect("valid artifacts should parse");

        assert_eq!(state.artifacts.len(), 1);
        assert_eq!(state.artifacts[0].path, "build/artifact.zip");
    }

    fn release_package_extension(id: &str, command: &str) -> ExtensionManifest {
        let mut manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": id,
            "version": "1.0.0",
            "actions": [{
                "id": "release.package",
                "label": "Package release",
                "type": "command",
                "command": command,
            }]
        }))
        .expect("manifest fixture");
        manifest.id = id.to_string();
        manifest
    }

    fn non_package_extension(id: &str) -> ExtensionManifest {
        let mut manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
            "name": id,
            "version": "1.0.0",
            "actions": [{
                "id": "release.prepare",
                "label": "Prepare release",
                "type": "command",
                "command": "true",
            }]
        }))
        .expect("manifest fixture");
        manifest.id = id.to_string();
        manifest
    }

    #[test]
    fn run_package_collects_artifacts_from_multiple_package_providers() {
        crate::test_support::with_isolated_home(|_| {
            let component = tempfile::tempdir().expect("component tempdir");
            let package_a = release_package_extension(
                "package-a",
                "printf '[{\"path\":\"target/package-a.tgz\",\"type\":\"archive\"}]'",
            );
            let package_b = release_package_extension(
                "package-b",
                "printf '[{\"path\":\"target/package-b.zip\",\"type\":\"archive\"}]'",
            );
            crate::core::extension::save_manifest(&package_a).expect("save package A extension");
            crate::core::extension::save_manifest(&package_b).expect("save package B extension");

            let mut state = crate::core::release::types::ReleaseState::default();
            let result = run_package(
                &[package_a, non_package_extension("docs"), package_b],
                &mut state,
                "fixture",
                &component.path().to_string_lossy(),
                false,
            )
            .expect("package step");

            assert_eq!(result.status, ReleaseStepStatus::Success);
            assert_eq!(state.artifacts.len(), 2);
            assert_eq!(state.artifacts[0].path, "target/package-a.tgz");
            assert_eq!(state.artifacts[1].path, "target/package-b.zip");
            let data = result.data.expect("package data");
            assert_eq!(
                data["extensions"],
                serde_json::json!(["package-a", "package-b"])
            );
        });
    }

    #[test]
    fn run_package_passes_component_id_from_release_payload_to_action_env() {
        crate::test_support::with_isolated_home(|_| {
            let component = tempfile::tempdir_in(std::env::temp_dir())
                .expect("component tempdir with mismatched basename");
            let package = release_package_extension(
                "wordpress",
                "printf '[{\"path\":\"build/%s.zip\",\"type\":\"wordpress\"}]' \"$HOMEBOY_COMPONENT_ID\"",
            );
            crate::core::extension::save_manifest(&package).expect("save package extension");

            let mut state = crate::core::release::types::ReleaseState::default();
            let result = run_package(
                &[package],
                &mut state,
                "intelligence-horse-theme",
                &component.path().to_string_lossy(),
                false,
            )
            .expect("package step");

            assert_eq!(result.status, ReleaseStepStatus::Success);
            assert_eq!(state.artifacts.len(), 1);
            assert_eq!(
                state.artifacts[0].path,
                "build/intelligence-horse-theme.zip"
            );
        });
    }

    #[test]
    fn run_package_failure_names_the_failing_package_provider() {
        crate::test_support::with_isolated_home(|_| {
            let component = tempfile::tempdir().expect("component tempdir");
            let package_a = release_package_extension(
                "package-a",
                "printf '[{\"path\":\"target/package-a.tgz\"}]'",
            );
            let package_b = release_package_extension(
                "package-b",
                "printf 'archive command failed' >&2; exit 7",
            );
            crate::core::extension::save_manifest(&package_a).expect("save package A extension");
            crate::core::extension::save_manifest(&package_b).expect("save package B extension");

            let mut state = crate::core::release::types::ReleaseState::default();
            let err = run_package(
                &[package_a, package_b],
                &mut state,
                "fixture",
                &component.path().to_string_lossy(),
                false,
            )
            .expect_err("failing package provider should fail the step");

            assert!(err
                .message
                .contains("release.package failed for extension 'package-b'"));
            assert!(err.message.contains("archive command failed"));
            assert_eq!(state.artifacts.len(), 1);
            assert_eq!(state.artifacts[0].path, "target/package-a.tgz");
        });
    }

    #[test]
    fn run_package_retries_transient_failure_then_surfaces_full_output() {
        // Issue #3238: the package command (npm install inside build.sh) can
        // fail intermittently.  A bounded retry gives the warm-cache path a
        // chance; when both attempts fail, the error must surface the full
        // captured stdout AND stderr so the operator can diagnose it.
        crate::test_support::with_isolated_home(|_| {
            let component = tempfile::tempdir().expect("component tempdir");
            // Simulates a build.sh that prints progress to stdout, writes
            // the npm error to stderr, and exits non-zero on every attempt.
            let package = release_package_extension(
                "wordpress",
                "printf '[BUILD] Installing npm dependencies...\\n'; \
                 printf 'npm error: ERESOLVE\\n' >&2; exit 1",
            );
            crate::core::extension::save_manifest(&package).expect("save package extension");

            let mut state = crate::core::release::types::ReleaseState::default();
            let err = run_package(
                &[package],
                &mut state,
                "fixture",
                &component.path().to_string_lossy(),
                false,
            )
            .expect_err("persistently failing package should fail after retry");

            // stderr (npm error) must be surfaced — this is the core bug.
            assert!(
                err.message.contains("npm error: ERESOLVE"),
                "error should surface npm stderr, got: {}",
                err.message
            );
            // stdout (build progress) should also be visible.
            assert!(
                err.message.contains("Installing npm dependencies"),
                "error should include build progress stdout, got: {}",
                err.message
            );
            // No artifacts stored from the failed build.
            assert!(state.artifacts.is_empty());
        });
    }

    #[test]
    fn run_package_retries_transient_failure_then_recovers() {
        // When the first attempt fails but the retry succeeds, the step
        // should complete normally with the artifacts from the successful
        // attempt.
        crate::test_support::with_isolated_home(|_| {
            let component = tempfile::tempdir().expect("component tempdir");
            let marker = component.path().join("attempt-counter");
            // Script that fails on the first invocation but succeeds on the
            // second (warm-cache retry).
            let script = format!(
                "n=$(cat '{marker}' 2>/dev/null || echo 0); \
                 echo $((n + 1)) > '{marker}'; \
                 if [ \"$n\" = \"0\" ]; then \
                   printf 'npm error: transient registry failure\\n' >&2; exit 1; \
                 else \
                   printf '[{{\"path\":\"build/artifact.zip\",\"type\":\"archive\"}}]'; \
                 fi",
                marker = marker.display()
            );
            let package = release_package_extension("wordpress", &script);
            crate::core::extension::save_manifest(&package).expect("save package extension");

            let mut state = crate::core::release::types::ReleaseState::default();
            let result = run_package(
                &[package],
                &mut state,
                "fixture",
                &component.path().to_string_lossy(),
                false,
            )
            .expect("retry after transient failure should succeed");

            assert_eq!(result.status, ReleaseStepStatus::Success);
            assert_eq!(state.artifacts.len(), 1);
            assert_eq!(state.artifacts[0].path, "build/artifact.zip");
        });
    }

    // ----- Orphan-tag regression coverage (issue #2234) -----

    use super::version_targets::{
        collect_head_version_mismatches, collect_version_target_mismatches,
    };
    use super::{run_git_tag, run_tag_availability_preflight};
    use crate::core::component::VersionTarget;

    fn run_in(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
        let output = std::process::Command::new(args[0])
            .args(&args[1..])
            .current_dir(dir)
            .output()
            .expect("spawn command");
        assert!(
            output.status.success(),
            "command {:?} failed: stdout={:?} stderr={:?}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        output
    }

    /// Fixture: a git repo with one committed plugin header file at version
    /// `committed_version`. The working tree is clean. Returns (temp, component).
    fn plugin_repo_at(committed_version: &str) -> (tempfile::TempDir, Component) {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = temp.path();
        run_in(dir, &["git", "init", "-q"]);
        run_in(dir, &["git", "config", "user.email", "test@example.com"]);
        run_in(dir, &["git", "config", "user.name", "Test"]);
        run_in(dir, &["git", "config", "commit.gpgsign", "false"]);

        let plugin = format!(
            "<?php\n/*\nPlugin Name: Fixture\nVersion: {}\n*/\n",
            committed_version
        );
        std::fs::write(dir.join("plugin.php"), plugin).expect("write plugin");
        run_in(dir, &["git", "add", "."]);
        run_in(dir, &["git", "commit", "-q", "-m", "Initial commit"]);

        let component = Component {
            id: "fixture".to_string(),
            local_path: dir.to_string_lossy().to_string(),
            version_targets: Some(vec![VersionTarget {
                file: "plugin.php".to_string(),
                pattern: Some(r"(?:Version|version)[:=]\s+([0-9]+\.[0-9]+\.[0-9]+)".to_string()),
                artifact_path: None,
            }]),
            ..Component::default()
        };
        (temp, component)
    }

    #[test]
    fn collect_version_target_mismatches_returns_none_when_disk_matches_expected() {
        let (_temp, component) = plugin_repo_at("0.6.13");
        assert!(collect_version_target_mismatches(&component, "0.6.13").is_none());
    }

    #[test]
    fn collect_version_target_mismatches_flags_unbumped_target() {
        let (_temp, component) = plugin_repo_at("0.6.12");
        let mismatches = collect_version_target_mismatches(&component, "0.6.13")
            .expect("expected mismatch on stale version file");
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].file, "plugin.php");
        assert_eq!(mismatches[0].expected, "0.6.13");
        assert_eq!(mismatches[0].found.as_deref(), Some("0.6.12"));
    }

    #[test]
    fn collect_head_version_mismatches_flags_when_head_lacks_new_version() {
        // HEAD has 0.6.12 committed, but state.version is 0.6.13.
        // Working tree is also at 0.6.12 (no bump happened). HEAD check fires.
        let (_temp, component) = plugin_repo_at("0.6.12");
        let mismatches = collect_head_version_mismatches(&component, "0.6.13")
            .expect("HEAD should not show 0.6.13");
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].found.as_deref(), Some("0.6.12"));
    }

    #[test]
    fn collect_head_version_mismatches_returns_none_when_head_matches() {
        let (_temp, component) = plugin_repo_at("0.6.13");
        assert!(collect_head_version_mismatches(&component, "0.6.13").is_none());
    }

    /// Fixture: a git repo whose toplevel contains a `subdir/` with the
    /// version target file. `component.local_path` points at `<toplevel>/subdir`,
    /// NOT the git toplevel. This mirrors the monorepo-extension layout
    /// (`homeboy-extensions/wordpress`, `homeboy-extensions/swift`, etc.) that
    /// triggers issue #2327.
    fn plugin_repo_at_subdir(
        committed_version: &str,
        subdir: &str,
    ) -> (tempfile::TempDir, Component) {
        let temp = tempfile::tempdir().expect("tempdir");
        let toplevel = temp.path();
        run_in(toplevel, &["git", "init", "-q"]);
        run_in(
            toplevel,
            &["git", "config", "user.email", "test@example.com"],
        );
        run_in(toplevel, &["git", "config", "user.name", "Test"]);
        run_in(toplevel, &["git", "config", "commit.gpgsign", "false"]);

        let sub = toplevel.join(subdir);
        std::fs::create_dir_all(&sub).expect("create subdir");

        let plugin = format!(
            "<?php\n/*\nPlugin Name: Fixture\nVersion: {}\n*/\n",
            committed_version
        );
        std::fs::write(sub.join("plugin.php"), plugin).expect("write plugin");
        run_in(toplevel, &["git", "add", "."]);
        run_in(toplevel, &["git", "commit", "-q", "-m", "Initial commit"]);

        let component = Component {
            id: "fixture".to_string(),
            local_path: sub.to_string_lossy().to_string(),
            version_targets: Some(vec![VersionTarget {
                file: "plugin.php".to_string(),
                pattern: Some(r"(?:Version|version)[:=]\s+([0-9]+\.[0-9]+\.[0-9]+)".to_string()),
                artifact_path: None,
            }]),
            ..Component::default()
        };
        (temp, component)
    }

    #[test]
    fn collect_head_version_mismatches_works_in_monorepo_subdir() {
        // Issue #2327: when the component's local_path is a subdir of the git
        // toplevel (monorepo extension layout), `git show HEAD:<path>` must
        // resolve `<path>` against the git toplevel, not against
        // component.local_path. Before the fix this returns `None` because
        // `git show HEAD:plugin.php` fails ("path subdir/plugin.php exists,
        // but not plugin.php"), which makes the HEAD invariant treat the
        // committed version as `<unreadable>` and either flag a spurious
        // mismatch or silently pass when comparing `None != Some(expected)`.
        let (_temp, component) = plugin_repo_at_subdir("0.6.13", "wordpress");

        // HEAD has 0.6.13. With a correct toplevel-relative path resolution,
        // `read_version_at_head` returns Some("0.6.13") and the mismatch
        // collector returns None.
        assert!(
            collect_head_version_mismatches(&component, "0.6.13").is_none(),
            "HEAD has the expected version; mismatch collector should return None \
             but the bug makes git show fail and mismatches are reported with \
             found = None"
        );

        // And when HEAD does NOT have the expected version, the collector
        // must still surface the real committed value (0.6.13) — not None.
        let mismatches = collect_head_version_mismatches(&component, "0.7.0")
            .expect("HEAD does not have 0.7.0; mismatch expected");
        assert_eq!(mismatches.len(), 1);
        assert_eq!(mismatches[0].file, "plugin.php");
        assert_eq!(mismatches[0].expected, "0.7.0");
        assert_eq!(
            mismatches[0].found.as_deref(),
            Some("0.6.13"),
            "found value must be the version actually committed at HEAD, \
             not None from a failed `git show HEAD:<wrong-path>`"
        );
    }

    #[test]
    fn git_tag_step_refuses_invalid_release_tags() {
        // The orphan-tag scenario from issue #2234: the in-memory state.version
        // says 0.6.13, but HEAD's plugin.php still reads 0.6.12. Without the
        // invariant check this would happily push a tag onto the wrong commit.
        let (_temp, component) = plugin_repo_at("0.6.12");
        let mut state = ReleaseState {
            version: Some("0.6.13".to_string()),
            tag: Some("v0.6.13".to_string()),
            ..ReleaseState::default()
        };

        let result = run_git_tag(&component, "fixture", &mut state, "v0.6.13")
            .expect("step should return a result, not propagate Err");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        let err = result.error.expect("expected failure error");
        assert!(
            err.contains("issue #2234"),
            "expected #2234 reference in error, got: {}",
            err
        );
        assert!(err.contains("v0.6.13"), "error should name the tag");
        assert!(
            !crate::core::git::tag_exists_locally(&component.local_path, "v0.6.13").unwrap_or(true),
            "tag must NOT have been created when invariant fails"
        );

        let (_temp, component) = plugin_repo_at("0.6.12");
        let component_path = std::path::Path::new(&component.local_path);
        run_in(component_path, &["git", "tag", "v0.6.13"]);

        std::fs::write(
            component_path.join("plugin.php"),
            "<?php\n/*\nPlugin Name: Fixture\nVersion: 0.6.13\n*/\n",
        )
        .expect("write bumped plugin");
        run_in(component_path, &["git", "add", "plugin.php"]);
        run_in(
            component_path,
            &["git", "commit", "-q", "-m", "release: v0.6.13"],
        );

        let mut state = ReleaseState {
            version: Some("0.6.13".to_string()),
            tag: Some("v0.6.13".to_string()),
            ..ReleaseState::default()
        };

        let result = run_git_tag(&component, "fixture", &mut state, "v0.6.13")
            .expect("existing tag on older commit should return a failed step");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        let err = result.error.expect("expected tag divergence error");
        assert!(
            err.contains("does not point at HEAD"),
            "expected tag divergence message, got: {}",
            err
        );
        assert!(err.contains("Local tag:"), "expected local tag state");
        assert!(err.contains("origin tag:"), "expected remote tag state");
        assert!(
            result
                .data
                .as_ref()
                .and_then(|data| data.get("local_tag"))
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "expected local tag commit in structured data"
        );
        assert!(
            result
                .hints
                .iter()
                .any(|hint| hint.message.contains("previous successful release")),
            "expected next-version recovery guidance"
        );

        let (_temp, component) = plugin_repo_at("0.6.12");
        let component_path = std::path::Path::new(&component.local_path);
        let remote = tempfile::tempdir().expect("remote tempdir");
        run_in(remote.path(), &["git", "init", "--bare", "-q"]);
        run_in(
            component_path,
            &[
                "git",
                "remote",
                "add",
                "origin",
                remote.path().to_str().expect("remote path"),
            ],
        );
        run_in(component_path, &["git", "tag", "v0.6.13"]);
        run_in(component_path, &["git", "push", "origin", "v0.6.13"]);
        run_in(component_path, &["git", "tag", "-d", "v0.6.13"]);

        std::fs::write(
            component_path.join("plugin.php"),
            "<?php\n/*\nPlugin Name: Fixture\nVersion: 0.6.13\n*/\n",
        )
        .expect("write bumped plugin");
        run_in(component_path, &["git", "add", "plugin.php"]);
        run_in(
            component_path,
            &["git", "commit", "-q", "-m", "release: v0.6.13"],
        );

        let mut state = ReleaseState {
            version: Some("0.6.13".to_string()),
            tag: Some("v0.6.13".to_string()),
            ..ReleaseState::default()
        };

        let result = run_git_tag(&component, "fixture", &mut state, "v0.6.13")
            .expect("remote-only divergent tag should return a failed step");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(
            !crate::core::git::tag_exists_locally(&component.local_path, "v0.6.13").unwrap(),
            "remote-only divergent tag must not be recreated locally"
        );
        assert!(
            result
                .data
                .as_ref()
                .and_then(|data| data.get("remote_tag"))
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "expected remote tag commit in structured data"
        );
        assert!(
            result
                .hints
                .iter()
                .any(|hint| hint.message.contains("git push origin :refs/tags/v0.6.13")),
            "expected deliberate remote tag cleanup guidance"
        );
    }

    #[test]
    fn tag_availability_preflight_refuses_existing_release_tags_before_mutation() {
        let (_temp, component) = plugin_repo_at("0.6.12");
        let component_path = std::path::Path::new(&component.local_path);
        run_in(component_path, &["git", "tag", "v0.6.13"]);

        let result = run_tag_availability_preflight(&component, "fixture", "v0.6.13")
            .expect("preflight should return a result, not propagate Err");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert_eq!(result.id, "preflight.tag_availability");
        let err = result.error.expect("expected tag availability error");
        assert!(
            err.contains("Release tag v0.6.13 already exists"),
            "expected existing tag message, got: {}",
            err
        );
        assert!(err.contains("Local tag:"), "expected local tag state");
        assert!(
            result
                .hints
                .iter()
                .any(|hint| hint.message.contains("git tag -d v0.6.13")),
            "expected deliberate local tag cleanup guidance"
        );

        let (_temp, component) = plugin_repo_at("0.6.12");
        let component_path = std::path::Path::new(&component.local_path);
        let remote = tempfile::tempdir().expect("remote tempdir");
        run_in(remote.path(), &["git", "init", "--bare", "-q"]);
        run_in(
            component_path,
            &[
                "git",
                "remote",
                "add",
                "origin",
                remote.path().to_str().expect("remote path"),
            ],
        );
        run_in(component_path, &["git", "tag", "v0.6.13"]);
        run_in(component_path, &["git", "push", "origin", "v0.6.13"]);
        run_in(component_path, &["git", "tag", "-d", "v0.6.13"]);

        let result = run_tag_availability_preflight(&component, "fixture", "v0.6.13")
            .expect("remote-only existing tag should return a failed step");

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert!(
            result
                .data
                .as_ref()
                .and_then(|data| data.get("remote_tag"))
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "expected origin tag commit in structured data"
        );
        assert!(
            result
                .hints
                .iter()
                .any(|hint| hint.message.contains("git push origin :refs/tags/v0.6.13")),
            "expected deliberate origin tag cleanup guidance"
        );
    }

    #[test]
    fn git_tag_step_creates_tag_when_head_matches_state_version() {
        let (_temp, component) = plugin_repo_at("0.6.13");
        let mut state = ReleaseState {
            version: Some("0.6.13".to_string()),
            tag: Some("v0.6.13".to_string()),
            ..ReleaseState::default()
        };

        let result = run_git_tag(&component, "fixture", &mut state, "v0.6.13")
            .expect("step should succeed when HEAD shows the bumped version");

        assert_eq!(result.status, ReleaseStepStatus::Success);
        assert!(
            crate::core::git::tag_exists_locally(&component.local_path, "v0.6.13").unwrap_or(false),
            "tag should have been created on HEAD"
        );
    }
}
