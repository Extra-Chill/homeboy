use serde::Serialize;

use crate::core::engine::shell;
use crate::core::error::{Error, Result};

use super::{
    workspace::{canonical_workspace_path, git_output, ssh_client_for_runner},
    Runner, RunnerKind,
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub(crate) struct RunnerGitDependencyMaterializationOutput {
    pub local_path: String,
    pub remote_path: String,
    pub remote_url: String,
    pub head: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RunnerGitDependencyMaterializationOptions {
    pub local_path: String,
    pub remote_path: String,
    pub remote_url: Option<String>,
    pub required_subpath: Option<String>,
}

pub(crate) fn materialize_git_dependency(
    runner: &Runner,
    options: RunnerGitDependencyMaterializationOptions,
) -> Result<RunnerGitDependencyMaterializationOutput> {
    let local_path = canonical_workspace_path(&options.local_path)?;
    let head = git_output(&local_path, &["rev-parse", "HEAD"])?;
    let remote_url = match options.remote_url {
        Some(remote_url) if !remote_url.trim().is_empty() => remote_url,
        _ => git_output(&local_path, &["config", "--get", "remote.origin.url"])?,
    };
    if remote_url.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "remote_url",
            "rig dependency materialization requires a git remote URL",
            Some(local_path.display().to_string()),
            Some(vec![
                "Set components.<id>.remote_url in the rig spec or configure remote.origin.url on the local checkout.".to_string(),
            ]),
        ));
    }

    let command = materialize_git_dependency_command(
        &options.remote_path,
        &remote_url,
        &head,
        options.required_subpath.as_deref(),
    );
    let status = match runner.kind {
        RunnerKind::Local => {
            run_shell_command_with_stdout(&command, "materialize local git dependency")?
        }
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            let output = client.execute(&command);
            if !output.success {
                return Err(Error::validation_invalid_argument(
                    "rig_component_dependency",
                    "runner dispatch could not safely materialize a rig component dependency",
                    Some(options.remote_path.clone()),
                    Some(vec![output.stderr.trim().to_string()]),
                ));
            }
            output.stdout.trim().to_string()
        }
    };

    Ok(RunnerGitDependencyMaterializationOutput {
        local_path: local_path.display().to_string(),
        remote_path: options.remote_path,
        remote_url,
        head,
        status,
    })
}

fn materialize_git_dependency_command(
    remote_path: &str,
    remote_url: &str,
    head: &str,
    required_subpath: Option<&str>,
) -> String {
    let required_subpath_check = required_subpath
        .filter(|subpath| !subpath.trim().is_empty())
        .map(|subpath| {
            format!(
                " && if [ ! -d \"$dest/{}\" ]; then echo \"dependency checkout is missing required subpath: $dest/{}\" >&2; exit 23; fi",
                shell_escape_double_quoted(subpath),
                shell_escape_double_quoted(subpath),
            )
        })
        .unwrap_or_default();

    format!(
        r#"raw={raw}; case "$raw" in '~') dest="$HOME" ;; [~]/*) suffix=${{raw#\~/}}; dest="$HOME/$suffix" ;; *) dest="$raw" ;; esac; parent=$(dirname "$dest"); mkdir -p "$parent" && if [ ! -e "$dest" ]; then git clone {url} "$dest" && git -C "$dest" fetch --prune origin '+refs/heads/*:refs/remotes/origin/*' && git -C "$dest" checkout --detach {head} && echo cloned; elif [ ! -d "$dest/.git" ]; then echo "dependency path exists but is not a git checkout: $dest" >&2; exit 20; else actual_url=$(git -C "$dest" config --get remote.origin.url || true); if [ "$actual_url" != {url} ]; then echo "dependency checkout remote mismatch at $dest: expected {url}, found $actual_url" >&2; exit 21; fi; git -C "$dest" fetch --prune origin '+refs/heads/*:refs/remotes/origin/*'; actual_head=$(git -C "$dest" rev-parse HEAD); if [ "$actual_head" != {head} ]; then echo "dependency checkout freshness mismatch at $dest: expected {head}, found $actual_head" >&2; exit 22; fi; echo reused; fi{required_subpath_check}"#,
        raw = shell::quote_arg(remote_path),
        url = shell::quote_arg(remote_url),
        head = shell::quote_arg(head),
        required_subpath_check = required_subpath_check,
    )
}

fn shell_escape_double_quoted(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`")
}

fn run_shell_command_with_stdout(command: &str, action: &str) -> Result<String> {
    let output = std::process::Command::new("sh")
        .args(["-c", command])
        .output()
        .map_err(|err| Error::internal_io(err.to_string(), Some(action.to_string())))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    Err(Error::internal_unexpected(format!(
        "{action} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    )))
}

#[cfg(test)]
mod tests {
    use super::materialize_git_dependency_command;

    #[test]
    fn dependency_materialization_clones_missing_checkout_non_destructively() {
        let command = materialize_git_dependency_command(
            "~/Developer/woocommerce",
            "https://github.com/woocommerce/woocommerce.git",
            "abc123",
            Some("plugins/woocommerce"),
        );

        assert!(command.contains("if [ ! -e \"$dest\" ]; then git clone"));
        assert!(command.contains("checkout --detach"));
        assert!(command.contains("abc123"));
        assert!(command.contains("elif [ ! -d \"$dest/.git\" ]; then"));
        assert!(command.contains("dependency path exists but is not a git checkout"));
        assert!(command.contains("dependency checkout is missing required subpath"));
        assert!(!command.contains("rm -rf"));
        assert!(!command.contains("reset --hard"));
        assert!(!command.contains("clean -ffdqx"));
    }

    #[test]
    fn dependency_materialization_diagnoses_remote_and_freshness_mismatches() {
        let command = materialize_git_dependency_command(
            "/home/chubes/Developer/woocommerce",
            "https://github.com/woocommerce/woocommerce.git",
            "abc123",
            None,
        );

        assert!(command.contains("dependency checkout remote mismatch"));
        assert!(command.contains("dependency checkout freshness mismatch"));
        assert!(command.contains("git -C \"$dest\" fetch --prune origin"));
        assert!(command.contains("actual_head=$(git -C \"$dest\" rev-parse HEAD)"));
    }

    #[test]
    fn dependency_materialization_preserves_runner_tilde_expansion() {
        let command = materialize_git_dependency_command(
            "~/Developer/woocommerce",
            "https://github.com/woocommerce/woocommerce.git",
            "abc123",
            None,
        );

        assert!(command.contains("case \"$raw\" in '~')"));
        assert!(command.contains("[~]/*) suffix=${raw#\\~/}; dest=\"$HOME/$suffix\""));
        assert!(!command.contains("$HOME/~/"));
    }
}
