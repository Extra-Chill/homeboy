//! Extension-owned notification transport config.
//!
//! An installed extension declares a notification command (a literal argv
//! prefix) that homeboy invokes with typed completion-event arguments. A pure
//! serde config type + its validation contract, shared below core so the
//! extension subsystem and its future crate depend on the slim seam.

use homeboy_error::{Error, Result};
use serde::{Deserialize, Serialize};

pub const NOTIFICATION_TRANSPORT_SCHEMA: &str = "homeboy/notification-transport/v1";
pub const NOTIFICATION_ROUTE_RESOLVER_SCHEMA: &str = "homeboy/notification-route-resolver/v1";
pub const NOTIFICATION_ROUTE_RESOLVER_REQUEST_SCHEMA: &str =
    "homeboy/notification-route-resolver-request/v1";

/// The complete, transport-neutral request sent to a declared route resolver
/// over stdin. Context remains extension-owned and is never serialized here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NotificationRouteResolverRequest {
    pub schema: String,
    pub transport: String,
}

/// A resolver's explicit selection state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NotificationRouteResolverStatus {
    Matched,
    Unmatched,
}

/// The one JSON response a declared resolver writes to stdout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NotificationRouteResolverResponse {
    pub schema: String,
    pub status: NotificationRouteResolverStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    /// Extension-owned names for caller context that would permit a match.
    /// Values are never included, keeping resolver diagnostics safe to persist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_context: Vec<String>,
}

/// Safe, read-only transport metadata exposed by extension discovery surfaces.
/// Invocation argv stays confined to the installed manifest and dispatch path.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NotificationTransportDescriptor {
    pub schema: String,
    pub id: String,
    pub has_route_resolver: bool,
}

/// An extension-owned, bounded literal argv command that may derive a route
/// from the caller context it understands. Homeboy passes a versioned request
/// on stdin and reads one versioned response from stdout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct NotificationRouteResolverConfig {
    #[serde(default = "default_notification_route_resolver_schema")]
    pub schema: String,
    pub command: Vec<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_resolver: Option<NotificationRouteResolverConfig>,
}

fn default_notification_transport_schema() -> String {
    NOTIFICATION_TRANSPORT_SCHEMA.to_string()
}

fn default_notification_route_resolver_schema() -> String {
    NOTIFICATION_ROUTE_RESOLVER_SCHEMA.to_string()
}

impl NotificationTransportConfig {
    pub fn descriptor(&self) -> NotificationTransportDescriptor {
        NotificationTransportDescriptor {
            schema: self.schema.clone(),
            id: self.id.clone(),
            has_route_resolver: self.route_resolver.is_some(),
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
        if let Some(resolver) = &self.route_resolver {
            resolver.validate()?;
        }
        Ok(())
    }
}

impl NotificationRouteResolverConfig {
    pub fn validate(&self) -> Result<()> {
        if self.schema != NOTIFICATION_ROUTE_RESOLVER_SCHEMA {
            return Err(Error::validation_invalid_argument(
                "notification_transports.route_resolver.schema",
                format!("must be {NOTIFICATION_ROUTE_RESOLVER_SCHEMA}"),
                Some(self.schema.clone()),
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
                "notification_transports.route_resolver.command",
                "must be a non-empty literal argv array without empty or NUL values",
                None,
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
            route_resolver: None,
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
                "id": "example.completed",
                "has_route_resolver": false
            })
        );
    }

    #[test]
    fn resolver_requires_a_versioned_literal_argv_contract() {
        let mut config = transport("example.completed");
        config.route_resolver = Some(NotificationRouteResolverConfig {
            schema: NOTIFICATION_ROUTE_RESOLVER_SCHEMA.to_string(),
            command: vec!["resolve-route".to_string()],
        });
        config.validate().expect("valid resolver");
        assert!(config.descriptor().has_route_resolver);

        config.route_resolver.as_mut().unwrap().command.clear();
        assert!(config.validate().is_err());
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
