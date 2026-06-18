use crate::core::error::{Error, Result};

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
