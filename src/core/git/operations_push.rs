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
        args.push("-c".to_string());
        args.push("http.https://github.com/.extraheader=".to_string());
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
        args.push("origin".to_string());
    }
    if let Some(refspec) = options.refspec {
        args.push(refspec);
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output =
        execute_git(&path, &arg_refs).map_err(|e| Error::git_command_failed(e.to_string()))?;
    Ok(GitOutput::from_output(id, path, "push", output))
}

fn resolve_push_remote_url(
    remote_url: Option<&str>,
    token: Option<&str>,
) -> Result<Option<String>> {
    match (remote_url, token) {
        (Some(url), Some(token)) => {
            if !url.starts_with("https://github.com/") {
                return Err(Error::validation_invalid_argument(
                    "token",
                    "--token requires --remote-url to start with https://github.com/",
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
