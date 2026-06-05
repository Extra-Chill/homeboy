//! GitHub Release helper result builders and probes.

use crate::core::component::Component;
use crate::core::component::GithubConfig;
use crate::core::deploy::release_download::GitHubRepo;
use crate::core::error::{Error, Result};
use crate::core::release::changelog;
use crate::core::release::types::{ReleaseState, ReleaseStepResult};

use super::step_success;

#[derive(Debug, Clone)]
pub(super) struct GitHubReleaseRepairCommands {
    pub notes_file: String,
    pub notes_guidance: String,
    pub generate_notes_command: String,
    pub create_command: String,
    pub view_command: String,
    pub env_hint: Option<String>,
}

/// Create a GitHub Release for the just-pushed tag. Fails soft in every
/// plausible failure mode (no `gh` binary, not authenticated, release already
/// exists, `gh release create` errors) — the tag is already pushed by the
/// time this runs and we don't want to mark an otherwise-successful release
/// as failed.
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

    if !gh_is_available() {
        let repair =
            github_release_repair_commands(&tag, &github, &component.github, &artifact_paths, None);
        log_status!(
            "release",
            "⚠ `gh` CLI not found on PATH — skipping GitHub Release creation"
        );
        log_repair_commands(&repair);
        return Ok(skipped_result(
            &tag,
            &github,
            "gh-not-available",
            Some(repair),
        ));
    }

    if !gh_is_authenticated(&github, &component.github) {
        let repair =
            github_release_repair_commands(&tag, &github, &component.github, &artifact_paths, None);
        log_status!(
            "release",
            "⚠ `gh` is not authenticated — skipping GitHub Release creation"
        );
        log_status!("release", "Authenticate with `gh auth login`, then run:");
        log_repair_commands(&repair);
        return Ok(skipped_result(
            &tag,
            &github,
            "gh-not-authenticated",
            Some(repair),
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
            log_status!("release", "⚠ `gh release upload` failed: {}", stderr.trim());
            return Ok(upload_failed_result(
                &tag,
                &github,
                stdout,
                stderr,
                artifact_paths.len(),
            ));
        }

        return Ok(upload_success_result(&tag, &github, artifact_paths.len()));
    }

    let notes_start_tag = github_generated_notes_start_tag(component, &tag)?;
    let generated_notes = match github_generated_notes(
        &github,
        &component.github,
        &tag,
        notes_start_tag.as_deref(),
    ) {
        Ok(notes) => notes,
        Err(err) => {
            let repair = github_release_repair_commands(
                &tag,
                &github,
                &component.github,
                &artifact_paths,
                None,
            );
            log_status!(
                "release",
                "⚠ GitHub generated release notes failed: {}",
                err
            );
            log_repair_commands(&repair);
            return Ok(skipped_result(
                &tag,
                &github,
                "generated-notes-failed",
                Some(repair),
            ));
        }
    };
    let changelog_url = github_changelog_url(component, &github, &tag);
    let release_notes = changelog_url
        .as_deref()
        .map(|url| replace_full_changelog_footer(&generated_notes, url))
        .unwrap_or(generated_notes);

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
    if let Some(previous_tag) = notes_start_tag.as_deref() {
        create_args.extend_from_slice(&["--notes-start-tag", previous_tag]);
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
        let repair = github_release_repair_commands(
            &tag,
            &github,
            &component.github,
            &artifact_paths,
            notes_start_tag.as_deref(),
        );
        log_status!("release", "⚠ `gh release create` failed: {}", stderr.trim());
        log_repair_commands(&repair);
        return Ok(step_success(
            "github.release",
            "github.release",
            Some(serde_json::json!({
                "skipped": true,
                "reason": "gh-command-failed",
                "tag": tag,
                "owner": github.owner,
                "repo": github.repo,
                "stdout": stdout,
                "stderr": stderr,
                "fallback_command": repair.create_command.clone(),
                "repair": repair_data(&repair),
            })),
            Vec::new(),
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
            "generated_notes": true,
            "changelog_url": changelog_url,
            "notes_start_tag": notes_start_tag,
        })),
        Vec::new(),
    ))
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

pub(super) fn upload_failed_result(
    tag: &str,
    github: &GitHubRepo,
    stdout: String,
    stderr: String,
    artifact_count: usize,
) -> ReleaseStepResult {
    step_success(
        "github.release",
        "github.release",
        Some(serde_json::json!({
            "skipped": true,
            "reason": "gh-upload-failed",
            "tag": tag,
            "host": github.host,
            "owner": github.owner,
            "repo": github.repo,
            "stdout": stdout,
            "stderr": stderr,
            "artifact_count": artifact_count,
        })),
        Vec::new(),
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
) -> GitHubReleaseRepairCommands {
    github_release_repair_commands_with_env(
        tag,
        github,
        artifact_paths,
        previous_tag,
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
    github_release_repair_commands_with_env(tag, github, artifact_paths, previous_tag, env)
}

fn github_release_repair_commands_with_env(
    tag: &str,
    github: &GitHubRepo,
    artifact_paths: &[String],
    previous_tag: Option<&str>,
    env: Vec<(String, String)>,
) -> GitHubReleaseRepairCommands {
    let env_prefix = gh_env_prefix(&env);
    let repo_flag = format!("{}/{}", github.owner, github.repo);
    let notes_file = format!("build/{}-release-notes.md", safe_filename(tag));
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
    let generate_notes_command = format!(
        "{} > {}",
        generate_notes.join(" "),
        shell_quote(&notes_file)
    );

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

    GitHubReleaseRepairCommands {
        notes_file,
        notes_guidance: "Review the generated markdown body in the notes file before creating the release; keep it as the content passed to --notes-file.".to_string(),
        generate_notes_command,
        create_command: create.join(" "),
        view_command,
        env_hint,
    }
}

fn repair_data(repair: &GitHubReleaseRepairCommands) -> serde_json::Value {
    serde_json::json!({
        "notes_file": repair.notes_file,
        "notes_guidance": repair.notes_guidance,
        "generate_notes_command": repair.generate_notes_command,
        "create_command": repair.create_command,
        "view_command": repair.view_command,
        "env_hint": repair.env_hint,
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

    use super::github_cli_env;

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
