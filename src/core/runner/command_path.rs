use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::error::{Error, Result};

use super::lab_args::LabPathRemap;

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

pub(crate) fn preflight_remote_path_bearing_surfaces(
    context: &str,
    runner_id: &str,
    command: &[String],
    env: &HashMap<String, String>,
    source_path: &Path,
    remote_cwd: &str,
    mappings: &[LabPathRemap],
) -> Result<()> {
    let mut failures = Vec::new();
    collect_path_setting_failures(command, source_path, remote_cwd, mappings, &mut failures);
    collect_path_env_failures(env, source_path, remote_cwd, mappings, &mut failures);

    if failures.is_empty() {
        return Ok(());
    }

    failures.sort_by(|left, right| left.surface.cmp(&right.surface));
    let preview = failures
        .iter()
        .take(5)
        .map(|failure| {
            format!(
                "{} `{}` -> `{}` (exists locally: {})",
                failure.kind, failure.surface, failure.path, failure.exists_locally
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    let mut hints = vec![
        format!("Selected runner: {runner_id}"),
        format!("Remote workspace: {remote_cwd}"),
    ];
    hints.extend(failures.iter().take(5).map(|failure| {
        format!(
            "Missing workspace mapping candidate: `{}` from `{}`",
            failure.path, failure.surface
        )
    }));
    hints.push("Materialize each controller-local path through Lab workspace sync/remapping before dispatch, or replace it with a runner-local path that is valid on the selected runner.".to_string());

    Err(Error::validation_invalid_argument(
        "path",
        format!(
            "{context} refused to dispatch to runner `{runner_id}`: {} path-bearing remote surface(s) still reference controller-local absolute paths",
            failures.len()
        ),
        Some(preview),
        Some(hints),
    ))
}

#[derive(Debug)]
struct PathSurfaceFailure {
    kind: &'static str,
    surface: String,
    path: String,
    exists_locally: bool,
}

fn collect_path_setting_failures(
    command: &[String],
    source_path: &Path,
    remote_cwd: &str,
    mappings: &[LabPathRemap],
    failures: &mut Vec<PathSurfaceFailure>,
) {
    let mut iter = command.iter().peekable();
    let mut passthrough = false;
    while let Some(arg) = iter.next() {
        if passthrough {
            continue;
        }
        if arg == "--" {
            passthrough = true;
            continue;
        }
        if arg == "--setting" {
            if let Some(raw) = iter.next() {
                collect_setting_pair_failures(
                    "--setting",
                    raw,
                    source_path,
                    remote_cwd,
                    mappings,
                    failures,
                );
            }
            continue;
        }
        if arg == "--setting-json" {
            if let Some(raw) = iter.next() {
                collect_json_setting_pair_failures(
                    "--setting-json",
                    raw,
                    source_path,
                    remote_cwd,
                    mappings,
                    failures,
                );
            }
            continue;
        }
        if let Some(raw) = arg.strip_prefix("--setting=") {
            collect_setting_pair_failures(
                "--setting",
                raw,
                source_path,
                remote_cwd,
                mappings,
                failures,
            );
        } else if let Some(raw) = arg.strip_prefix("--setting-json=") {
            collect_json_setting_pair_failures(
                "--setting-json",
                raw,
                source_path,
                remote_cwd,
                mappings,
                failures,
            );
        } else if let Some((flag, value)) = arg.split_once('=') {
            if is_path_bearing_flag(flag) {
                collect_value_path_failures(
                    "arg",
                    flag,
                    value,
                    source_path,
                    remote_cwd,
                    mappings,
                    failures,
                );
            }
        } else if is_path_bearing_flag(arg) {
            if let Some(value) = iter.peek() {
                collect_value_path_failures(
                    "arg",
                    arg,
                    value,
                    source_path,
                    remote_cwd,
                    mappings,
                    failures,
                );
            }
        }
    }
}

fn collect_setting_pair_failures(
    flag: &str,
    raw: &str,
    source_path: &Path,
    remote_cwd: &str,
    mappings: &[LabPathRemap],
    failures: &mut Vec<PathSurfaceFailure>,
) {
    let Some((key, value)) = raw.split_once('=') else {
        return;
    };
    if !is_path_bearing_key(key) {
        return;
    }
    collect_value_path_failures(
        "setting",
        &format!("{flag} {key}"),
        value,
        source_path,
        remote_cwd,
        mappings,
        failures,
    );
}

fn collect_json_setting_pair_failures(
    flag: &str,
    raw: &str,
    source_path: &Path,
    remote_cwd: &str,
    mappings: &[LabPathRemap],
    failures: &mut Vec<PathSurfaceFailure>,
) {
    let Some((key, value)) = raw.split_once('=') else {
        return;
    };
    if !is_path_bearing_key(key) {
        return;
    }
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(json) => collect_json_value_path_failures(
            "setting-json",
            &format!("{flag} {key}"),
            &json,
            source_path,
            remote_cwd,
            mappings,
            failures,
        ),
        Err(_) => collect_value_path_failures(
            "setting-json",
            &format!("{flag} {key}"),
            value,
            source_path,
            remote_cwd,
            mappings,
            failures,
        ),
    }
}

fn collect_json_value_path_failures(
    kind: &'static str,
    surface: &str,
    value: &serde_json::Value,
    source_path: &Path,
    remote_cwd: &str,
    mappings: &[LabPathRemap],
    failures: &mut Vec<PathSurfaceFailure>,
) {
    match value {
        serde_json::Value::String(text) => collect_value_path_failures(
            kind,
            surface,
            text,
            source_path,
            remote_cwd,
            mappings,
            failures,
        ),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_json_value_path_failures(
                    kind,
                    surface,
                    item,
                    source_path,
                    remote_cwd,
                    mappings,
                    failures,
                );
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_json_value_path_failures(
                    kind,
                    surface,
                    item,
                    source_path,
                    remote_cwd,
                    mappings,
                    failures,
                );
            }
        }
        _ => {}
    }
}

fn collect_path_env_failures(
    env: &HashMap<String, String>,
    source_path: &Path,
    remote_cwd: &str,
    mappings: &[LabPathRemap],
    failures: &mut Vec<PathSurfaceFailure>,
) {
    for (key, value) in env {
        if !is_path_bearing_env_key(key) {
            continue;
        }
        collect_value_path_failures(
            "env",
            key,
            value,
            source_path,
            remote_cwd,
            mappings,
            failures,
        );
    }
}

fn collect_value_path_failures(
    kind: &'static str,
    surface: &str,
    value: &str,
    source_path: &Path,
    remote_cwd: &str,
    mappings: &[LabPathRemap],
    failures: &mut Vec<PathSurfaceFailure>,
) {
    for path in path_candidates(value) {
        if !is_unmapped_controller_path(&path, source_path, remote_cwd, mappings) {
            continue;
        }
        failures.push(PathSurfaceFailure {
            kind,
            surface: surface.to_string(),
            exists_locally: expanded_path(&path).exists(),
            path,
        });
    }
}

fn path_candidates(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.starts_with('/') || trimmed.starts_with("~/") {
        return vec![trimmed.to_string()];
    }
    trimmed
        .split(':')
        .filter(|part| part.starts_with('/') || part.starts_with("~/"))
        .map(str::to_string)
        .collect()
}

fn is_unmapped_controller_path(
    value: &str,
    source_path: &Path,
    remote_cwd: &str,
    mappings: &[LabPathRemap],
) -> bool {
    if path_is_under_remote_root(value, remote_cwd, mappings) {
        return false;
    }
    if path_is_under_local_root(value, &source_path.display().to_string()) {
        return true;
    }
    if mappings
        .iter()
        .any(|mapping| path_is_under_local_root(value, &mapping.local))
    {
        return true;
    }
    let expanded = expanded_path(value);
    if expanded.exists() {
        return true;
    }
    std::env::var_os("HOME").is_some_and(|home| {
        path_is_under_local_root(value, &PathBuf::from(home).display().to_string())
    })
}

fn path_is_under_remote_root(value: &str, remote_cwd: &str, mappings: &[LabPathRemap]) -> bool {
    path_is_under_local_root(value, remote_cwd)
        || mappings
            .iter()
            .any(|mapping| path_is_under_local_root(value, &mapping.remote))
}

fn path_is_under_local_root(value: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    !root.is_empty() && (value == root || value.starts_with(&format!("{root}/")))
}

fn expanded_path(value: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(value).to_string())
}

fn is_path_bearing_key(key: &str) -> bool {
    let key = key.rsplit('.').next().unwrap_or(key).to_ascii_lowercase();
    key == "path"
        || key == "paths"
        || key == "cwd"
        || key == "dir"
        || key == "dirs"
        || key == "file"
        || key == "files"
        || key == "root"
        || key == "roots"
        || key.ends_with("_path")
        || key.ends_with("_paths")
        || key.ends_with("_dir")
        || key.ends_with("_dirs")
        || key.ends_with("_file")
        || key.ends_with("_files")
        || key.ends_with("_root")
        || key.ends_with("_roots")
}

fn is_path_bearing_env_key(key: &str) -> bool {
    if key == "PATH" || key.starts_with("HOMEBOY_") {
        return false;
    }
    is_path_bearing_key(key)
}

fn is_path_bearing_flag(flag: &str) -> bool {
    let Some(name) = flag.strip_prefix("--") else {
        return false;
    };
    is_path_bearing_key(&name.replace('-', "_"))
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

    #[test]
    fn preflight_rejects_split_path_setting_with_unmapped_controller_path() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let fixture = controller.path().join("fixture-root");
        fs::create_dir_all(&source).expect("source");
        fs::create_dir_all(&fixture).expect("fixture");
        let command = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--setting".to_string(),
            format!("fixture_root={}", fixture.display()),
        ];

        let err = preflight_remote_path_bearing_surfaces(
            "Lab offload",
            "lab-runner",
            &command,
            &HashMap::new(),
            &source,
            "/runner/workspaces/primary",
            &[],
        )
        .expect_err("unmapped path setting must fail locally");

        assert!(err.message.contains("lab-runner"));
        assert!(err.message.contains("controller-local absolute paths"));
        assert!(err
            .details
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id.contains("--setting fixture_root")));
        assert!(err
            .details
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id.contains("exists locally: true")));
    }

    #[test]
    fn preflight_rejects_inline_json_setting_with_unmapped_controller_path() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let nested = controller.path().join("nested");
        fs::create_dir_all(&source).expect("source");
        fs::create_dir_all(&nested).expect("nested");
        let command = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            format!(
                "--setting-json=config_paths={}",
                serde_json::json!({ "fixtures": [nested.display().to_string()] })
            ),
        ];

        let err = preflight_remote_path_bearing_surfaces(
            "Lab offload",
            "lab-runner",
            &command,
            &HashMap::new(),
            &source,
            "/runner/workspaces/primary",
            &[],
        )
        .expect_err("unmapped json path setting must fail locally");

        let id = err
            .details
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(id.contains("--setting-json config_paths"));
        assert!(id.contains(&nested.display().to_string()));
    }

    #[test]
    fn preflight_rejects_path_like_env_override_with_controller_path() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let tools = controller.path().join("tools");
        fs::create_dir_all(&source).expect("source");
        fs::create_dir_all(&tools).expect("tools");
        let env = HashMap::from([("TOOL_ROOT".to_string(), tools.display().to_string())]);

        let err = preflight_remote_path_bearing_surfaces(
            "Lab offload",
            "lab-runner",
            &["homeboy".to_string(), "test".to_string()],
            &env,
            &source,
            "/runner/workspaces/primary",
            &[],
        )
        .expect_err("unmapped path env must fail locally");

        let id = err
            .details
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(id.contains("env `TOOL_ROOT`"));
        assert!(id.contains(&tools.display().to_string()));
    }

    #[test]
    fn preflight_rejects_path_bearing_argv_flag_with_controller_path() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let cwd = controller.path().join("workload");
        fs::create_dir_all(&source).expect("source");
        fs::create_dir_all(&cwd).expect("cwd");
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "dispatch".to_string(),
            "--cwd".to_string(),
            cwd.display().to_string(),
        ];

        let err = preflight_remote_path_bearing_surfaces(
            "Lab offload",
            "lab-runner",
            &command,
            &HashMap::new(),
            &source,
            "/runner/workspaces/primary",
            &[],
        )
        .expect_err("unmapped argv path flag must fail locally");

        let id = err
            .details
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        assert!(id.contains("arg `--cwd`"));
        assert!(id.contains(&cwd.display().to_string()));
    }

    #[test]
    fn preflight_accepts_mapped_remote_paths_and_non_path_settings() {
        let controller = tempfile::tempdir().expect("controller");
        let source = controller.path().join("primary");
        let local_tools = controller.path().join("tools");
        fs::create_dir_all(&source).expect("source");
        fs::create_dir_all(&local_tools).expect("tools");
        let command = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--setting=tool_root=/runner/workspaces/tools".to_string(),
            "--setting".to_string(),
            format!("mode={}", local_tools.display()),
        ];
        let mappings = vec![LabPathRemap {
            local: local_tools.display().to_string(),
            remote: "/runner/workspaces/tools".to_string(),
        }];

        preflight_remote_path_bearing_surfaces(
            "Lab offload",
            "lab-runner",
            &command,
            &HashMap::new(),
            &source,
            "/runner/workspaces/primary",
            &mappings,
        )
        .expect("remote mapped paths and non-path settings should pass");
    }
}
