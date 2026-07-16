use crate::error::{Error, Result};

use super::ServerAuth;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSshSession {
    pub control_path: String,
    pub persist: String,
}

impl ManagedSshSession {
    pub fn from_auth(auth: &ServerAuth) -> Self {
        Self {
            control_path: expand_control_path(
                auth.session
                    .control_path
                    .as_deref()
                    .unwrap_or("~/.ssh/controlmasters/%h-%p-%r"),
            ),
            persist: auth
                .session
                .persist
                .clone()
                .unwrap_or_else(|| "4h".to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManagedSshSessionOutput {
    pub session: super::ServerSessionConfig,
    pub live: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

pub fn ensure_control_path_parent(control_path: &str) -> Result<()> {
    let path = std::path::Path::new(control_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!(
                    "create SSH control path directory {}",
                    parent.display()
                )),
            )
        })?;
    }
    Ok(())
}

fn expand_control_path(path: &str) -> String {
    shellexpand::tilde(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{ServerAuthMode, ServerSessionConfig};

    #[test]
    fn test_from_auth() {
        let auth = ServerAuth {
            mode: ServerAuthMode::KeyPlusPasswordControlmaster,
            session: ServerSessionConfig {
                control_path: Some("/tmp/homeboy-session-%h-%p-%r".to_string()),
                persist: Some("30m".to_string()),
            },
        };

        let session = ManagedSshSession::from_auth(&auth);

        assert_eq!(session.control_path, "/tmp/homeboy-session-%h-%p-%r");
        assert_eq!(session.persist, "30m");
    }

    #[test]
    fn test_from_auth_defaults() {
        let auth = ServerAuth {
            mode: ServerAuthMode::KeyPlusPasswordControlmaster,
            session: ServerSessionConfig::default(),
        };

        let session = ManagedSshSession::from_auth(&auth);

        assert!(session
            .control_path
            .ends_with("/.ssh/controlmasters/%h-%p-%r"));
        assert_eq!(session.persist, "4h");
    }

    #[test]
    fn test_ensure_control_path_parent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let control_path = dir.path().join("nested/control");

        ensure_control_path_parent(&control_path.to_string_lossy()).expect("create parent");

        assert!(control_path.parent().expect("parent").exists());
    }
}
