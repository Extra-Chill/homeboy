//! Versioned, extension-owned detail hydration for external CI checks.

use homeboy_error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Component, Path};

pub const EXTERNAL_CHECK_DETAIL_RESOLVER_SCHEMA: &str = "homeboy/external-check-detail-resolver/v1";
pub const EXTERNAL_CHECK_DETAIL_REQUEST_SCHEMA: &str = "homeboy/external-check-detail-request/v1";
pub const EXTERNAL_CHECK_DETAIL_RESPONSE_SCHEMA: &str = "homeboy/external-check-detail-response/v1";

/// Literal-argv resolver declaration. `provider` is an exact, case-sensitive
/// matcher for the normalized check provider; it is not a glob or a command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExternalCheckDetailResolverConfig {
    #[serde(default = "default_schema")]
    pub schema: String,
    pub provider: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub public_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secret_env: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ExternalCheckDetailResolverDeclaration {
    Config(Box<ExternalCheckDetailResolverConfig>),
    Malformed(Value),
}

impl ExternalCheckDetailResolverDeclaration {
    pub fn declared_provider(&self) -> Option<String> {
        match self {
            Self::Config(config) => Some(config.provider.clone()),
            Self::Malformed(value) => value
                .as_object()
                .and_then(|object| object.get("provider"))
                .and_then(Value::as_str)
                .map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExternalCheckDetailRequest {
    pub schema: String,
    pub provider: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExternalCheckDetailResponse {
    pub schema: String,
    pub provider: String,
    /// Provider-defined build identity for joining the hydrated detail to its UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log_refs: Vec<String>,
}

fn default_schema() -> String {
    EXTERNAL_CHECK_DETAIL_RESOLVER_SCHEMA.to_string()
}

impl ExternalCheckDetailResolverConfig {
    pub fn validate(&self) -> Result<()> {
        if self.schema != EXTERNAL_CHECK_DETAIL_RESOLVER_SCHEMA {
            return Err(invalid(
                "schema",
                format!("must be {EXTERNAL_CHECK_DETAIL_RESOLVER_SCHEMA}"),
            ));
        }
        if !valid_name(&self.provider) {
            return Err(invalid(
                "provider",
                "must be a non-empty ASCII provider identifier",
            ));
        }
        if self.command.is_empty()
            || self
                .command
                .iter()
                .any(|arg| arg.is_empty() || arg.contains('\0'))
        {
            return Err(invalid(
                "command",
                "must be a non-empty literal argv array without empty or NUL values",
            ));
        }
        if Path::new(&self.command[0]).is_absolute()
            || Path::new(&self.command[0])
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(invalid(
                "command",
                "program must be a relative path contained by the declaring extension",
            ));
        }
        let mut public_names = HashSet::new();
        let mut secret_names = HashSet::new();
        for name in &self.public_env {
            if !valid_env_name(name) {
                return Err(invalid(
                    "public_env/secret_env",
                    "must contain valid environment variable names",
                ));
            }
            if !public_names.insert(name) {
                return Err(invalid("public_env", "must not contain duplicate names"));
            }
        }
        for name in &self.secret_env {
            if !valid_env_name(name) {
                return Err(invalid(
                    "public_env/secret_env",
                    "must contain valid environment variable names",
                ));
            }
            if !secret_names.insert(name) {
                return Err(invalid("secret_env", "must not contain duplicate names"));
            }
        }
        if self
            .public_env
            .iter()
            .any(|name| self.secret_env.contains(name))
        {
            return Err(invalid(
                "public_env/secret_env",
                "must not project an identity as both public and secret",
            ));
        }
        Ok(())
    }
}

fn invalid(field: &str, message: impl Into<String>) -> Error {
    Error::validation_invalid_argument(
        format!("external_check_detail_resolvers.{field}"),
        message.into(),
        None,
        None,
    )
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_env_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_requires_versioned_literal_argv_and_disjoint_projection() {
        let mut resolver = ExternalCheckDetailResolverConfig {
            schema: EXTERNAL_CHECK_DETAIL_RESOLVER_SCHEMA.into(),
            provider: "example-ci".into(),
            command: vec!["resolver".into()],
            public_env: vec!["PUBLIC".into()],
            secret_env: vec!["TOKEN".into()],
        };
        resolver.validate().unwrap();
        resolver.secret_env = vec!["PUBLIC".into()];
        assert!(resolver.validate().is_err());
        resolver.secret_env = vec!["TOKEN".into(), "TOKEN".into()];
        assert!(resolver.validate().is_err());
    }
}
