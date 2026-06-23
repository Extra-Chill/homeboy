//! Release-notes body construction, generated-notes probes, and footer rewrites.

use crate::core::component::Component;
use crate::core::component::GithubConfig;
use crate::core::deploy::release_download::GitHubRepo;
use crate::core::error::{Error, Result};
use crate::core::release::changelog;
use crate::core::release::types::ReleaseState;

use super::gh_cli::{gh_command, safe_filename};

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
pub(super) fn persist_release_body(component: &Component, tag: &str, body: &str) -> Option<String> {
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

pub(crate) fn github_changelog_url(
    component: &Component,
    github: &GitHubRepo,
    tag: &str,
) -> Option<String> {
    let changelog_path = changelog::resolve_changelog_path(component).ok()?;
    let local_path = std::path::Path::new(&component.local_path);
    let component_relative = changelog_path
        .strip_prefix(local_path)
        .unwrap_or(&changelog_path)
        .to_string_lossy()
        .replace('\\', "/");

    // The release URL is anchored at the repository root, but the changelog
    // path above is relative to the component directory. For a component that
    // lives in a monorepo subdirectory (e.g. `php-transformer`), prepend the
    // component's path prefix so the link points at the real file
    // (`php-transformer/CHANGELOG.md`) instead of a root-level `CHANGELOG.md`
    // that does not exist (issue #6146).
    let repo_relative =
        match crate::core::git::MonorepoContext::detect(&component.local_path, &component.id) {
            Some(ctx) => {
                let prefix = ctx.path_prefix.replace('\\', "/");
                let prefix = prefix.trim_matches('/');
                if prefix.is_empty() {
                    component_relative
                } else {
                    format!("{}/{}", prefix, component_relative)
                }
            }
            None => component_relative,
        };

    Some(format!(
        "https://{}/{}/{}/blob/{}/{}",
        github.host, github.owner, github.repo, tag, repo_relative
    ))
}

pub(crate) fn replace_full_changelog_footer(notes: &str, changelog_url: &str) -> String {
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

pub(crate) fn github_generated_notes_start_tag(
    component: &Component,
    tag: &str,
) -> Result<Option<String>> {
    let monorepo = crate::core::git::MonorepoContext::detect(&component.local_path, &component.id);
    let (git_root, tag_prefix) = match monorepo.as_ref() {
        Some(ctx) => (ctx.git_root.as_str(), Some(ctx.tag_prefix.as_str())),
        None => (component.local_path.as_str(), None),
    };
    let previous =
        crate::core::git::get_previous_tag_before_any_with_prefix(git_root, tag, tag_prefix)?;

    if let Some(previous_tag) = previous.as_deref() {
        if !crate::core::git::is_ancestor(git_root, previous_tag, tag)? {
            return Err(Error::validation_invalid_argument(
                "release-notes-range",
                format!(
                    "Previous release tag {} is not reachable from release tag {}. Refusing to generate GitHub release notes because using an older reachable tag would duplicate prior release ranges.",
                    previous_tag, tag
                ),
                Some(format!("Repository: {}", git_root)),
                Some(vec![
                    format!(
                        "Merge or recover the {} release commit onto the selected release base/default branch, then rerun the release.",
                        previous_tag
                    ),
                    format!(
                        "Inspect the boundary: git merge-base --is-ancestor {} {}",
                        previous_tag, tag
                    ),
                ]),
            ));
        }
    }

    Ok(previous)
}

pub(crate) fn github_release_notes_start_tag(component: &Component, tag: &str) -> Option<String> {
    match github_generated_notes_start_tag(component, tag) {
        Ok(notes_start_tag) => notes_start_tag,
        Err(err) => {
            log_status!(
                "release",
                "GitHub-generated release notes unavailable for {}: {}. Falling back to Homeboy release notes.",
                tag,
                err
            );
            None
        }
    }
}

/// Build the release body used when GitHub-generated notes are unavailable.
///
/// Prefer the changelog section captured in [`ReleaseState::notes`]; fall back
/// to a minimal `Release <tag>` body. Either way, append the changelog link
/// footer when we have one so the fallback release still points back at the
/// full changelog.
pub(crate) fn fallback_release_notes(
    state: &ReleaseState,
    changelog_url: Option<&str>,
    tag: &str,
) -> String {
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
