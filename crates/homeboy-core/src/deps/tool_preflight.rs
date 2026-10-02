//! Dependency tool preflight (#15364).
//!
//! Gate setup runs each dependency provider's install command (for example
//! `pnpm install --frozen-lockfile` when `pnpm-lock.yaml` is present). When the
//! provider's program is not installed on the host that runs gates, setup
//! fails only after a provider attempt has already been paid for. This module
//! resolves the same hydration plans without running anything and reports
//! which programs are missing, so admission and preview can reject early.

use super::provider;
use crate::component;
use crate::cooperative_control::CooperativeControl;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A dependency provider whose install program is not available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingDependencyTool {
    pub provider_id: String,
    pub program: String,
    pub package_root: String,
    pub install_command: Vec<String>,
}

/// Report dependency-provider programs that hydrating `checkout` (its root and
/// direct child package roots, as gate setup does) would need but cannot find.
///
/// Discovery or planning failures are not reported here; gate setup surfaces
/// them with full diagnostics. Only "the planned program is not installed" is.
pub fn missing_dependency_tools(
    checkout: &Path,
    component_id: Option<&str>,
) -> Vec<MissingDependencyTool> {
    let mut missing = Vec::new();
    for (root, relative) in candidate_roots(checkout) {
        let path_arg = root.display().to_string();
        let Ok(mut component) = component::resolve_effective(component_id, Some(&path_arg), None)
        else {
            continue;
        };
        component.local_path = path_arg;
        let control = CooperativeControl::unbounded();
        let Ok(providers) = provider::resolve_dependency_providers_optional_with_control(
            &component, &root, &control,
        ) else {
            continue;
        };
        for dependency_provider in providers {
            let Ok(Some(plan)) = dependency_provider.hydration_plan(&component, &root, &control)
            else {
                continue;
            };
            // The interpreter itself (e.g. `sh` for adapter shell strings)
            // and, for shell strings, the tool the string invokes.
            let program = [
                Some(plan.install.program.clone()),
                required_program(&plan.install.program, &plan.install.args),
            ]
            .into_iter()
            .flatten()
            .find(|program| !program_is_available(program, &plan.install.cwd));
            let Some(program) = program else {
                continue;
            };
            let entry = MissingDependencyTool {
                provider_id: plan.provider_id.clone(),
                program,
                package_root: relative.clone(),
                install_command: crate::redaction::redact_argv(&plan.install.argv()),
            };
            if !missing.contains(&entry) {
                missing.push(entry);
            }
        }
    }
    missing
}

/// The checkout root and its real direct child directories, matching gate
/// setup's dependency root discovery.
fn candidate_roots(checkout: &Path) -> Vec<(PathBuf, String)> {
    let mut roots = vec![(checkout.to_path_buf(), ".".to_string())];
    if let Ok(entries) = std::fs::read_dir(checkout) {
        let mut children = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter(|entry| entry.file_name() != ".git")
            .filter(|entry| !is_nested_repository(&entry.path()))
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                (entry.path(), name)
            })
            .collect::<Vec<_>>();
        children.sort();
        roots.extend(children);
    }
    roots
}

/// The program an install command needs. Adapter commands are shell strings
/// run as `sh -c "<command>"` by contract, so the tool is the first word of
/// that string (`pnpm install --frozen-lockfile` → `pnpm`). Commands that
/// begin with shell syntax or an environment assignment are not guessed at;
/// gate setup still reports their failures in full.
pub(crate) fn required_program(program: &str, args: &[String]) -> Option<String> {
    let shell = matches!(program, "sh" | "bash" | "/bin/sh" | "/bin/bash");
    if !(shell && args.first().map(String::as_str) == Some("-c")) {
        return Some(program.to_string());
    }
    let first = args.get(1)?.split_whitespace().next()?;
    let plain = first
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "._-/+".contains(character));
    // Shell builtins and keywords have no executable to find.
    const SHELL_BUILTINS: &[&str] = &[
        "cd", "exec", "export", "set", "test", "[", "true", "false", ":", ".", "source", "if",
        "for", "while", "case", "env", "command", "eval", "unset", "umask",
    ];
    (plain && !first.contains('=') && !SHELL_BUILTINS.contains(&first)).then(|| first.to_string())
}

/// Whether `dir` is its own Git repository (a submodule or a nested clone).
///
/// Gate setup hydrates the checkout root and its direct child package roots.
/// A nested repository is owned by its own project: the superproject's
/// workspace consumes its packages (e.g. a pnpm workspace that lists
/// `subrouter/cli`), and its own lockfile is not part of this repository's
/// contract, so it must not be installed independently. Before submodules
/// were initialized in Cook checkouts (#15355) these directories were empty
/// and never hydrated; this preserves that behavior.
pub fn is_nested_repository(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// Whether `program` can be spawned: an explicit path that exists, or a bare
/// name found as an executable file on `PATH`.
pub(crate) fn program_is_available(program: &str, cwd: &Path) -> bool {
    if program.is_empty() {
        return false;
    }
    let candidate = Path::new(program);
    if candidate.components().count() > 1 {
        let resolved = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            cwd.join(candidate)
        };
        return is_executable_file(&resolved);
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| is_executable_file(&dir.join(program)))
        })
        .unwrap_or(false)
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_program_reads_the_tool_from_shell_commands() {
        let shell =
            |command: &str| required_program("sh", &["-c".to_string(), command.to_string()]);
        assert_eq!(
            shell("pnpm install --frozen-lockfile").as_deref(),
            Some("pnpm")
        );
        assert_eq!(shell("npm ci").as_deref(), Some("npm"));
        assert_eq!(
            shell("./scripts/deps.sh install").as_deref(),
            Some("./scripts/deps.sh")
        );
        assert_eq!(shell("CI=1 pnpm install"), None);
        assert_eq!(shell("cd sub && make"), None);
        assert_eq!(shell("env CI=1 pnpm install"), None);
        assert_eq!(shell("$(which pnpm) install"), None);
        assert_eq!(shell(""), None);
        assert_eq!(
            required_program("composer", &["install".to_string()]).as_deref(),
            Some("composer")
        );
    }

    #[test]
    fn nested_repositories_are_not_dependency_roots() {
        let checkout = tempfile::tempdir().unwrap();
        let package = checkout.path().join("package");
        let submodule = checkout.path().join("vendor");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::create_dir_all(&submodule).unwrap();
        // Submodules carry a `.git` file pointing at the superproject's modules.
        std::fs::write(submodule.join(".git"), "gitdir: ../.git/modules/vendor\n").unwrap();
        assert!(is_nested_repository(&submodule));
        assert!(!is_nested_repository(&package));
        let roots = candidate_roots(checkout.path())
            .into_iter()
            .map(|(_, relative)| relative)
            .collect::<Vec<_>>();
        assert_eq!(roots, vec![".".to_string(), "package".to_string()]);
    }

    #[test]
    fn program_availability_checks_path_and_explicit_paths() {
        let dir = tempfile::tempdir().unwrap();
        assert!(program_is_available("sh", dir.path()));
        assert!(!program_is_available(
            "homeboy-definitely-not-installed-15364",
            dir.path()
        ));
        assert!(!program_is_available("", dir.path()));

        let script = dir.path().join("tool.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        assert!(
            !program_is_available("./tool.sh", dir.path()),
            "not executable"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(program_is_available("./tool.sh", dir.path()));
        assert!(program_is_available(
            &script.display().to_string(),
            dir.path()
        ));
    }
}
