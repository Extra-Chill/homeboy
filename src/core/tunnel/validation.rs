use crate::core::config;
use crate::core::error::{Error, Result};
use crate::core::server;

use super::preview::parse_rfc3339_utc;
use super::types::*;

pub(super) fn validate_service_tunnel(tunnel: &ServiceTunnel) -> Result<()> {
    if tunnel.server_id != RUNNER_LOCAL_SERVICE_SERVER_ID && !server::exists(&tunnel.server_id) {
        let suggestions = config::find_similar_ids::<server::Server>(&tunnel.server_id);
        return Err(Error::server_not_found(
            tunnel.server_id.clone(),
            suggestions,
        ));
    }
    if tunnel.target.host.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "target.host",
            "remote host is required",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    if tunnel.target.port == 0 {
        return Err(Error::validation_invalid_argument(
            "target.port",
            "remote port must be greater than zero",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    validate_loopback_host(&tunnel.local_host, &tunnel.id)?;
    if !matches!(
        tunnel.policy.exposure,
        ServiceTunnelExposure::PrivateLoopback
    ) {
        return Err(Error::validation_invalid_argument(
            "policy.exposure",
            "only private_loopback exposure is supported",
            Some(tunnel.id.clone()),
            Some(vec!["private_loopback".to_string()]),
        ));
    }
    if !tunnel.policy.require_auth {
        return Err(Error::validation_invalid_argument(
            "policy.require_auth",
            "service tunnels must require explicit auth policy",
            Some(tunnel.id.clone()),
            Some(vec!["true".to_string()]),
        ));
    }
    if matches!(
        tunnel.auth.mode,
        ServiceTunnelAuthMode::BearerEnv
            | ServiceTunnelAuthMode::HeaderEnv
            | ServiceTunnelAuthMode::BasicEnv
    ) && tunnel
        .auth
        .env_var
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return Err(Error::validation_invalid_argument(
            "auth.env_var",
            "selected auth mode requires an environment variable name",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    if matches!(
        tunnel.policy.preview.mode,
        ServiceTunnelPreviewPolicyMode::KeepAliveUntil
    ) {
        let Some(expires_at) = tunnel.policy.preview.keep_alive_until.as_deref() else {
            return Err(Error::validation_invalid_argument(
                "policy.preview.keep_alive_until",
                "keep_alive_until preview policy requires an RFC3339 expiry",
                Some(tunnel.id.clone()),
                None,
            ));
        };
        if parse_rfc3339_utc(expires_at).is_none() {
            return Err(Error::validation_invalid_argument(
                "policy.preview.keep_alive_until",
                "preview expiry must be a valid RFC3339 timestamp",
                Some(tunnel.id.clone()),
                None,
            ));
        }
    }
    validate_native_preview_auth_policy(&tunnel.policy.native_preview_auth, &tunnel.id)?;
    Ok(())
}

fn validate_native_preview_auth_policy(
    policy: &ServiceTunnelNativePreviewAuthPolicy,
    id: &str,
) -> Result<()> {
    if policy.default_session_ttl_secs == 0 || policy.max_session_ttl_secs == 0 {
        return Err(Error::validation_invalid_argument(
            "policy.native_preview_auth.ttl",
            "native preview session TTLs must be greater than zero",
            Some(id.to_string()),
            None,
        ));
    }
    if policy.default_session_ttl_secs > policy.max_session_ttl_secs {
        return Err(Error::validation_invalid_argument(
            "policy.native_preview_auth.default_session_ttl_secs",
            "default native preview session TTL cannot exceed max_session_ttl_secs",
            Some(id.to_string()),
            None,
        ));
    }
    for token in &policy.tokens {
        if token.id.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "policy.native_preview_auth.tokens.id",
                "native preview token id is required",
                Some(id.to_string()),
                None,
            ));
        }
        if token.token_sha256.len() != 64
            || !token
                .token_sha256
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(Error::validation_invalid_argument(
                "policy.native_preview_auth.tokens.token_sha256",
                "native preview tokens store a SHA-256 digest, not plaintext token material",
                Some(token.id.clone()),
                None,
            ));
        }
        if token
            .expires_at
            .as_deref()
            .is_some_and(|expires_at| parse_rfc3339_utc(expires_at).is_none())
        {
            return Err(Error::validation_invalid_argument(
                "policy.native_preview_auth.tokens.expires_at",
                "native preview token expiry must be a valid RFC3339 timestamp",
                Some(token.id.clone()),
                None,
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_loopback_host(host: &str, id: &str) -> Result<()> {
    if host != "127.0.0.1" && host != "localhost" {
        return Err(Error::validation_invalid_argument(
            "local_host",
            "service tunnels may only bind to loopback hosts",
            Some(id.to_string()),
            Some(vec!["127.0.0.1".to_string(), "localhost".to_string()]),
        ));
    }
    Ok(())
}

pub(super) fn validate_backend_spec(spec: &StartServiceTunnelSpec) -> Result<()> {
    match spec.backend {
        ServiceTunnelTunnelBackend::None => Ok(()),
        ServiceTunnelTunnelBackend::Command => {
            require_backend_value(
                "public_tunnel_command",
                spec.backend_command.as_deref(),
                "command backend requires a backend command",
                &spec.id,
            )?;
            require_backend_value(
                "public_tunnel_public_url",
                spec.backend_public_url.as_deref(),
                "command backend requires the public URL it exposes",
                &spec.id,
            )?;
            Ok(())
        }
    }
}

fn require_backend_value(field: &str, value: Option<&str>, message: &str, id: &str) -> Result<()> {
    if value.unwrap_or_default().trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            field,
            message,
            Some(id.to_string()),
            None,
        ));
    }
    Ok(())
}
