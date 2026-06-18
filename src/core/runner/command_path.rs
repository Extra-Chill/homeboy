use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::error::{Error, Result};

const HOME_BIN_DIRS: &[&str] = &[".local/bin"];
const ABSOLUTE_BIN_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];

pub(crate) fn normalize_runner_command_env(env: &mut HashMap<String, String>) {
    if env.contains_key("PATH") {
        return;
    }
    if let Some(path) = local_runner_command_path() {
        env.insert("PATH".to_string(), path.to_string_lossy().to_string());
    }
}

pub(crate) fn remote_shell_path_preamble() -> &'static str {
    concat!(
        "export PATH=\"$HOME/.local/bin:$HOME/.",
        "car",
        "go/bin:$HOME/.kimaki/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:${PATH:-}\"; ",
        "for d in \"$HOME\"/.local/opt/node-*/bin \"$HOME\"/.nvm/versions/node/*/bin; do ",
        "[ -d \"$d\" ] && PATH=\"$d:$PATH\"; done; export PATH"
    )
}

pub(crate) fn quote_runner_env_value(key: &str, value: &str) -> String {
    if key == "PATH" {
        return format!("\"{}\"", escape_double_quoted_env_value(value));
    }

    crate::core::engine::shell::quote_arg(value)
}

/// Explicit path-translation preflight for a remote dispatch argv.
///
/// Rejects any argument that still embeds the controller-local source-checkout
/// root (`source_path`) without having been translated to the remote workspace
/// (`remote_cwd`). This is the shared final gate before a remote `exec`, so a
/// missed path remap fails loudly on the controller instead of handing a
/// non-existent local path to the remote runner. `context` labels the calling
/// dispatch path (Lab offload, rig source management, ...) in the error (#5093).
pub fn preflight_remote_argv_path_translation(
    context: &str,
    runner_id: &str,
    command: &[String],
    source_path: &Path,
    remote_cwd: &str,
) -> Result<()> {
    let local_root = source_path.display().to_string();
    let local_root = local_root.trim_end_matches('/');
    if local_root.is_empty() {
        return Ok(());
    }

    let leaked: Vec<String> = command
        .iter()
        .filter(|arg| arg_embeds_untranslated_local_path(arg, local_root, remote_cwd))
        .cloned()
        .collect();
    if leaked.is_empty() {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "command",
        format!(
            "{context} refused to dispatch to runner `{runner_id}`: {} remote argv argument(s) still reference the controller-local source path `{local_root}` instead of the remote workspace `{remote_cwd}`",
            leaked.len()
        ),
        Some(runner_id.to_string()),
        Some(vec![
            format!("Untranslated argument(s): {}", leaked.join(", ")),
            "This is a path-translation defect in the remote dispatch argv pipeline; the argument must be remapped to the remote workspace path before dispatch.".to_string(),
        ]),
    ))
}

/// True when `arg` embeds the controller-local source root but has not been
/// translated to the remote workspace path. Arguments that already point at the
/// remote workspace (or do not reference the local root at all) are accepted.
fn arg_embeds_untranslated_local_path(arg: &str, local_root: &str, remote_cwd: &str) -> bool {
    if !arg.contains(local_root) {
        return false;
    }
    // A correctly translated argument addresses the remote workspace root; if it
    // happens to share a prefix string with the local root that is fine.
    if !remote_cwd.is_empty() && arg.contains(remote_cwd) {
        return false;
    }
    true
}

fn escape_double_quoted_env_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
}

fn local_runner_command_path() -> Option<OsString> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let existing_path = std::env::var_os("PATH");
    build_runner_command_path(home.as_deref(), existing_path.as_deref())
}

fn build_runner_command_path(
    home: Option<&Path>,
    existing_path: Option<&OsStr>,
) -> Option<OsString> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();

    if let Some(home) = home {
        for rel in HOME_BIN_DIRS {
            push_existing_path(&mut paths, &mut seen, home.join(rel));
        }
        push_existing_path(
            &mut paths,
            &mut seen,
            home.join([".car", "go"].concat()).join("bin"),
        );
        push_existing_path(&mut paths, &mut seen, home.join(".kimaki/bin"));
        push_node_bins(&mut paths, &mut seen, &home.join(".local/opt"), "node-");
        push_node_bins(&mut paths, &mut seen, &home.join(".nvm/versions/node"), "");
    }

    for path in ABSOLUTE_BIN_DIRS {
        push_existing_path(&mut paths, &mut seen, PathBuf::from(path));
    }

    if let Some(existing_path) = existing_path {
        for path in std::env::split_paths(existing_path) {
            push_path(&mut paths, &mut seen, path);
        }
    }

    if paths.is_empty() {
        None
    } else {
        std::env::join_paths(paths).ok()
    }
}

fn push_node_bins(
    paths: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    versions_dir: &Path,
    prefix: &str,
) {
    let Ok(entries) = fs::read_dir(versions_dir) else {
        return;
    };

    let mut bins = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            prefix.is_empty()
                || path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(prefix))
        })
        .map(|path| path.join("bin"))
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    bins.sort();
    bins.reverse();

    for bin in bins {
        push_path(paths, seen, bin);
    }
}

fn push_existing_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if path.exists() {
        push_path(paths, seen, path);
    }
}

fn push_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if seen.insert(path.clone()) {
        paths.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn runner_command_path_prepends_common_user_tool_dirs() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let local_bin = home.join(".local/bin");
        let toolchain_bin = home.join([".car", "go"].concat()).join("bin");
        let kimaki_bin = home.join(".kimaki/bin");
        let local_node = home.join(".local/opt/node-v24.13.1-linux-x64/bin");
        let nvm_node = home.join(".nvm/versions/node/v20.0.0/bin");
        fs::create_dir_all(&local_bin).expect("local bin");
        fs::create_dir_all(&toolchain_bin).expect("toolchain bin");
        fs::create_dir_all(&kimaki_bin).expect("agent bin");
        fs::create_dir_all(&local_node).expect("local node");
        fs::create_dir_all(&nvm_node).expect("nvm node");

        let path = build_runner_command_path(Some(&home), Some(&OsString::from("/usr/bin:/bin")))
            .expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], local_bin);
        assert_eq!(parts[1], toolchain_bin);
        assert_eq!(parts[2], kimaki_bin);
        assert!(parts.contains(&local_node));
        assert!(parts.contains(&nvm_node));
        assert!(parts.contains(&PathBuf::from("/usr/bin")));
        assert!(parts.contains(&PathBuf::from("/bin")));
    }

    #[test]
    fn runner_env_keeps_explicit_path() {
        let mut env = HashMap::from([("PATH".to_string(), "/custom/bin".to_string())]);

        normalize_runner_command_env(&mut env);

        assert_eq!(env.get("PATH").map(String::as_str), Some("/custom/bin"));
    }

    #[test]
    fn remote_shell_path_preamble_includes_local_opt_node_glob() {
        let preamble = remote_shell_path_preamble();

        assert!(preamble.contains("$HOME/.local/bin"));
        assert!(preamble.contains("$HOME\"/.local/opt/node-*/bin"));
        assert!(preamble.contains("$HOME\"/.nvm/versions/node/*/bin"));
    }

    #[test]
    fn path_env_value_allows_existing_path_expansion() {
        assert_eq!(
            quote_runner_env_value("PATH", "$PATH:/custom/bin"),
            "\"$PATH:/custom/bin\""
        );
    }

    #[test]
    fn non_path_env_value_uses_shell_quoting() {
        assert_eq!(
            quote_runner_env_value("TOKEN", "hello world"),
            "'hello world'"
        );
    }
}
