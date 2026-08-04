use homeboy_core::error::{Error, Result};

const PRIVATE_PROXIED_SOURCE_HOSTS_ENV: &str = "HOMEBOY_PRIVATE_PROXIED_SOURCE_HOSTS";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SourceMaterializationPolicy {
    pub private_proxied_source_hosts: Vec<String>,
}

impl SourceMaterializationPolicy {
    pub(super) fn from_env() -> Self {
        Self {
            private_proxied_source_hosts: split_env_list(PRIVATE_PROXIED_SOURCE_HOSTS_ENV)
                .into_iter()
                .map(|host| host.to_ascii_lowercase())
                .collect(),
        }
    }
}

pub(super) fn validate_runner_git_materialization(remote_url: &str, runner_id: &str) -> Result<()> {
    let policy = SourceMaterializationPolicy::from_env();
    validate_runner_git_materialization_with_policy(remote_url, runner_id, &policy)
}

/// A declared Lab stack has no controller checkout to fall back to, so its
/// repository transport must be explicit, encrypted, and free of credentials.
pub(super) fn validate_lab_stack_repository(remote_url: &str, runner_id: &str) -> Result<()> {
    validate_sanitized_git_remote(remote_url)?;
    let remote = remote_url.trim();
    let supported_transport = remote.starts_with("https://")
        || remote.starts_with("ssh://")
        || remote.starts_with("git@");
    if supported_transport {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "lab_stack.repository",
        "Lab stack repository must use sanitized HTTPS or SSH transport",
        Some(runner_id.to_string()),
        Some(vec![
            "Use a credential-free HTTPS or SSH repository URL. Lab fetches only with runner-owned credentials and never receives controller Git credentials.".to_string(),
        ]),
    ))
}

/// A remote URL is restored as runner metadata after bundle materialization.
/// SSH URI usernames are valid, but passwords and non-SSH URI userinfo must
/// not be persisted in the checkout metadata.
pub(super) fn validate_sanitized_git_remote(remote_url: &str) -> Result<()> {
    let value = remote_url.trim();
    let has_query_or_fragment = value.contains('?') || value.contains('#');
    let unsafe_uri_userinfo = value
        .split_once("://")
        .map(|(scheme, remainder)| {
            remainder
                .split(['/', '?', '#'])
                .next()
                .and_then(|authority| authority.split_once('@'))
                .is_some_and(|(userinfo, _)| {
                    userinfo.contains(':') || !scheme.eq_ignore_ascii_case("ssh")
                })
        })
        .unwrap_or(false);
    if !has_query_or_fragment && !unsafe_uri_userinfo {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "remote.origin.url",
        "git workspace sync refuses credential-bearing repository URL data in URI userinfo, query, or fragment",
        None,
        Some(vec![
            "Configure origin with a sanitized HTTPS or SSH URL; Homeboy never transfers Git credentials to a runner.".to_string(),
        ]),
    ))
}

fn validate_runner_git_materialization_with_policy(
    remote_url: &str,
    runner_id: &str,
    policy: &SourceMaterializationPolicy,
) -> Result<()> {
    if let Some(host) = private_proxied_source_host(remote_url, policy) {
        return Err(private_proxied_source_error(
            "mode",
            &host,
            runner_id,
            "--mode git would fetch a private/proxied source on the runner; use controller-routed workspace sync",
        ));
    }

    Ok(())
}

pub(super) fn requires_controller_routed_workspace_sync(remote_url: &str) -> bool {
    let policy = SourceMaterializationPolicy::from_env();
    requires_controller_routed_workspace_sync_with_policy(remote_url, &policy)
}

pub(super) fn requires_controller_routed_workspace_sync_with_policy(
    remote_url: &str,
    policy: &SourceMaterializationPolicy,
) -> bool {
    private_proxied_source_host(remote_url, policy).is_some()
}

pub(super) fn validate_runner_exec_source_fetch(command: &[String], runner_id: &str) -> Result<()> {
    let policy = SourceMaterializationPolicy::from_env();
    validate_runner_exec_source_fetch_with_policy(command, runner_id, &policy)
}

fn validate_runner_exec_source_fetch_with_policy(
    command: &[String],
    runner_id: &str,
    policy: &SourceMaterializationPolicy,
) -> Result<()> {
    if !looks_like_git_fetch_command(command) {
        return Ok(());
    }

    if let Some(host) = command
        .iter()
        .find_map(|arg| private_proxied_source_host(arg, policy))
    {
        return Err(private_proxied_source_error(
            "command",
            &host,
            runner_id,
            "runner-side Git fetch for a private/proxied source is not allowed; use controller-routed workspace sync",
        ));
    }

    Ok(())
}

fn private_proxied_source_error(field: &str, host: &str, runner_id: &str, problem: &str) -> Error {
    Error::validation_invalid_argument(
        field,
        format!("{problem}: `{host}`"),
        Some(runner_id.to_string()),
        Some(vec![
            "Keep authenticated or proxy-dependent Git operations on the controller machine."
                .to_string(),
            "Materialize the controller checkout with `homeboy runner workspace sync <runner-id> --path <local-worktree> --mode snapshot`.".to_string(),
            "Use the returned `remote_path` as the runner command cwd/path.".to_string(),
            format!(
                "Override the private/proxied host list with `{PRIVATE_PROXIED_SOURCE_HOSTS_ENV}` only when the runner is explicitly allowed to fetch those sources."
            ),
        ]),
    )
}

fn looks_like_git_fetch_command(command: &[String]) -> bool {
    let joined = command.join(" ");
    let lower = joined.to_ascii_lowercase();

    lower.contains("git clone")
        || lower.contains("git fetch")
        || lower.contains("git pull")
        || lower.contains("git ls-remote")
}

fn private_proxied_source_host(
    value: &str,
    policy: &SourceMaterializationPolicy,
) -> Option<String> {
    let value = value.trim();
    policy
        .private_proxied_source_hosts
        .iter()
        .find(|host| remote_matches_host(value, host))
        .cloned()
}

fn split_env_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .ok()
        .into_iter()
        .flat_map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn remote_matches_host(value: &str, host: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let host = host.trim().trim_start_matches('.');

    lower == host
        || lower.contains(&format!("@{host}:"))
        || lower.contains(&format!("@{host}/"))
        || lower.contains(&format!("//{host}/"))
        || lower.contains(&format!("//{host}:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_product_neutral() {
        let policy = SourceMaterializationPolicy::default();

        assert_eq!(
            private_proxied_source_host("git@github.example.com:example-org/example.git", &policy),
            None
        );
    }

    #[test]
    fn detects_configured_private_proxied_source_hosts() {
        let policy = SourceMaterializationPolicy {
            private_proxied_source_hosts: vec!["github.example.com".to_string()],
        };

        assert_eq!(
            private_proxied_source_host("git@github.example.com:example-org/example.git", &policy),
            Some("github.example.com".to_string())
        );
        assert_eq!(
            private_proxied_source_host(
                "https://github.example.com/example-org/example.git",
                &policy
            ),
            Some("github.example.com".to_string())
        );
        assert_eq!(
            private_proxied_source_host("https://github.com/Extra-Chill/homeboy.git", &policy),
            None
        );
    }

    #[test]
    fn rejects_runner_side_private_proxied_git_materialization() {
        let policy = SourceMaterializationPolicy {
            private_proxied_source_hosts: vec!["github.example.com".to_string()],
        };
        let err = validate_runner_git_materialization_with_policy(
            "git@github.example.com:example-org/example.git",
            "homeboy-lab",
            &policy,
        )
        .expect_err("private/proxied runner-side git materialization should fail");

        assert!(err.message.contains("--mode git"));
        assert!(err.message.contains("github.example.com"));
        assert!(err.message.contains("workspace sync"));
    }

    #[test]
    fn rejects_credential_bearing_https_remote_without_echoing_the_secret() {
        let remote = "https://token-value@github.example.com/example/private.git";
        let error = validate_sanitized_git_remote(remote).expect_err("credential URL is unsafe");

        assert!(error.message.contains("credential-bearing"));
        assert!(!error.message.contains("token-value"));
        assert!(!error.details.to_string().contains("token-value"));
    }

    #[test]
    fn accepts_credential_free_uri_usernames_and_scp_style_ssh() {
        for remote in [
            "ssh://git@example.org/org/repo.git",
            "git@example.org:org/repo.git",
        ] {
            validate_sanitized_git_remote(remote).expect("credential-free repository URL");
        }
    }

    #[test]
    fn rejects_password_bearing_uri_userinfo_without_echoing_the_secret() {
        for remote in [
            "https://example.test/repo.git?access_token=secret-value",
            "https://example.test/repo.git#secret-value",
            "ssh://git:secret-value@example.test/repo.git",
        ] {
            let error = validate_sanitized_git_remote(remote).expect_err("unsafe repository URL");
            assert!(!error.to_string().contains("secret-value"));
            assert!(!error.details.to_string().contains("secret-value"));
        }
    }

    #[test]
    fn lab_stack_requires_encrypted_credential_free_repository_transport() {
        validate_lab_stack_repository("git@github.example.com:example/private.git", "lab")
            .expect("SSH transport is supported");
        let error = validate_lab_stack_repository("file:///tmp/private.git", "lab")
            .expect_err("filesystem transport bypasses Lab network policy");
        assert!(error.message.contains("sanitized HTTPS or SSH"));
    }

    #[test]
    fn identifies_sources_that_need_controller_routed_workspace_sync() {
        let policy = SourceMaterializationPolicy {
            private_proxied_source_hosts: vec!["github.example.com".to_string()],
        };
        assert!(requires_controller_routed_workspace_sync_with_policy(
            "git@github.example.com:example-org/example.git",
            &policy
        ));
        assert!(!requires_controller_routed_workspace_sync_with_policy(
            "https://github.com/Extra-Chill/homeboy.git",
            &policy
        ));
    }

    #[test]
    fn rejects_runner_exec_private_proxied_git_fetches() {
        let policy = SourceMaterializationPolicy {
            private_proxied_source_hosts: vec!["github.example.com".to_string()],
        };
        let err = validate_runner_exec_source_fetch_with_policy(
            &[
                "sh".to_string(),
                "-c".to_string(),
                "git clone git@github.example.com:example-org/example.git".to_string(),
            ],
            "homeboy-lab",
            &policy,
        )
        .expect_err("private/proxied runner-side git clone should fail");

        assert!(err.message.contains("runner-side Git fetch"));
        assert!(err.message.contains("github.example.com"));
        assert!(err.message.contains("workspace sync"));
    }
}
