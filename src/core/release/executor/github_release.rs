//! GitHub Release helper result builders and probes.

use crate::core::component::Component;
use crate::core::component::GithubConfig;
use crate::core::deploy::release_download::GitHubRepo;
use crate::core::error::{Error, Result};
use crate::core::release::changelog;
use crate::core::release::types::{ReleaseState, ReleaseStepResult};

use super::{step_failed, step_success};

#[derive(Debug, Clone)]
pub(super) struct GitHubReleaseRepairCommands {
    pub notes_file: String,
    pub notes_guidance: String,
    pub generate_notes_command: String,
    pub create_command: String,
    pub view_command: String,
    pub env_hint: Option<String>,
    /// True when `notes_file` is the persisted exact Homeboy release body
    /// (issue #3508), so recovery reproduces the identical body rather than
    /// regenerating notes that could diverge.
    pub exact_body_available: bool,
}

/// Create a GitHub Release for the just-pushed tag.
///
/// The step result must faithfully represent whether a GitHub Release object
/// now exists, because downstream `publish.<target>` / upload steps assume the
/// release is present (see issue #3541). The rules are:
///
/// - Release object created (or already exists) → `Success`.
/// - Release object NOT created and not recoverable here (no `gh` binary, not
///   authenticated, `gh release create` failed) → `Failed`, carrying the exact
///   recovery commands so the operator can resume from the pushed tag + built
///   artifacts without making a second tag.
/// - Generated release notes failed → we retry the create with fallback notes
///   (the changelog section, or a minimal body) and verify the release exists.
///   Only if that fallback create also fails do we mark the step `Failed`.
///
/// `github.release` is a release-pipeline show-stopper, so a `Failed` result
/// halts the plan before publish/upload runs against a non-existent release.
pub(crate) fn run_github_release(
    component: &Component,
    state: &ReleaseState,
) -> Result<ReleaseStepResult> {
    let tag = state.tag.clone().ok_or_else(|| {
        Error::internal_unexpected(
            "github.release: tag state not set (git.tag must run first)".to_string(),
        )
    })?;
    let local_path = &component.local_path;

    let remote_url = component
        .remote_url
        .clone()
        .or_else(|| {
            crate::core::deploy::release_download::detect_remote_url(std::path::Path::new(
                local_path,
            ))
        })
        .ok_or_else(|| {
            Error::internal_unexpected(
                "github.release: no remote_url configured and git remote get-url origin failed"
                    .to_string(),
            )
        })?;

    let github =
        crate::core::deploy::release_download::parse_github_url(&remote_url).ok_or_else(|| {
            Error::validation_invalid_argument(
                "github.release",
                format!("Remote URL '{}' is not a GitHub URL", remote_url),
                None,
                Some(vec![
                    "Use a GitHub or GitHub Enterprise remote for automatic GitHub Releases"
                        .to_string(),
                    "Use --no-github-release to skip this step".to_string(),
                ]),
            )
        })?;

    // Collect artifact paths from state. Populated by release.package
    // (or any other extension action that emits artifact metadata into
    // ReleaseState::artifacts). Passing these to `gh release create` or
    // `gh release upload --clobber` attaches them to the Release in a
    // single API call — keeping the github.release step responsible for
    // the full Release lifecycle (entry + assets) instead of requiring a
    // separate publish.<target> step.
    let artifact_paths: Vec<String> = state
        .artifacts
        .iter()
        .filter(|artifact| {
            std::fs::metadata(&artifact.path)
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
        })
        .map(|artifact| artifact.path.clone())
        .collect();
    let has_artifacts = !artifact_paths.is_empty();
    // Repair commands available before the exact body is built/persisted (gh
    // missing / unauthenticated / upload paths). These regenerate notes since
    // no persisted exact-body file exists yet. The create path below rebuilds a
    // persisted-aware closure once the body is written to disk (issue #3508).
    let repair_commands = |notes_start_tag: Option<&str>| {
        github_release_repair_commands(
            &tag,
            &github,
            &component.github,
            &artifact_paths,
            notes_start_tag,
            None,
        )
    };

    if !gh_is_available() {
        let repair = repair_commands(None);
        log_status!(
            "release",
            "✗ `gh` CLI not found on PATH — GitHub Release was NOT created"
        );
        log_repair_commands(&repair);
        return Ok(not_created_result(
            &tag,
            &github,
            "gh-not-available",
            "`gh` CLI not found on PATH; GitHub Release was not created.",
            repair,
        ));
    }

    if !gh_is_authenticated(&github, &component.github) {
        let repair = repair_commands(None);
        log_status!(
            "release",
            "✗ `gh` is not authenticated — GitHub Release was NOT created"
        );
        log_status!("release", "Authenticate with `gh auth login`, then run:");
        log_repair_commands(&repair);
        return Ok(not_created_result(
            &tag,
            &github,
            "gh-not-authenticated",
            "`gh` is not authenticated; GitHub Release was not created.",
            repair,
        ));
    }

    let repo_flag = format!("{}/{}", github.owner, github.repo);
    if gh_release_exists(&github, &component.github, &tag, &repo_flag) {
        // Release entry already exists (idempotent retry, or release
        // created out of band). When the release has no artifacts to
        // attach, skip — there is nothing to update. When artifacts are
        // present, upload them with --clobber so retries keep the latest
        // build attached without duplicating the GitHub Release entry.
        if !has_artifacts {
            log_status!(
                "release",
                "GitHub Release {} already exists for {} — skipping (idempotent)",
                tag,
                repo_flag
            );
            return Ok(skipped_result(
                &tag,
                &github,
                "release-already-exists",
                None,
            ));
        }

        log_status!(
            "release",
            "GitHub Release {} already exists for {} — uploading {} artifact(s) with --clobber",
            tag,
            repo_flag,
            artifact_paths.len()
        );

        let mut upload_args: Vec<&str> = vec!["release", "upload", &tag];
        for path in &artifact_paths {
            upload_args.push(path);
        }
        upload_args.extend_from_slice(&["--clobber", "-R", &repo_flag]);

        let upload_output = gh_command(&github, &component.github, &upload_args)
            .output()
            .map_err(|e| {
                Error::internal_io(
                    format!("Failed to invoke gh: {}", e),
                    Some("gh release upload".to_string()),
                )
            })?;

        if !upload_output.status.success() {
            let stderr = String::from_utf8_lossy(&upload_output.stderr).to_string();
            let stdout = String::from_utf8_lossy(&upload_output.stdout).to_string();
            let repair = repair_commands(None);
            log_status!("release", "✗ `gh release upload` failed: {}", stderr.trim());
            log_repair_commands(&repair);
            return Ok(upload_failed_result(
                &tag,
                &github,
                stdout,
                stderr,
                artifact_paths.len(),
                repair,
            ));
        }

        return Ok(upload_success_result(&tag, &github, artifact_paths.len()));
    }

    let notes_start_tag = github_generated_notes_start_tag(component, &tag)?;
    let changelog_url = github_changelog_url(component, &github, &tag);

    // Build the EXACT body Homeboy will post (issue #3508). This is the single
    // source of truth for the release body — generated notes + changelog footer,
    // or the changelog-section fallback + footer. Persisting it (below) lets the
    // repair commands reproduce the identical body via `--notes-file` instead of
    // re-deriving it from source and risking a divergent body.
    let body = build_github_release_body(
        component,
        &github,
        &tag,
        state,
        changelog_url.as_deref(),
        notes_start_tag.as_deref(),
    );
    let generated_notes_ok = body.generated_notes_ok;
    let release_notes = body.body.clone();

    // Persist the exact body so it is inspectable after the fact and so the
    // repair `--notes-file` reproduces it byte-for-byte. A failure to write the
    // artifact is non-fatal: fall back to commands that regenerate notes.
    let persisted_notes_path = persist_release_body(component, &tag, &release_notes);
    let repair_commands = |notes_start_tag: Option<&str>| {
        github_release_repair_commands(
            &tag,
            &github,
            &component.github,
            &artifact_paths,
            notes_start_tag,
            persisted_notes_path.as_deref(),
        )
    };

    log_status!(
        "release",
        "Creating GitHub Release {} on {} with {} artifact(s)...",
        tag,
        repo_flag,
        artifact_paths.len()
    );

    // Build args dynamically so we can append artifact paths as positional
    // arguments — `gh release create <tag> [files...]` attaches each file
    // as a Release asset in the same API call.
    let mut create_args: Vec<&str> = vec![
        "release",
        "create",
        &tag,
        "--title",
        &tag,
        "--notes",
        &release_notes,
        "-R",
        &repo_flag,
    ];
    // Only pass --notes-start-tag when generated notes succeeded. With explicit
    // fallback `--notes`, re-triggering the note generation that just failed
    // would be pointless and could fail the create for the same reason.
    if generated_notes_ok {
        if let Some(previous_tag) = notes_start_tag.as_deref() {
            create_args.extend_from_slice(&["--notes-start-tag", previous_tag]);
        }
    }
    for path in &artifact_paths {
        create_args.push(path);
    }

    let output = gh_command(&github, &component.github, &create_args)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to invoke gh: {}", e),
                Some("gh release create".to_string()),
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let repair = repair_commands(notes_start_tag.as_deref());
        // Distinguish the path that brought us here so operators (and tests)
        // can see whether the fallback-after-generated-notes-failure also
        // failed, versus a plain create failure with working notes.
        let reason = if generated_notes_ok {
            "gh-command-failed"
        } else {
            "generated-notes-failed"
        };
        log_status!("release", "✗ `gh release create` failed: {}", stderr.trim());
        log_repair_commands(&repair);
        return Ok(create_failed_result(
            &tag,
            &github,
            reason,
            stdout,
            stderr,
            repair,
            &body,
            persisted_notes_path.as_deref(),
        ));
    }

    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    log_status!("release", "Created GitHub Release: {}", url);

    Ok(step_success(
        "github.release",
        "github.release",
        Some(serde_json::json!({
            "action": "github.release",
            "tag": tag,
            "owner": github.owner,
            "repo": github.repo,
            "url": url,
            "artifact_count": artifact_paths.len(),
            "generated_notes": generated_notes_ok,
            // The changelog URL embedded in the release body footer, read back
            // from the single body builder so step metadata and the posted body
            // can never disagree (issue #3508).
            "changelog_url": body.changelog_url,
            "notes_start_tag": notes_start_tag,
            // The exact GitHub Release body Homeboy posted, plus a persisted
            // copy on disk, so manual recovery reproduces the identical body
            // without reconstructing it from source (issue #3508).
            "release_body": release_notes,
            "release_body_source": body.source_label(),
            "release_body_file": persisted_notes_path,
        })),
        Vec::new(),
    ))
}

/// The exact GitHub Release body Homeboy posts, with provenance.
///
/// This is the single source of truth for the release body (issue #3508). Every
/// path that needs the body — the live `gh release create`, the persisted notes
/// artifact, the JSON step data, and the repair/recovery commands — reads it
/// from here so an operator never reconstructs a divergent "equivalent" body.
///
/// The body is one of:
/// - GitHub-generated notes with the `**Full Changelog**` footer rewritten to
///   point at the component's changelog URL (`source = GeneratedNotes`), or
/// - the changelog section text from [`ReleaseState::notes`] (or a minimal
///   `Release <tag>` body) with the same changelog footer appended
///   (`source = ChangelogFallback`) when generated notes are unavailable.
#[derive(Debug, Clone)]
pub(crate) struct GitHubReleaseBody {
    /// The exact markdown body passed to `gh release create --notes`.
    pub body: String,
    /// Whether GitHub-generated notes succeeded. `false` means the changelog
    /// fallback body was used.
    pub generated_notes_ok: bool,
    /// The changelog URL embedded in the footer, when one was resolved.
    pub changelog_url: Option<String>,
}

impl GitHubReleaseBody {
    /// Human/JSON-readable label distinguishing the body's provenance so
    /// operators can tell generated notes from the changelog fallback.
    pub(crate) fn source_label(&self) -> &'static str {
        if self.generated_notes_ok {
            "generated-notes"
        } else {
            "changelog-fallback"
        }
    }
}

/// Build the exact GitHub Release body Homeboy will post (issue #3508).
///
/// Distinguishing the four concepts the issue calls out:
/// - *changelog section text* lives in [`ReleaseState::notes`],
/// - *changelog URL* is the `changelog_url` link,
/// - *final GitHub Release body* is what this function returns,
/// - *structured step metadata* is the JSON emitted by the step.
pub(crate) fn build_github_release_body(
    component: &Component,
    github: &GitHubRepo,
    tag: &str,
    state: &ReleaseState,
    changelog_url: Option<&str>,
    notes_start_tag: Option<&str>,
) -> GitHubReleaseBody {
    match github_generated_notes(github, &component.github, tag, notes_start_tag) {
        Ok(generated_notes) => {
            let body = changelog_url
                .map(|url| replace_full_changelog_footer(&generated_notes, url))
                .unwrap_or(generated_notes);
            GitHubReleaseBody {
                body,
                generated_notes_ok: true,
                changelog_url: changelog_url.map(str::to_string),
            }
        }
        Err(err) => {
            log_status!(
                "release",
                "⚠ GitHub generated release notes failed: {} — falling back to changelog notes",
                err
            );
            GitHubReleaseBody {
                body: fallback_release_notes(state, changelog_url, tag),
                generated_notes_ok: false,
                changelog_url: changelog_url.map(str::to_string),
            }
        }
    }
}

/// Persist the exact release body to `build/<tag>-release-notes.md` so it is
/// inspectable after the run and so the repair `--notes-file` reproduces the
/// identical body. Returns the path on success; a write failure is non-fatal
/// (the repair commands fall back to regenerating notes).
fn persist_release_body(component: &Component, tag: &str, body: &str) -> Option<String> {
    let build_dir = std::path::Path::new(&component.local_path).join("build");
    if let Err(err) = std::fs::create_dir_all(&build_dir) {
        log_status!(
            "release",
            "⚠ Could not create build/ to persist release body: {}",
            err
        );
        return None;
    }
    let file = build_dir.join(format!("{}-release-notes.md", safe_filename(tag)));
    match std::fs::write(&file, body) {
        Ok(()) => Some(file.to_string_lossy().replace('\\', "/")),
        Err(err) => {
            log_status!("release", "⚠ Could not persist release body: {}", err);
            None
        }
    }
}

fn github_generated_notes(
    github: &GitHubRepo,
    config: &GithubConfig,
    tag: &str,
    previous_tag: Option<&str>,
) -> Result<String> {
    let endpoint = format!(
        "repos/{}/{}/releases/generate-notes",
        github.owner, github.repo
    );
    let tag_field = format!("tag_name={}", tag);
    let mut args: Vec<&str> = vec!["api", &endpoint, "-f", &tag_field, "--jq", ".body"];
    let previous_field;
    if let Some(previous) = previous_tag {
        previous_field = format!("previous_tag_name={}", previous);
        args.extend_from_slice(&["-f", &previous_field]);
    }

    let output = gh_command(github, config, &args).output().map_err(|e| {
        Error::internal_io(
            format!("Failed to invoke gh: {}", e),
            Some("gh api releases/generate-notes".to_string()),
        )
    })?;

    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "gh api releases/generate-notes failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn github_changelog_url(component: &Component, github: &GitHubRepo, tag: &str) -> Option<String> {
    let changelog_path = changelog::resolve_changelog_path(component).ok()?;
    let local_path = std::path::Path::new(&component.local_path);
    let relative = changelog_path
        .strip_prefix(local_path)
        .unwrap_or(&changelog_path)
        .to_string_lossy()
        .replace('\\', "/");
    Some(format!(
        "https://{}/{}/{}/blob/{}/{}",
        github.host, github.owner, github.repo, tag, relative
    ))
}

pub(super) fn replace_full_changelog_footer(notes: &str, changelog_url: &str) -> String {
    let replacement = format!("**Full Changelog**: {}", changelog_url);
    let mut lines: Vec<&str> = notes.lines().collect();

    if let Some(index) = lines.iter().rposition(|line| {
        line.trim_start()
            .starts_with("**Full Changelog**: https://")
    }) {
        lines[index] = &replacement;
        return lines.join("\n");
    }

    if notes.trim().is_empty() {
        return replacement;
    }

    format!("{}\n\n{}", notes.trim_end(), replacement)
}

fn github_generated_notes_start_tag(component: &Component, tag: &str) -> Result<Option<String>> {
    let monorepo = crate::core::git::MonorepoContext::detect(&component.local_path, &component.id);
    let (git_root, tag_prefix) = match monorepo.as_ref() {
        Some(ctx) => (ctx.git_root.as_str(), Some(ctx.tag_prefix.as_str())),
        None => (component.local_path.as_str(), None),
    };
    crate::core::git::get_previous_tag_before_with_prefix(git_root, tag, tag_prefix)
}

/// Build the release body used when GitHub-generated notes are unavailable.
///
/// Prefer the changelog section captured in [`ReleaseState::notes`]; fall back
/// to a minimal `Release <tag>` body. Either way, append the changelog link
/// footer when we have one so the fallback release still points back at the
/// full changelog.
fn fallback_release_notes(state: &ReleaseState, changelog_url: Option<&str>, tag: &str) -> String {
    let base = state
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|notes| !notes.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Release {}", tag));

    match changelog_url {
        Some(url) => replace_full_changelog_footer(&base, url),
        None => base,
    }
}

/// A successful but no-op result for an idempotent retry where the GitHub
/// Release object already exists. The release exists, so this is `Success`.
pub(super) fn skipped_result(
    tag: &str,
    github: &GitHubRepo,
    reason: &str,
    repair: Option<GitHubReleaseRepairCommands>,
) -> ReleaseStepResult {
    let mut data = serde_json::json!({
        "skipped": true,
        "reason": reason,
        "tag": tag,
        "host": github.host,
        "owner": github.owner,
        "repo": github.repo,
    });
    if let Some(repair) = repair {
        data["fallback_command"] = serde_json::json!(repair.create_command.clone());
        data["repair"] = repair_data(&repair);
    }

    step_success("github.release", "github.release", Some(data), Vec::new())
}

/// The GitHub Release object was NOT created and cannot be recovered in this
/// run (no `gh` binary / not authenticated). This must be `Failed`, not a
/// success-with-`skipped`, so the release pipeline halts before publish/upload
/// steps run against a release that does not exist (issue #3541).
pub(super) fn not_created_result(
    tag: &str,
    github: &GitHubRepo,
    reason: &str,
    error: &str,
    repair: GitHubReleaseRepairCommands,
) -> ReleaseStepResult {
    let data = serde_json::json!({
        "skipped": false,
        "release_created": false,
        "reason": reason,
        "tag": tag,
        "host": github.host,
        "owner": github.owner,
        "repo": github.repo,
        "fallback_command": repair.create_command.clone(),
        "repair": repair_data(&repair),
    });

    step_failed(
        "github.release",
        "github.release",
        Some(data),
        Some(error.to_string()),
        repair_hints(&repair),
    )
}

/// `gh release create` failed, so no GitHub Release object exists. `Failed`,
/// carrying the recovery commands so the operator can finish the release from
/// the already-pushed tag + built artifacts without making a second tag.
pub(super) fn create_failed_result(
    tag: &str,
    github: &GitHubRepo,
    reason: &str,
    stdout: String,
    stderr: String,
    repair: GitHubReleaseRepairCommands,
    body: &GitHubReleaseBody,
    persisted_notes_path: Option<&str>,
) -> ReleaseStepResult {
    let data = serde_json::json!({
        "skipped": false,
        "release_created": false,
        "reason": reason,
        "tag": tag,
        "host": github.host,
        "owner": github.owner,
        "repo": github.repo,
        "stdout": stdout,
        "stderr": stderr.clone(),
        "fallback_command": repair.create_command.clone(),
        "repair": repair_data(&repair),
        // Expose the EXACT body Homeboy attempted to post + its persisted copy
        // so manual recovery reproduces the identical release body (issue #3508).
        "release_body": body.body,
        "release_body_source": body.source_label(),
        "release_body_file": persisted_notes_path,
    });

    let detail = stderr.trim();
    let error = if detail.is_empty() {
        format!("`gh release create` failed for {}", tag)
    } else {
        format!("`gh release create` failed for {}: {}", tag, detail)
    };

    step_failed(
        "github.release",
        "github.release",
        Some(data),
        Some(error),
        repair_hints(&repair),
    )
}

/// The GitHub Release exists but attaching the build artifacts failed. The
/// step is responsible for the full release lifecycle (entry + assets), so a
/// failed asset upload is a `Failed` step: downstream consumers would
/// otherwise assume the assets are present.
pub(super) fn upload_failed_result(
    tag: &str,
    github: &GitHubRepo,
    stdout: String,
    stderr: String,
    artifact_count: usize,
    repair: GitHubReleaseRepairCommands,
) -> ReleaseStepResult {
    let data = serde_json::json!({
        "skipped": false,
        "release_created": true,
        "reason": "gh-upload-failed",
        "tag": tag,
        "host": github.host,
        "owner": github.owner,
        "repo": github.repo,
        "stdout": stdout,
        "stderr": stderr.clone(),
        "artifact_count": artifact_count,
        "repair": repair_data(&repair),
    });

    let detail = stderr.trim();
    let error = if detail.is_empty() {
        format!("`gh release upload` failed for {}", tag)
    } else {
        format!("`gh release upload` failed for {}: {}", tag, detail)
    };

    step_failed(
        "github.release",
        "github.release",
        Some(data),
        Some(error),
        repair_hints(&repair),
    )
}

pub(super) fn upload_success_result(
    tag: &str,
    github: &GitHubRepo,
    artifact_count: usize,
) -> ReleaseStepResult {
    step_success(
        "github.release",
        "github.release",
        Some(serde_json::json!({
            "action": "github.release.upload",
            "tag": tag,
            "host": github.host,
            "owner": github.owner,
            "repo": github.repo,
            "artifact_count": artifact_count,
        })),
        Vec::new(),
    )
}

pub(super) fn gh_is_available() -> bool {
    crate::core::git::gh_probe_succeeds(&["--version"])
}

pub(super) fn gh_is_authenticated(github: &GitHubRepo, config: &GithubConfig) -> bool {
    gh_probe_succeeds(
        github,
        config,
        &["auth", "status", "--hostname", &github.host],
    )
}

pub(super) fn gh_release_exists(
    github: &GitHubRepo,
    config: &GithubConfig,
    tag: &str,
    repo_flag: &str,
) -> bool {
    gh_probe_succeeds(github, config, &["release", "view", tag, "-R", repo_flag])
}

pub(super) fn github_release_repair_commands(
    tag: &str,
    github: &GitHubRepo,
    config: &GithubConfig,
    artifact_paths: &[String],
    previous_tag: Option<&str>,
    persisted_notes_path: Option<&str>,
) -> GitHubReleaseRepairCommands {
    github_release_repair_commands_with_env(
        tag,
        github,
        artifact_paths,
        previous_tag,
        persisted_notes_path,
        github_cli_env(github, config),
    )
}

#[cfg(test)]
pub(super) fn github_release_repair_commands_with_proxy(
    tag: &str,
    github: &GitHubRepo,
    artifact_paths: &[String],
    previous_tag: Option<&str>,
    proxy_hint: Option<&str>,
) -> GitHubReleaseRepairCommands {
    let env = proxy_hint
        .filter(|value| !value.trim().is_empty())
        .map(|proxy| {
            let mut env = Vec::new();
            if github.host != "github.com" {
                env.push(("GH_HOST".to_string(), github.host.clone()));
            }
            env.push(("HTTPS_PROXY".to_string(), proxy.trim().to_string()));
            env
        })
        .unwrap_or_else(|| {
            if github.host != "github.com" {
                vec![("GH_HOST".to_string(), github.host.clone())]
            } else {
                Vec::new()
            }
        });
    github_release_repair_commands_with_env(tag, github, artifact_paths, previous_tag, None, env)
}

fn github_release_repair_commands_with_env(
    tag: &str,
    github: &GitHubRepo,
    artifact_paths: &[String],
    previous_tag: Option<&str>,
    persisted_notes_path: Option<&str>,
    env: Vec<(String, String)>,
) -> GitHubReleaseRepairCommands {
    let env_prefix = gh_env_prefix(&env);
    let repo_flag = format!("{}/{}", github.owner, github.repo);
    // When the exact Homeboy release body was persisted to disk (issue #3508),
    // point recovery at THAT file so a manual `gh release create` reproduces the
    // identical body. Only fall back to regenerating notes into a fresh file
    // when no persisted body exists (gh missing/unauth paths, write failure).
    let exact_body_available = persisted_notes_path.is_some();
    let notes_file = persisted_notes_path
        .map(str::to_string)
        .unwrap_or_else(|| format!("build/{}-release-notes.md", safe_filename(tag)));
    let endpoint = format!(
        "repos/{}/{}/releases/generate-notes",
        github.owner, github.repo
    );
    let mut generate_notes = vec![
        format!("{}gh", env_prefix),
        "api".to_string(),
        shell_quote(&endpoint),
        "-f".to_string(),
        shell_quote(&format!("tag_name={}", tag)),
    ];
    if let Some(previous) = previous_tag {
        generate_notes.push("-f".to_string());
        generate_notes.push(shell_quote(&format!("previous_tag_name={}", previous)));
    }
    generate_notes.push("--jq".to_string());
    generate_notes.push(shell_quote(".body"));
    let regenerate_command = format!(
        "{} > {}",
        generate_notes.join(" "),
        shell_quote(&notes_file)
    );
    // The notes-generation step is only meaningful when there is no persisted
    // exact body. With a persisted body, regenerating would risk a divergent
    // result, so the "generate" step becomes a no-op note that reuses the file.
    let generate_notes_command = if exact_body_available {
        format!(
            "# Exact Homeboy release body already saved at {} — use it as-is",
            notes_file
        )
    } else {
        regenerate_command
    };

    let mut create = vec![
        format!("{}gh", env_prefix),
        "release".to_string(),
        "create".to_string(),
        shell_quote(tag),
        "--title".to_string(),
        shell_quote(tag),
        "--notes-file".to_string(),
        shell_quote(&notes_file),
    ];
    for path in artifact_paths {
        create.push(shell_quote(path));
    }
    create.push("-R".to_string());
    create.push(shell_quote(&repo_flag));

    let view_command = format!(
        "{}gh release view {} -R {}",
        env_prefix,
        shell_quote(tag),
        shell_quote(&repo_flag)
    );
    let env_hint = gh_env_hint(github, &env);

    let notes_guidance = if exact_body_available {
        format!(
            "The exact GitHub Release body Homeboy generated is saved at {}. Create the release straight from it (no regeneration) so the body matches byte-for-byte.",
            notes_file
        )
    } else {
        "Review the generated markdown body in the notes file before creating the release; keep it as the content passed to --notes-file.".to_string()
    };

    GitHubReleaseRepairCommands {
        notes_file,
        notes_guidance,
        generate_notes_command,
        create_command: create.join(" "),
        view_command,
        env_hint,
        exact_body_available,
    }
}

/// Surface the manual recovery commands as step hints so a failed
/// `github.release` step tells the operator exactly how to finish the release
/// from the already-pushed tag + built artifacts without re-tagging.
fn repair_hints(repair: &GitHubReleaseRepairCommands) -> Vec<crate::core::error::Hint> {
    let mut hints = Vec::new();
    if let Some(env_hint) = repair.env_hint.as_deref() {
        hints.push(crate::core::error::Hint {
            message: env_hint.to_string(),
        });
    }
    hints.push(crate::core::error::Hint {
        message: format!("Generate release notes: {}", repair.generate_notes_command),
    });
    hints.push(crate::core::error::Hint {
        message: format!(
            "Create the GitHub Release from the pushed tag and built artifacts (no new tag): {}",
            repair.create_command
        ),
    });
    hints.push(crate::core::error::Hint {
        message: format!("Verify the release exists: {}", repair.view_command),
    });
    hints
}

fn repair_data(repair: &GitHubReleaseRepairCommands) -> serde_json::Value {
    serde_json::json!({
        "notes_file": repair.notes_file,
        "notes_guidance": repair.notes_guidance,
        "generate_notes_command": repair.generate_notes_command,
        "create_command": repair.create_command,
        "view_command": repair.view_command,
        "env_hint": repair.env_hint,
        "exact_body_available": repair.exact_body_available,
    })
}

fn log_repair_commands(repair: &GitHubReleaseRepairCommands) {
    if let Some(hint) = repair.env_hint.as_deref() {
        log_status!("release", "{}", hint);
    }
    log_status!(
        "release",
        "Repair release notes file: {}",
        repair.notes_file
    );
    log_status!("release", "{}", repair.notes_guidance);
    log_status!(
        "release",
        "Generate notes: `{}`",
        repair.generate_notes_command
    );
    log_status!("release", "Create release: `{}`", repair.create_command);
    log_status!("release", "Verify release: `{}`", repair.view_command);
}

fn gh_env_prefix(env: &[(String, String)]) -> String {
    let parts = env
        .iter()
        .filter(|(key, value)| !key.is_empty() && !value.is_empty())
        .map(|(key, value)| format!("{}={}", key, shell_quote(value)))
        .collect::<Vec<_>>();
    if parts.is_empty() {
        String::new()
    } else {
        format!("{} ", parts.join(" "))
    }
}

fn gh_env_hint(github: &GitHubRepo, env: &[(String, String)]) -> Option<String> {
    if github.host == "github.com" && env.is_empty() {
        return None;
    }

    let mut hints = Vec::new();
    let has_proxy = env
        .iter()
        .any(|(key, value)| key.eq_ignore_ascii_case("HTTPS_PROXY") && !value.is_empty());
    if github.host != "github.com" {
        hints.push(format!(
            "GitHub Enterprise host detected: repair commands include GH_HOST={}",
            github.host
        ));
    }
    if has_proxy {
        hints.push("Configured HTTPS_PROXY is included in repair commands.".to_string());
    } else if github.host != "github.com" {
        hints.push(
            "If this Enterprise host requires a proxy, prefix the commands with HTTPS_PROXY=<proxy-url>.".to_string(),
        );
    }

    Some(hints.join(" "))
}

fn safe_filename(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn shell_quote(value: &str) -> String {
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '=' | '@'))
    {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

fn gh_probe_succeeds(github: &GitHubRepo, config: &GithubConfig, args: &[&str]) -> bool {
    gh_command(github, config, args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn gh_command(github: &GitHubRepo, config: &GithubConfig, args: &[&str]) -> std::process::Command {
    let mut command = std::process::Command::new("gh");
    command.args(args);
    for (key, value) in github_cli_env(github, config) {
        command.env(key, value);
    }
    command
}

pub(super) fn github_cli_env(github: &GitHubRepo, config: &GithubConfig) -> Vec<(String, String)> {
    let mut env = Vec::new();
    if github.host != "github.com" {
        env.push(("GH_HOST".to_string(), github.host.clone()));
    }

    let Some(host_config) = config.hosts.get(&github.host) else {
        return env;
    };

    if let Some(proxy) = host_config
        .proxy
        .as_deref()
        .filter(|proxy| !proxy.is_empty())
    {
        env.push(("HTTPS_PROXY".to_string(), proxy.to_string()));
    }

    for (key, value) in &host_config.env {
        if !key.is_empty() && key != "GH_HOST" {
            env.retain(|(existing, _)| existing != key);
            env.push((key.clone(), value.clone()));
        }
    }

    env
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::core::component::{GithubConfig, GithubHostConfig};
    use crate::core::deploy::release_download::GitHubRepo;
    use crate::core::release::types::{ReleaseState, ReleaseStepStatus};

    use super::{
        create_failed_result, fallback_release_notes, github_cli_env,
        github_release_repair_commands, not_created_result, upload_failed_result,
        GitHubReleaseBody,
    };

    fn test_repo() -> GitHubRepo {
        GitHubRepo {
            host: "github.com".to_string(),
            owner: "example-org".to_string(),
            repo: "studio-web".to_string(),
        }
    }

    fn test_repair() -> super::GitHubReleaseRepairCommands {
        github_release_repair_commands(
            "v0.10.6",
            &test_repo(),
            &GithubConfig::default(),
            &["build/studio-web.zip".to_string()],
            None,
            None,
        )
    }

    fn test_body() -> GitHubReleaseBody {
        GitHubReleaseBody {
            body: "## What's Changed\n\n**Full Changelog**: https://example/CHANGELOG.md"
                .to_string(),
            generated_notes_ok: true,
            changelog_url: Some("https://example/CHANGELOG.md".to_string()),
        }
    }

    fn data_str<'a>(result: &'a super::ReleaseStepResult, key: &str) -> Option<&'a str> {
        result
            .data
            .as_ref()
            .and_then(|data| data.get(key))
            .and_then(|value| value.as_str())
    }

    fn data_bool(result: &super::ReleaseStepResult, key: &str) -> Option<bool> {
        result
            .data
            .as_ref()
            .and_then(|data| data.get(key))
            .and_then(|value| value.as_bool())
    }

    #[test]
    fn not_created_result_is_failed_and_not_marked_skipped_success() {
        // Regression for #3541: a release that was never created must NOT be a
        // success-with-skipped step — that lets publish/upload run against a
        // missing release. It must be Failed.
        let result = not_created_result(
            "v0.10.6",
            &test_repo(),
            "gh-not-authenticated",
            "`gh` is not authenticated; GitHub Release was not created.",
            test_repair(),
        );

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert_eq!(data_bool(&result, "skipped"), Some(false));
        assert_eq!(data_bool(&result, "release_created"), Some(false));
        assert_eq!(data_str(&result, "reason"), Some("gh-not-authenticated"));
        assert!(result
            .error
            .as_deref()
            .unwrap()
            .contains("not authenticated"));
        assert!(data_str(&result, "fallback_command").is_some());
        assert!(result
            .hints
            .iter()
            .any(|hint| hint.message.contains("no new tag")));
    }

    #[test]
    fn create_failed_result_reports_generated_notes_failed_as_failure() {
        // The exact scenario from #3541: generated notes failed, the fallback
        // create also failed, so no release object exists. Must be Failed and
        // must carry the generated-notes-failed reason — not success/skipped.
        let result = create_failed_result(
            "v0.10.6",
            &test_repo(),
            "generated-notes-failed",
            String::new(),
            "HTTP 502: bad gateway".to_string(),
            test_repair(),
            &test_body(),
            Some("build/v0.10.6-release-notes.md"),
        );

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert_eq!(data_bool(&result, "skipped"), Some(false));
        assert_eq!(data_bool(&result, "release_created"), Some(false));
        assert_eq!(data_str(&result, "reason"), Some("generated-notes-failed"));
        assert!(result
            .error
            .as_deref()
            .unwrap()
            .contains("`gh release create` failed for v0.10.6"));
        assert!(result
            .error
            .as_deref()
            .unwrap()
            .contains("HTTP 502: bad gateway"));
        assert!(data_str(&result, "fallback_command").is_some());
    }

    #[test]
    fn create_failed_result_reports_plain_create_failure() {
        let result = create_failed_result(
            "v0.10.6",
            &test_repo(),
            "gh-command-failed",
            String::new(),
            "release v0.10.6 already exists".to_string(),
            test_repair(),
            &test_body(),
            Some("build/v0.10.6-release-notes.md"),
        );

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert_eq!(data_str(&result, "reason"), Some("gh-command-failed"));
    }

    #[test]
    fn upload_failed_result_is_failed_but_records_release_exists() {
        // The release object exists but assets did not attach. Still Failed so
        // nothing assumes the assets are present, but release_created stays true.
        let result = upload_failed_result(
            "v0.10.6",
            &test_repo(),
            String::new(),
            "could not upload asset".to_string(),
            1,
            test_repair(),
        );

        assert_eq!(result.status, ReleaseStepStatus::Failed);
        assert_eq!(data_bool(&result, "skipped"), Some(false));
        assert_eq!(data_bool(&result, "release_created"), Some(true));
        assert_eq!(data_str(&result, "reason"), Some("gh-upload-failed"));
        assert!(result
            .error
            .as_deref()
            .unwrap()
            .contains("could not upload asset"));
    }

    #[test]
    fn fallback_release_notes_uses_changelog_notes_when_present() {
        let state = ReleaseState {
            notes: Some("## v0.10.6\n\n- Fixed a thing".to_string()),
            ..Default::default()
        };

        let notes = fallback_release_notes(
            &state,
            Some("https://github.com/example-org/studio-web/blob/v0.10.6/CHANGELOG.md"),
            "v0.10.6",
        );

        assert!(notes.contains("- Fixed a thing"));
        assert!(notes.contains(
            "**Full Changelog**: https://github.com/example-org/studio-web/blob/v0.10.6/CHANGELOG.md"
        ));
    }

    #[test]
    fn fallback_release_notes_falls_back_to_minimal_body_when_empty() {
        let state = ReleaseState {
            notes: Some("   ".to_string()),
            ..Default::default()
        };

        let notes = fallback_release_notes(&state, None, "v0.10.6");

        assert_eq!(notes, "Release v0.10.6");
    }

    // ---- Issue #3508: the exact GitHub Release body must be discoverable ----

    #[test]
    fn release_body_source_label_distinguishes_generated_from_fallback() {
        let generated = GitHubReleaseBody {
            body: "x".to_string(),
            generated_notes_ok: true,
            changelog_url: None,
        };
        let fallback = GitHubReleaseBody {
            body: "x".to_string(),
            generated_notes_ok: false,
            changelog_url: None,
        };
        assert_eq!(generated.source_label(), "generated-notes");
        assert_eq!(fallback.source_label(), "changelog-fallback");
    }

    #[test]
    fn create_failed_result_exposes_exact_release_body_and_persisted_file() {
        // Regression for #3508: a failed create must surface the EXACT body
        // Homeboy attempted to post plus its persisted-file path so manual
        // recovery reproduces the identical body instead of reconstructing it.
        let body = test_body();
        let result = create_failed_result(
            "v0.10.6",
            &test_repo(),
            "generated-notes-failed",
            String::new(),
            "HTTP 502".to_string(),
            test_repair(),
            &body,
            Some("build/v0.10.6-release-notes.md"),
        );

        assert_eq!(data_str(&result, "release_body"), Some(body.body.as_str()));
        assert_eq!(
            data_str(&result, "release_body_source"),
            Some("generated-notes")
        );
        assert_eq!(
            data_str(&result, "release_body_file"),
            Some("build/v0.10.6-release-notes.md")
        );
        // The exact body must carry the changelog link footer.
        assert!(data_str(&result, "release_body")
            .unwrap()
            .contains("**Full Changelog**:"));
    }

    #[test]
    fn repair_commands_reuse_persisted_exact_body_when_available() {
        // With the persisted exact body, recovery must `--notes-file` THAT file
        // and must NOT regenerate notes (which could diverge) — issue #3508.
        let repair = github_release_repair_commands(
            "v0.10.6",
            &test_repo(),
            &GithubConfig::default(),
            &["build/studio-web.zip".to_string()],
            None,
            Some("build/v0.10.6-release-notes.md"),
        );

        assert!(repair.exact_body_available);
        assert_eq!(repair.notes_file, "build/v0.10.6-release-notes.md");
        assert!(repair
            .create_command
            .contains("--notes-file build/v0.10.6-release-notes.md"));
        // The generate step must not re-run note generation against the API.
        assert!(!repair.generate_notes_command.contains("generate-notes"));
        assert!(repair.notes_guidance.contains("byte-for-byte"));
    }

    #[test]
    fn repair_commands_regenerate_notes_when_no_persisted_body() {
        // Without a persisted body (gh missing / unauth), recovery falls back to
        // regenerating notes into a fresh file.
        let repair = github_release_repair_commands(
            "v0.10.6",
            &test_repo(),
            &GithubConfig::default(),
            &["build/studio-web.zip".to_string()],
            None,
            None,
        );

        assert!(!repair.exact_body_available);
        assert!(repair.generate_notes_command.contains("generate-notes"));
        assert!(repair
            .create_command
            .contains("--notes-file build/v0.10.6-release-notes.md"));
    }

    #[test]
    fn github_cli_env_sets_enterprise_host_and_proxy() {
        let github = GitHubRepo {
            host: "github.enterprise.test".to_string(),
            owner: "owner".to_string(),
            repo: "repo".to_string(),
        };
        let config = GithubConfig {
            hosts: HashMap::from([(
                "github.enterprise.test".to_string(),
                GithubHostConfig {
                    proxy: Some("socks5://127.0.0.1:9999".to_string()),
                    env: HashMap::new(),
                },
            )]),
        };

        let env = github_cli_env(&github, &config);

        assert_eq!(
            env,
            vec![
                ("GH_HOST".to_string(), "github.enterprise.test".to_string()),
                (
                    "HTTPS_PROXY".to_string(),
                    "socks5://127.0.0.1:9999".to_string()
                ),
            ]
        );
    }

    #[test]
    fn github_cli_env_allows_explicit_host_env_override() {
        let github = GitHubRepo {
            host: "github.enterprise.test".to_string(),
            owner: "owner".to_string(),
            repo: "repo".to_string(),
        };
        let config = GithubConfig {
            hosts: HashMap::from([(
                "github.enterprise.test".to_string(),
                GithubHostConfig {
                    proxy: Some("socks5://127.0.0.1:9999".to_string()),
                    env: HashMap::from([(
                        "HTTPS_PROXY".to_string(),
                        "https://proxy.example.test:8443".to_string(),
                    )]),
                },
            )]),
        };

        let env = github_cli_env(&github, &config);

        assert!(env.contains(&("GH_HOST".to_string(), "github.enterprise.test".to_string())));
        assert!(env.contains(&(
            "HTTPS_PROXY".to_string(),
            "https://proxy.example.test:8443".to_string()
        )));
        assert!(!env.contains(&(
            "HTTPS_PROXY".to_string(),
            "socks5://127.0.0.1:9999".to_string()
        )));
    }
}
