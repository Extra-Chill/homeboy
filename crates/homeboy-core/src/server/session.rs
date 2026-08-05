use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

use super::ServerAuth;

pub const LEGACY_DEFAULT_PERSIST: &str = "4h";
pub const PERSIST_SCOPE: &str =
    "local OpenSSH ControlMaster idle lifetime; not a remote server policy";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagedSshSessionPersistSource {
    Configured,
    Migrated,
    LegacyDefault,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManagedSshSession {
    pub control_path: String,
    pub persist: String,
    pub persist_source: ManagedSshSessionPersistSource,
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
                .unwrap_or_else(|| LEGACY_DEFAULT_PERSIST.to_string()),
            persist_source: auth.session.persist_source.unwrap_or_else(|| {
                if auth.session.persist.is_some() {
                    ManagedSshSessionPersistSource::Configured
                } else {
                    ManagedSshSessionPersistSource::LegacyDefault
                }
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ManagedSshSessionOutput {
    pub session: ManagedSshSession,
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

pub fn validate_persist(persist: &str) -> Result<()> {
    let valid = matches!(persist, "yes" | "no") || is_openssh_duration(persist);

    if valid {
        Ok(())
    } else {
        Err(Error::validation_invalid_argument(
            "auth.persist",
            "must be an OpenSSH ControlPersist value: yes, no, seconds, or a compound duration such as 4h or 1h30m",
            None,
            None,
        ))
    }
}

fn is_openssh_duration(persist: &str) -> bool {
    let mut remaining = persist.as_bytes();
    while !remaining.is_empty() {
        let digits = remaining
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digits == 0 {
            return false;
        }
        remaining = &remaining[digits..];
        if remaining.is_empty() {
            return true;
        }
        if !matches!(remaining[0], b's' | b'm' | b'h' | b'd' | b'w') {
            return false;
        }
        remaining = &remaining[1..];
        if remaining.is_empty() {
            return true;
        }
    }
    false
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
                persist_source: None,
                legacy_persist_loaded: false,
            },
        };

        let session = ManagedSshSession::from_auth(&auth);

        assert_eq!(session.control_path, "/tmp/homeboy-session-%h-%p-%r");
        assert_eq!(session.persist, "30m");
        assert_eq!(
            session.persist_source,
            ManagedSshSessionPersistSource::Configured
        );
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
        assert_eq!(session.persist, LEGACY_DEFAULT_PERSIST);
        assert_eq!(
            session.persist_source,
            ManagedSshSessionPersistSource::LegacyDefault
        );
    }

    #[test]
    fn test_ensure_control_path_parent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let control_path = dir.path().join("nested/control");

        ensure_control_path_parent(&control_path.to_string_lossy()).expect("create parent");

        assert!(control_path.parent().expect("parent").exists());
    }

    #[test]
    fn validate_persist_accepts_openssh_durations() {
        for persist in ["yes", "no", "0", "4h", "1h30m"] {
            validate_persist(persist).expect(persist);
        }
    }

    #[test]
    fn validate_persist_rejects_malformed_values() {
        for persist in ["", "four hours", "1x", "01:30:00", "1h-30m"] {
            assert!(
                validate_persist(persist).is_err(),
                "{persist} should be rejected"
            );
        }
    }
}
