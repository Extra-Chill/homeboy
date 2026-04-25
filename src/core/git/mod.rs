mod changes;
mod commits;
mod github;
mod operations;
mod primitives;
mod stack;

pub use changes::*;
pub use commits::*;
pub use github::*;
pub use operations::*;
pub use primitives::*;
pub use stack::*;

use std::process::Command;

use crate::error::Error;

fn execute_git(path: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git").args(args).current_dir(path).output()
}

/// Well-known bot identity for CI commits.
pub const BOT_NAME: &str = "homeboy-ci[bot]";
/// Well-known bot email for CI commits (GitHub noreply address).
pub const BOT_EMAIL: &str = "266378653+homeboy-ci[bot]@users.noreply.github.com";

/// Parsed git identity (name + email).
pub struct GitIdentity {
    pub name: String,
    pub email: String,
}

/// Parse a `--git-identity` value into name + email.
///
/// - `None` or `"bot"` → default CI bot identity
/// - `"Name <email>"` → parsed
/// - `"Name"` → name with bot email
pub fn parse_git_identity(identity: Option<&str>) -> GitIdentity {
    match identity {
        None | Some("bot") => GitIdentity {
            name: BOT_NAME.to_string(),
            email: BOT_EMAIL.to_string(),
        },
        Some(custom) => {
            if let Some(angle_start) = custom.find('<') {
                let name = custom[..angle_start].trim().to_string();
                let email = custom[angle_start + 1..]
                    .trim_end_matches('>')
                    .trim()
                    .to_string();
                if !name.is_empty() && !email.is_empty() {
                    return GitIdentity { name, email };
                }
            }
            GitIdentity {
                name: custom.to_string(),
                email: BOT_EMAIL.to_string(),
            }
        }
    }
}

/// Configure git user.name and user.email in a repository.
pub fn configure_identity(path: &str, identity: &GitIdentity) -> crate::error::Result<()> {
    for (key, value) in [
        ("user.name", identity.name.as_str()),
        ("user.email", identity.email.as_str()),
    ] {
        let output = execute_git(path, &["config", key, value])
            .map_err(|e| Error::git_command_failed(format!("git config {key}: {e}")))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::git_command_failed(format!(
                "git config {key} failed: {stderr}"
            )));
        }
    }
    Ok(())
}

/// Resolve a (component_id, path) pair for a git operation.
///
/// Resolution order:
/// 1. **Both `component_id` and `path_override` provided** — trust both;
///    no registry lookup. Used by rig pipelines + workflows that already
///    know the path they want to operate on.
/// 2. **`path_override` only** — use the path; derive the ID from a
///    portable `homeboy.json` at the path or its git root, falling back
///    to the directory basename.
/// 3. **`component_id` only** — look the component up in the registry,
///    use its configured `local_path`.
/// 4. **Neither** — fall through to [`crate::component::resolve`], which
///    detects from CWD via the registry first, then portable
///    `homeboy.json` at CWD or git root. This is what makes
///    `homeboy git status` (and friends) work without arguments when
///    run from inside a checkout.
pub(super) fn resolve_target(
    component_id: Option<&str>,
    path_override: Option<&str>,
) -> crate::error::Result<(String, String)> {
    // Case 1 & 2: explicit path given.
    if let Some(path) = path_override {
        if let Some(id) = component_id {
            return Ok((id.to_string(), path.to_string()));
        }
        // Discover ID from path or its git root via portable homeboy.json.
        let dir = std::path::Path::new(path);
        if let Some(comp) = crate::component::discover_from_portable(dir) {
            return Ok((comp.id, path.to_string()));
        }
        if let Some(git_root) = crate::component::resolution::detect_git_root(dir) {
            if git_root != dir {
                if let Some(comp) = crate::component::discover_from_portable(&git_root) {
                    return Ok((comp.id, path.to_string()));
                }
            }
        }
        // No portable config — synthesize an ID from the path basename so
        // downstream output still has a meaningful identifier.
        let basename = dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("(unknown)")
            .to_string();
        return Ok((basename, path.to_string()));
    }

    // Case 3: ID without path — look it up in the registry.
    if let Some(id) = component_id {
        let comp = crate::component::resolve_effective(Some(id), None, None)?;
        return Ok((id.to_string(), comp.local_path));
    }

    // Case 4: neither — CWD detection.
    let comp = crate::component::resolve(None)?;
    Ok((comp.id, comp.local_path))
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn bot_shorthand() {
        let id = parse_git_identity(Some("bot"));
        assert_eq!(id.name, BOT_NAME);
        assert_eq!(id.email, BOT_EMAIL);
    }

    #[test]
    fn none_defaults_to_bot() {
        let id = parse_git_identity(None);
        assert_eq!(id.name, BOT_NAME);
        assert_eq!(id.email, BOT_EMAIL);
    }

    #[test]
    fn custom_name_and_email() {
        let id = parse_git_identity(Some("Deploy Bot <deploy@example.com>"));
        assert_eq!(id.name, "Deploy Bot");
        assert_eq!(id.email, "deploy@example.com");
    }

    #[test]
    fn name_only_uses_bot_email() {
        let id = parse_git_identity(Some("My Service"));
        assert_eq!(id.name, "My Service");
        assert_eq!(id.email, BOT_EMAIL);
    }
}

#[cfg(test)]
mod resolve_target_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Returns (TempDir, repo_path) where repo_path has a homeboy.json with
    /// the given component id.
    fn make_portable_repo(id: &str) -> (TempDir, std::path::PathBuf) {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let repo = dir.path().join(id);
        fs::create_dir_all(&repo).expect("Failed to create repo dir");

        let portable = serde_json::json!({ "id": id });
        fs::write(
            repo.join("homeboy.json"),
            serde_json::to_string_pretty(&portable).unwrap(),
        )
        .expect("Failed to write homeboy.json");

        // Capture path before moving dir.
        let path = repo.clone();
        (dir, path)
    }

    #[test]
    fn path_only_discovers_id_from_portable_config() {
        let (_dir, repo) = make_portable_repo("my-plugin");

        let (id, path) = resolve_target(None, Some(repo.to_str().unwrap()))
            .expect("resolve_target should succeed with --path pointing at portable config");

        assert_eq!(id, "my-plugin");
        assert_eq!(path, repo.to_string_lossy());
    }

    #[test]
    fn path_only_falls_back_to_basename_when_no_portable_config() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("bare-checkout");
        fs::create_dir_all(&repo).unwrap();

        let (id, path) = resolve_target(None, Some(repo.to_str().unwrap()))
            .expect("resolve_target should succeed with --path even without portable config");

        assert_eq!(id, "bare-checkout");
        assert_eq!(path, repo.to_string_lossy());
    }

    #[test]
    fn path_and_id_both_provided_skips_discovery() {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("anywhere");
        fs::create_dir_all(&repo).unwrap();

        let (id, path) = resolve_target(Some("explicit-id"), Some(repo.to_str().unwrap()))
            .expect("resolve_target should succeed with both args");

        // Trusts both inputs verbatim — no registry lookup, no discovery.
        assert_eq!(id, "explicit-id");
        assert_eq!(path, repo.to_string_lossy());
    }

    #[test]
    fn path_only_walks_up_to_git_root_for_portable_config() {
        // Layout:
        //   <tmp>/repo/                  (homeboy.json lives here)
        //   <tmp>/repo/.git/
        //   <tmp>/repo/subdir/
        //
        // Calling with path=<tmp>/repo/subdir should find homeboy.json at
        // the git root via the existing detect_git_root walk.
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("repo");
        fs::create_dir_all(repo.join("subdir")).unwrap();

        let portable = serde_json::json!({ "id": "monorepo-thing" });
        fs::write(
            repo.join("homeboy.json"),
            serde_json::to_string_pretty(&portable).unwrap(),
        )
        .unwrap();

        // git init so detect_git_root can find the root.
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .output()
            .expect("git init");

        let subdir = repo.join("subdir");
        let (id, path) = resolve_target(None, Some(subdir.to_str().unwrap()))
            .expect("resolve_target should walk up to git root for portable config");

        assert_eq!(id, "monorepo-thing");
        assert_eq!(path, subdir.to_string_lossy());
    }
}
