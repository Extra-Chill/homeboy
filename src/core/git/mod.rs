mod changes;
mod commits;
mod operations;
mod primitives;

pub use changes::*;
pub use commits::*;
pub use operations::*;
pub use primitives::*;

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

fn resolve_target(
    component_id: Option<&str>,
    path_override: Option<&str>,
) -> crate::error::Result<(String, String)> {
    let id = component_id.ok_or_else(|| {
        Error::validation_invalid_argument(
            "componentId",
            "Missing componentId",
            None,
            Some(vec![
                "Provide a component ID: homeboy git <command> <component-id>".to_string(),
                "List available components: homeboy component list".to_string(),
            ]),
        )
    })?;
    let path = if let Some(p) = path_override {
        p.to_string()
    } else {
        let comp = crate::component::resolve_effective(Some(id), None, None)?;
        comp.local_path
    };
    Ok((id.to_string(), path))
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
