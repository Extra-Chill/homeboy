//! Manual-recovery command builders, hints, and logging for failed releases.

use crate::core::component::GithubConfig;
use crate::core::deploy::release_download::GitHubRepo;

use super::gh_cli::{gh_env_hint, gh_env_prefix, github_cli_env, safe_filename, shell_quote};

#[derive(Debug, Clone)]
pub(crate) struct GitHubReleaseRepairCommands {
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

pub(crate) fn github_release_repair_commands(
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
pub(crate) fn github_release_repair_commands_with_proxy(
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
pub(super) fn repair_hints(repair: &GitHubReleaseRepairCommands) -> Vec<crate::core::error::Hint> {
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

pub(super) fn repair_data(repair: &GitHubReleaseRepairCommands) -> serde_json::Value {
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

pub(super) fn log_repair_commands(repair: &GitHubReleaseRepairCommands) {
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

pub(crate) fn gh_auth_failure_message(
    github: &GitHubRepo,
    repair: &GitHubReleaseRepairCommands,
) -> String {
    if github.host == "github.com" {
        return "`gh` is not authenticated; GitHub Release was not created.".to_string();
    }

    let proxy_context = repair
        .env_hint
        .as_deref()
        .filter(|hint| hint.contains("Proxy environment is included"))
        .map(|_| " with the inherited/configured proxy environment")
        .unwrap_or("");

    format!(
        "`gh auth status --hostname {}` failed{}; GitHub Release was not created. Authenticate this host with `gh auth login --hostname {}`, or verify the proxy/keyring environment used by the printed repair commands.",
        github.host, proxy_context, github.host
    )
}
