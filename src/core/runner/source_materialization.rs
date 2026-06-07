use crate::core::error::{Error, Result};

const PRIVATE_PROXIED_SOURCE_HOSTS_ENV: &str = "HOMEBOY_PRIVATE_PROXIED_SOURCE_HOSTS";
const DEFAULT_PRIVATE_PROXIED_SOURCE_HOSTS: &[&str] = &["github.a8c.com"];

pub(super) fn validate_runner_git_materialization(remote_url: &str, runner_id: &str) -> Result<()> {
    if let Some(host) = private_proxied_source_host(remote_url) {
        return Err(private_proxied_source_error(
            "mode",
            &host,
            runner_id,
            "--mode git would fetch a private/proxied source on the runner; use controller-routed workspace sync",
        ));
    }

    Ok(())
}

pub(super) fn validate_runner_exec_source_fetch(command: &[String], runner_id: &str) -> Result<()> {
    if !looks_like_git_fetch_command(command) {
        return Ok(());
    }

    if let Some(host) = command
        .iter()
        .find_map(|arg| private_proxied_source_host(arg))
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

fn private_proxied_source_host(value: &str) -> Option<String> {
    let value = value.trim();
    private_proxied_source_hosts()
        .into_iter()
        .find(|host| remote_matches_host(value, host))
}

fn private_proxied_source_hosts() -> Vec<String> {
    let raw = std::env::var(PRIVATE_PROXIED_SOURCE_HOSTS_ENV).unwrap_or_else(|_| {
        DEFAULT_PRIVATE_PROXIED_SOURCE_HOSTS
            .iter()
            .copied()
            .collect::<Vec<_>>()
            .join(",")
    });

    raw.split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(|host| host.to_ascii_lowercase())
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
    fn detects_default_private_proxied_source_hosts() {
        assert_eq!(
            private_proxied_source_host("git@github.a8c.com:Automattic/example.git"),
            Some("github.a8c.com".to_string())
        );
        assert_eq!(
            private_proxied_source_host("https://github.a8c.com/Automattic/example.git"),
            Some("github.a8c.com".to_string())
        );
        assert_eq!(
            private_proxied_source_host("https://github.com/Extra-Chill/homeboy.git"),
            None
        );
    }

    #[test]
    fn rejects_runner_side_private_proxied_git_materialization() {
        let err = validate_runner_git_materialization(
            "git@github.a8c.com:Automattic/example.git",
            "homeboy-lab",
        )
        .expect_err("private/proxied runner-side git materialization should fail");

        assert!(err.message.contains("--mode git"));
        assert!(err.message.contains("github.a8c.com"));
        assert!(err.message.contains("workspace sync"));
    }

    #[test]
    fn rejects_runner_exec_private_proxied_git_fetches() {
        let err = validate_runner_exec_source_fetch(
            &[
                "sh".to_string(),
                "-c".to_string(),
                "git clone git@github.a8c.com:Automattic/example.git".to_string(),
            ],
            "homeboy-lab",
        )
        .expect_err("private/proxied runner-side git clone should fail");

        assert!(err.message.contains("runner-side Git fetch"));
        assert!(err.message.contains("github.a8c.com"));
        assert!(err.message.contains("workspace sync"));
    }
}
