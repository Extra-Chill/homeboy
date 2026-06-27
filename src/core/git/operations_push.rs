use serde::Deserialize;

use crate::core::config::read_json_spec_to_string;
use crate::core::error::{Error, Result};
use crate::core::output::BulkResult;

use super::operation_output::{run_bulk_ids, GitOutput};
use super::{execute_git, resolve_target};

#[derive(Debug, Deserialize)]
struct PushBulkInput {
    component_ids: Vec<String>,
    #[serde(default)]
    tags: bool,
    #[serde(default)]
    force_with_lease: bool,
    #[serde(default)]
    remote_url: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    refspec: Option<String>,
    #[serde(default)]
    strip_extraheader: bool,
}

/// Options for [`push`].
#[derive(Debug, Clone, Default)]
pub struct PushOptions {
    /// Push tags as well (`--follow-tags`).
    pub tags: bool,
    /// Use `--force-with-lease` for safe force-pushes (e.g. after a rebase).
    /// Deliberately the only force flavour exposed — never plain `--force`.
    pub force_with_lease: bool,
    /// Push to a remote URL directly instead of the configured upstream.
    pub remote_url: Option<String>,
    /// GitHub App/user token injected into `remote_url` for this invocation.
    pub token: Option<String>,
    /// Explicit source/destination refspec, e.g. `HEAD:refs/heads/branch`.
    pub refspec: Option<String>,
    /// Clear the GitHub Actions checkout extraheader so URL auth wins.
    pub strip_extraheader: bool,
}

/// Push local commits for a component.
pub fn push(component_id: Option<&str>, options: PushOptions) -> Result<GitOutput> {
    push_at(component_id, options, None)
}

/// Like [`push`] but with an explicit path override for git operations.
pub fn push_at(
    component_id: Option<&str>,
    options: PushOptions,
    path_override: Option<&str>,
) -> Result<GitOutput> {
    let (id, path) = resolve_target(component_id, path_override)?;
    let remote_url =
        resolve_push_remote_url(options.remote_url.as_deref(), options.token.as_deref())?;
    let mut args: Vec<String> = Vec::new();
    if options.strip_extraheader {
        // Clear the auth header injected by actions/checkout so URL auth wins.
        // The header key is host-specific; derive it from the remote URL so
        // GitHub Enterprise remotes are stripped correctly, not just github.com.
        let host = options
            .remote_url
            .as_deref()
            .and_then(github_https_host)
            .unwrap_or_else(|| "github.com".to_string());
        args.push("-c".to_string());
        args.push(format!("http.https://{host}/.extraheader="));
    }
    args.push("push".to_string());
    if options.tags {
        args.push("--follow-tags".to_string());
    }
    if options.force_with_lease {
        args.push("--force-with-lease".to_string());
    }
    if let Some(remote) = remote_url {
        args.push(remote);
    } else if options.refspec.is_some() {
        args.push(super::resolve_default_remote(std::path::Path::new(&path)));
    }
    if let Some(refspec) = options.refspec {
        args.push(refspec);
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output =
        execute_git(&path, &arg_refs).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "push", output))
}

/// Host of an `https://` GitHub remote URL (github.com or Enterprise), if the
/// URL is an HTTPS GitHub URL. Returns `None` for SSH or non-GitHub URLs.
fn github_https_host(url: &str) -> Option<String> {
    if !url.starts_with("https://") {
        return None;
    }
    crate::core::deploy::release_download::parse_github_url(url).map(|repo| repo.host)
}

fn resolve_push_remote_url(
    remote_url: Option<&str>,
    token: Option<&str>,
) -> Result<Option<String>> {
    match (remote_url, token) {
        (Some(url), Some(token)) => {
            if github_https_host(url).is_none() {
                return Err(Error::validation_invalid_argument(
                    "token",
                    "--token requires --remote-url to be an https GitHub URL (e.g. https://github.com/owner/repo or https://github.a8c.com/owner/repo)",
                    None,
                    None,
                ));
            }
            Ok(Some(format!(
                "https://x-access-token:{}@{}",
                token,
                &url["https://".len()..]
            )))
        }
        (Some(url), None) => Ok(Some(url.to_string())),
        (None, Some(_)) => Err(Error::validation_invalid_argument(
            "token",
            "--token requires --remote-url",
            None,
            None,
        )),
        (None, None) => Ok(None),
    }
}

/// Push multiple components from JSON spec.
pub fn push_bulk(json_spec: &str) -> Result<BulkResult<GitOutput>> {
    let raw = read_json_spec_to_string(json_spec)?;
    let input: PushBulkInput = serde_json::from_str(&raw).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some("parse bulk push input".to_string()),
            Some(raw.chars().take(200).collect::<String>()),
        )
    })?;
    let push_tags = input.tags;
    let force_with_lease = input.force_with_lease;
    let remote_url = input.remote_url;
    let token = input.token;
    let refspec = input.refspec;
    let strip_extraheader = input.strip_extraheader;
    Ok(run_bulk_ids(&input.component_ids, "push", |id| {
        push(
            Some(id),
            PushOptions {
                tags: push_tags,
                force_with_lease,
                remote_url: remote_url.clone(),
                token: token.clone(),
                refspec: refspec.clone(),
                strip_extraheader,
            },
        )
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_https_host_parses_github_com_and_enterprise() {
        assert_eq!(
            github_https_host("https://github.com/owner/repo.git").as_deref(),
            Some("github.com")
        );
        assert_eq!(
            github_https_host("https://github.a8c.com/owner/repo").as_deref(),
            Some("github.a8c.com")
        );
    }

    #[test]
    fn github_https_host_rejects_ssh_and_non_github() {
        assert_eq!(github_https_host("git@github.com:owner/repo.git"), None);
        assert_eq!(github_https_host("https://example.com/owner/repo"), None);
    }

    #[test]
    fn token_push_injects_token_for_enterprise_https_remote() {
        let url = resolve_push_remote_url(Some("https://github.a8c.com/owner/repo.git"), Some("tok"))
            .expect("enterprise https remote should be accepted");
        assert_eq!(
            url.as_deref(),
            Some("https://x-access-token:tok@github.a8c.com/owner/repo.git")
        );
    }

    #[test]
    fn token_push_rejects_ssh_remote() {
        assert!(resolve_push_remote_url(Some("git@github.com:owner/repo.git"), Some("tok")).is_err());
    }
}
