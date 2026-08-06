//! Extension-owned notification transport config.
//!
//! An installed extension declares a notification command (a literal argv
//! prefix) that homeboy invokes with typed completion-event arguments. A pure
//! serde config type + its validation contract, shared below core so the
//! extension subsystem and its future crate depend on the slim seam.

use homeboy_error::{Error, Result};
use serde::{Deserialize, Serialize};

pub const NOTIFICATION_TRANSPORT_SCHEMA: &str = "homeboy/notification-transport/v1";

/// Safe, read-only transport metadata exposed by extension discovery surfaces.
/// Invocation argv stays confined to the installed manifest and dispatch path.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NotificationTransportDescriptor {
    pub schema: String,
    pub id: String,
}

/// An installed extension-owned notification command. `command` is a literal
/// argv prefix, never a shell command or template. Homeboy appends the typed
/// completion event arguments defined by the schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NotificationTransportConfig {
    #[serde(default = "default_notification_transport_schema")]
    pub schema: String,
    pub id: String,
    pub command: Vec<String>,
}

fn default_notification_transport_schema() -> String {
    NOTIFICATION_TRANSPORT_SCHEMA.to_string()
}

impl NotificationTransportConfig {
    pub fn descriptor(&self) -> NotificationTransportDescriptor {
        NotificationTransportDescriptor {
            schema: self.schema.clone(),
            id: self.id.clone(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != NOTIFICATION_TRANSPORT_SCHEMA {
            return Err(Error::validation_invalid_argument(
                "notification_transports.schema",
                format!("must be {NOTIFICATION_TRANSPORT_SCHEMA}"),
                Some(self.schema.clone()),
                None,
            ));
        }
        let valid_id = !self.id.is_empty()
            && self.id.len() <= 128
            && self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        if !valid_id {
            return Err(Error::validation_invalid_argument(
                "notification_transports.id",
                "must contain 1-128 ASCII letters, digits, '.', '_' or '-'",
                Some(self.id.clone()),
                None,
            ));
        }
        if self.command.is_empty()
            || self
                .command
                .iter()
                .any(|arg| arg.is_empty() || arg.contains('\0'))
        {
            return Err(Error::validation_invalid_argument(
                "notification_transports.command",
                "must be a non-empty literal argv array without empty or NUL values",
                Some(self.id.clone()),
                None,
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transport(id: &str) -> NotificationTransportConfig {
        NotificationTransportConfig {
            schema: NOTIFICATION_TRANSPORT_SCHEMA.to_string(),
            id: id.to_string(),
            command: vec!["notify".to_string()],
        }
    }

    #[test]
    fn descriptor_exposes_only_safe_discovery_metadata() {
        let descriptor = transport("example.completed").descriptor();

        assert_eq!(descriptor.id, "example.completed");
        assert_eq!(descriptor.schema, NOTIFICATION_TRANSPORT_SCHEMA);
        assert_eq!(
            serde_json::to_value(descriptor).expect("serialize descriptor"),
            serde_json::json!({
                "schema": NOTIFICATION_TRANSPORT_SCHEMA,
                "id": "example.completed"
            })
        );
    }

    #[test]
    fn invalid_transport_declaration_reports_the_invalid_field() {
        let error = NotificationTransportConfig {
            schema: "homeboy/notification-transport/v2".to_string(),
            ..transport("example.completed")
        }
        .validate()
        .expect_err("invalid schema must fail");

        assert!(error.to_string().contains("notification_transports.schema"));
    }
}
