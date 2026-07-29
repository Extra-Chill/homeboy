use std::path::Path;

use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};
use homeboy_core::server::SshClient;

use super::super::{Runner, RunnerKind};

#[allow(unused_imports)]
use super::*;

pub(super) fn validate_required_paths(
    runner: &Runner,
    required_paths: &[String],
    validate_on_host: bool,
) -> Result<()> {
    for path in required_paths {
        if !Path::new(path).is_absolute() {
            return Err(Error::validation_invalid_argument(
                "require_path",
                "runner exec --require-path expects absolute paths on the runner",
                Some(path.to_string()),
                Some(vec![
                    "Pass the path as it exists on the runner, not the controller.".to_string(),
                ]),
            ));
        }
        if (validate_on_host || runner.kind == RunnerKind::Local) && !Path::new(path).exists() {
            return Err(missing_required_path_error(runner, path));
        }
    }

    Ok(())
}

pub(super) fn validate_remote_required_paths(
    client: &mut SshClient,
    required_paths: &[String],
) -> Result<()> {
    for path in required_paths {
        let output = client.execute(&format!("test -e {}", shell::quote_arg(path)));
        if output.exit_code != 0 {
            return Err(Error::validation_invalid_argument(
                "require_path",
                "required runner path does not exist",
                Some(path.to_string()),
                Some(vec![
                    "Use the generated _lab_workspaces/... snapshot path when the controller worktree path was synced into a lab snapshot.".to_string(),
                    "Run an explicit workspace sync/adopt step before referencing controller worktree paths on the runner.".to_string(),
                ]),
            ));
        }
    }

    Ok(())
}

pub(super) fn missing_required_path_error(runner: &Runner, path: &str) -> Error {
    Error::validation_invalid_argument(
        "require_path",
        "required runner path does not exist",
        Some(path.to_string()),
        Some(vec![
            format!(
                "Runner `{}` workspace_root is {}.",
                runner.id,
                runner.workspace_root.as_deref().unwrap_or("not configured")
            ),
            "Use the generated _lab_workspaces/... snapshot path when the controller worktree path was synced into a lab snapshot.".to_string(),
            "Run an explicit workspace sync/adopt step before referencing controller worktree paths on the runner.".to_string(),
        ]),
    )
}

pub(super) fn validate_runner_process_cwd(runner: &Runner, cwd: &str) -> Result<()> {
    if !Path::new(cwd).is_absolute() {
        return Err(Error::validation_invalid_argument(
            "cwd",
            "runner exec requires an absolute cwd",
            Some(cwd.to_string()),
            None,
        ));
    }

    if runner.kind == RunnerKind::Local && !Path::new(cwd).is_dir() {
        return Err(Error::validation_invalid_argument(
            "cwd",
            "local runner cwd must exist and be a directory",
            Some(cwd.to_string()),
            None,
        ));
    }

    Ok(())
}

pub(super) fn resolve_cwd(runner: &Runner, cwd: Option<&str>) -> Result<String> {
    match runner.kind {
        RunnerKind::Local => {
            if let Some(cwd) = cwd {
                return Ok(cwd.to_string());
            }
            if let Some(root) = &runner.workspace_root {
                return Ok(root.clone());
            }
            std::env::current_dir()
                .map(|path| path.display().to_string())
                .map_err(|err| {
                    Error::internal_io(err.to_string(), Some("read current directory".to_string()))
                })
        }
        RunnerKind::Ssh => {
            let Some(root) = runner.workspace_root.as_deref() else {
                return Err(Error::validation_invalid_argument(
                    "workspace_root",
                    "SSH runner execution requires workspace_root so local paths are not silently reused remotely",
                    Some(runner.id.clone()),
                    Some(vec!["Set the runner workspace root or pass --cwd inside that root.".to_string()]),
                ));
            };
            let remote_cwd = cwd.unwrap_or(root);
            validate_remote_cwd(root, remote_cwd)?;
            Ok(remote_cwd.to_string())
        }
    }
}

pub(super) fn validate_remote_cwd(root: &str, cwd: &str) -> Result<()> {
    if !root.starts_with('/') || !cwd.starts_with('/') {
        return Err(Error::validation_invalid_argument(
            "cwd",
            "remote runner cwd and workspace_root must be absolute paths",
            Some(cwd.to_string()),
            None,
        ));
    }
    let root = trim_trailing_slashes(root);
    let cwd = trim_trailing_slashes(cwd);
    if cwd == root || cwd.starts_with(&format!("{root}/")) {
        return Ok(());
    }
    // A path outside the remote root that exists locally is almost always a
    // controller-local checkout handed to a remote-only field. Naming the
    // built-in materialization path turns an unusable "use a path under
    // /home/..." hint into the exact command that does what was intended,
    // instead of sending the operator through a manual `workspace sync` plus
    // remote-path plumbing (#10364).
    let mut tried = Vec::new();
    if std::path::Path::new(&cwd).exists() {
        tried.push(format!(
            "`{cwd}` exists on this controller, so it looks like a local checkout passed to a remote-only field."
        ));
        tried.push(format!(
            "To run against it on the runner, replace `--cwd {cwd}` with `--sync-workspace {cwd}`, keeping the rest of the command unchanged."
        ));
    }
    tried.push(format!("Use a remote path under {root}"));

    Err(Error::validation_invalid_argument(
        "cwd",
        "remote cwd must be inside the configured runner workspace_root",
        Some(cwd),
        Some(tried),
    ))
}

pub(super) fn trim_trailing_slashes(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod validate_remote_cwd_tests {
    use super::*;

    fn tried(error: &Error) -> String {
        error.details["tried"]
            .as_array()
            .expect("tried remediation")
            .iter()
            .filter_map(|hint| hint.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn remote_cwd_inside_the_workspace_root_is_accepted() {
        validate_remote_cwd("/home/chubes/Developer", "/home/chubes/Developer").expect("root");
        validate_remote_cwd(
            "/home/chubes/Developer",
            "/home/chubes/Developer/homeboy@fix",
        )
        .expect("nested path");
    }

    /// A local checkout handed to the remote-only `--cwd` must be diagnosed as
    /// such and answered with the equivalent `--sync-workspace` command (#10364).
    #[test]
    fn existing_local_cwd_outside_the_root_recommends_sync_workspace() {
        let local = tempfile::tempdir().expect("local checkout");
        let local_path = local.path().to_str().expect("utf-8 path");

        let error = validate_remote_cwd("/home/chubes/Developer", local_path)
            .expect_err("a local path is not a valid remote cwd");

        let hints = tried(&error);
        assert!(
            hints.contains("exists on this controller"),
            "must classify the value as a local path: {hints}"
        );
        assert!(
            hints.contains(&format!("--sync-workspace {local_path}")),
            "must name the equivalent one-command path: {hints}"
        );
    }

    /// A path that does not exist locally stays fail-closed with the original
    /// remote-root guidance and no misleading sync suggestion.
    #[test]
    fn missing_cwd_outside_the_root_keeps_the_remote_root_guidance() {
        let error = validate_remote_cwd(
            "/home/chubes/Developer",
            "/definitely/not/a/real/path/for/10364",
        )
        .expect_err("a nonexistent path is not a valid remote cwd");

        let hints = tried(&error);
        assert!(
            hints.contains("Use a remote path under /home/chubes/Developer"),
            "must retain remote-root guidance: {hints}"
        );
        assert!(
            !hints.contains("--sync-workspace"),
            "must not suggest syncing a path that does not exist: {hints}"
        );
    }

    #[test]
    fn relative_paths_are_rejected_before_local_classification() {
        let error = validate_remote_cwd("/home/chubes/Developer", "relative/path")
            .expect_err("relative remote cwd is rejected");
        assert!(error.message.contains("must be absolute paths"));
    }
}
