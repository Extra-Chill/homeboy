use std::path::PathBuf;

use crate::core::{Error, Result};

pub(super) const EXPLICIT_PASSTHROUGH_SENTINEL: &str = "__homeboy_explicit_passthrough__";

pub(super) fn lab_offload_source_path(args: &[String]) -> Result<PathBuf> {
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--path" || arg == "--cwd" {
            let value = iter.next().ok_or_else(|| {
                let field = arg.trim_start_matches("--");
                Error::validation_invalid_argument(
                    field,
                    format!("{arg} requires a value before Lab offload can sync the workspace"),
                    None,
                    None,
                )
            })?;
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
        if let Some(value) = arg.strip_prefix("--path=") {
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
        if let Some(value) = arg.strip_prefix("--cwd=") {
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
    }

    std::env::current_dir()
        .map_err(|err| Error::internal_io(err.to_string(), Some("read cwd".to_string())))
}

pub(super) fn rewrite_lab_offload_args(args: &[String], remote_path: &str) -> Vec<String> {
    let mut stripped = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    let has_force_hot = args.iter().any(|arg| arg == "--force-hot");
    while let Some(arg) = iter.next() {
        if arg == EXPLICIT_PASSTHROUGH_SENTINEL {
            continue;
        }
        if passthrough {
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--path" || arg == "--cwd" {
            stripped.push(arg.clone());
            let _ = iter.next();
            stripped.push(remote_path.to_string());
            continue;
        }
        if arg.starts_with("--path=") {
            stripped.push(format!("--path={remote_path}"));
            continue;
        }
        if arg.starts_with("--cwd=") {
            stripped.push(format!("--cwd={remote_path}"));
            continue;
        }
        if arg == "--runner" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--runner=") {
            continue;
        }
        if arg == "--output" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--output=") {
            continue;
        }
        stripped.push(arg.clone());
    }
    if !has_force_hot {
        stripped.insert(1, "--force-hot".to_string());
    }
    stripped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lab_source_path_uses_agent_task_dispatch_cwd() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--cwd".to_string(),
            "/Users/chubes/Developer/wp-site-generator".to_string(),
            "--prompt".to_string(),
            "cook".to_string(),
        ];

        assert_eq!(
            lab_offload_source_path(&args).expect("source path"),
            PathBuf::from("/Users/chubes/Developer/wp-site-generator")
        );
    }

    #[test]
    fn lab_args_rewrite_agent_task_dispatch_cwd() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--runner".to_string(),
            "homeboy-lab".to_string(),
            "--cwd=/Users/chubes/Developer/wp-site-generator".to_string(),
            "--prompt".to_string(),
            "cook".to_string(),
        ];

        assert_eq!(
            rewrite_lab_offload_args(&args, "/home/chubes/Developer/wp-site-generator"),
            vec![
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "agent-task".to_string(),
                "dispatch".to_string(),
                "--cwd=/home/chubes/Developer/wp-site-generator".to_string(),
                "--prompt".to_string(),
                "cook".to_string(),
            ]
        );
    }
}
