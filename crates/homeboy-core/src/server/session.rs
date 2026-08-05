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
    let valid = matches!(persist, "yes" | "no")
        || persist.split(':').enumerate().all(|(index, part)| {
            !part.is_empty()
                && part.chars().all(|character| character.is_ascii_digit())
                && (index == 0 || part.parse::<u8>().is_ok_and(|value| value < 60))
        })
        || persist
            .split(|character: char| character.is_ascii_alphabetic())
            .zip(
                persist
                    .chars()
                    .filter(|character| character.is_ascii_alphabetic()),
            )
            .all(|(amount, unit)| {
                !amount.is_empty()
                    && amount.chars().all(|character| character.is_ascii_digit())
                    && matches!(
                        unit,
                        's' | 'm' | 'h' | 'd' | 'w' | 'S' | 'M' | 'H' | 'D' | 'W'
                    )
            })
            && persist
                .chars()
                .last()
                .is_some_and(|character| character.is_ascii_alphabetic());

    if valid {
        Ok(())
    } else {
        Err(Error::validation_invalid_argument(
            "auth.persist",
            "must be an OpenSSH ControlPersist value: yes, no, seconds, a duration such as 4h or 1h30m, or HH:MM:SS",
            None,
            None,
        ))
    }
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
        for persist in ["yes", "no", "0", "4h", "1h30m", "01:30:00"] {
            validate_persist(persist).expect(persist);
        }
    }

    #[test]
    fn validate_persist_rejects_malformed_values() {
        for persist in ["", "four hours", "1x", "1:90:00", "1h-30m"] {
            assert!(
                validate_persist(persist).is_err(),
                "{persist} should be rejected"
            );
        }
    }
}
