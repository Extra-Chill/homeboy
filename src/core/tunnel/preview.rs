use crate::core::error::{Error, Result};

use super::entity::native_preview_token_sha256;
use super::types::*;
use super::validation::validate_loopback_host;

pub(in crate::core) fn preview_policy_allows(
    policy: &ServiceTunnelPreviewPolicy,
    context: &ServiceTunnelPreviewDecisionContext,
) -> bool {
    match policy.mode {
        ServiceTunnelPreviewPolicyMode::None => false,
        ServiceTunnelPreviewPolicyMode::Always => true,
        ServiceTunnelPreviewPolicyMode::OnFailure => context.run_failed,
        ServiceTunnelPreviewPolicyMode::ManualApproval => context.manual_approval_required,
        ServiceTunnelPreviewPolicyMode::KeepAliveUntil => policy
            .keep_alive_until
            .as_deref()
            .and_then(parse_rfc3339_utc)
            .is_some_and(|expires_at| context.now <= expires_at),
    }
}

pub(in crate::core) fn preview_artifact_for(
    tunnel: &ServiceTunnel,
    state: &ServiceTunnelRuntimeState,
    context: &ServiceTunnelPreviewDecisionContext,
) -> Option<ServiceTunnelPreviewArtifact> {
    if !preview_policy_allows(&tunnel.policy.preview, context) {
        return None;
    }

    Some(ServiceTunnelPreviewArtifact {
        schema: "homeboy/preview-url/v1".to_string(),
        kind: "preview_url".to_string(),
        preview_identity: ServiceTunnelPreviewIdentity {
            service_id: tunnel.id.clone(),
            public_url: state.preview_identity.public_url.clone(),
        },
        local_url: state.local_url.clone(),
        backend: state.backend.clone(),
        policy: tunnel.policy.preview.clone(),
        cleanup: preview_cleanup_metadata(&tunnel.policy.preview),
        source: ServiceTunnelPreviewSource {
            run_id: state.source_run_id.clone(),
            workflow_id: state.source_workflow_id.clone(),
        },
    })
}

pub fn validate_native_preview_claim(
    tunnel: &ServiceTunnel,
    request: ServiceTunnelNativePreviewClaimRequest,
) -> Result<ServiceTunnelNativePreviewClaim> {
    let policy = &tunnel.policy.native_preview_auth;
    if !policy.require_client_token {
        return Err(preview_auth_error(
            "auth",
            "native preview ingress requires client token authentication",
            Some(tunnel.id.clone()),
            Some(vec!["set require_client_token=true".to_string()]),
        ));
    }
    if request.client_id.trim().is_empty() {
        return Err(preview_auth_error(
            "client_id",
            "preview client id is required",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    if request.token.trim().is_empty() {
        return Err(preview_auth_error(
            "token",
            "preview client token is required",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    if request.public_host.trim().is_empty() {
        return Err(preview_auth_error(
            "public_host",
            "preview public host claim is required",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    if request.session_id.trim().is_empty() {
        return Err(preview_auth_error(
            "session_id",
            "preview session id claim is required",
            Some(tunnel.id.clone()),
            None,
        ));
    }
    validate_native_preview_local_origin(&request.local_origin, &tunnel.id)?;

    let token_hash = native_preview_token_sha256(&request.token);
    let Some(token) = policy
        .tokens
        .iter()
        .find(|candidate| candidate.token_sha256 == token_hash)
    else {
        return Err(preview_auth_error(
            "token",
            "preview client token is not recognized",
            Some(tunnel.id.clone()),
            None,
        ));
    };

    if token.revoked {
        return Err(preview_auth_error(
            "token",
            "preview client token is revoked",
            Some(token.id.clone()),
            None,
        ));
    }
    if let Some(expires_at) = token.expires_at.as_deref().and_then(parse_rfc3339_utc) {
        if request.now > expires_at {
            return Err(preview_auth_error(
                "token",
                "preview client token is expired",
                Some(token.id.clone()),
                None,
            ));
        }
    }
    if !string_claim_allowed(&request.client_id, &token.allowed_clients) {
        return Err(preview_auth_error(
            "client_id",
            "preview client is not authorized for this token",
            Some(request.client_id),
            Some(token.allowed_clients.clone()),
        ));
    }
    if !host_claim_allowed(&request.public_host, &policy.allowed_public_hosts)
        || !host_claim_allowed(&request.public_host, &token.allowed_public_hosts)
    {
        return Err(preview_auth_error(
            "public_host",
            "preview token is not authorized to claim this public host",
            Some(request.public_host),
            policy_host_suggestions(policy, token),
        ));
    }
    if !string_claim_allowed(&request.session_id, &policy.allowed_session_ids)
        || !string_claim_allowed(&request.session_id, &token.allowed_session_ids)
    {
        return Err(preview_auth_error(
            "session_id",
            "preview token is not authorized to claim this session id",
            Some(request.session_id),
            policy_session_suggestions(policy, token),
        ));
    }

    let ttl_secs = request
        .requested_ttl_secs
        .unwrap_or(policy.default_session_ttl_secs)
        .min(policy.max_session_ttl_secs);
    let expires_at = request.now + chrono::Duration::seconds(ttl_secs as i64);

    Ok(ServiceTunnelNativePreviewClaim {
        service_id: tunnel.id.clone(),
        client_id: request.client_id,
        token_id: token.id.clone(),
        public_host: request.public_host,
        session_id: request.session_id,
        local_origin: request.local_origin,
        expires_at: expires_at.to_rfc3339(),
    })
}

fn preview_auth_error(
    field: &str,
    message: impl Into<String>,
    value: Option<String>,
    suggestions: Option<Vec<String>>,
) -> Error {
    Error::validation_invalid_argument(field, message, value, suggestions)
}

fn validate_native_preview_local_origin(local_origin: &str, id: &str) -> Result<()> {
    let Some(rest) = local_origin.strip_prefix("http://") else {
        return Err(preview_auth_error(
            "local_origin",
            "preview local origin must use http:// loopback",
            Some(id.to_string()),
            Some(vec!["http://127.0.0.1:<port>".to_string()]),
        ));
    };
    let host = rest.split(['/', ':']).next().unwrap_or_default();
    validate_loopback_host(host, id)
}

fn string_claim_allowed(value: &str, allowed: &[String]) -> bool {
    allowed.is_empty() || allowed.iter().any(|candidate| candidate == value)
}

fn host_claim_allowed(value: &str, allowed: &[String]) -> bool {
    allowed.is_empty()
        || allowed
            .iter()
            .any(|candidate| candidate == value || glob_match::glob_match(candidate, value))
}

fn policy_host_suggestions(
    policy: &ServiceTunnelNativePreviewAuthPolicy,
    token: &ServiceTunnelNativePreviewToken,
) -> Option<Vec<String>> {
    suggestions_from_scopes(&policy.allowed_public_hosts, &token.allowed_public_hosts)
}

fn policy_session_suggestions(
    policy: &ServiceTunnelNativePreviewAuthPolicy,
    token: &ServiceTunnelNativePreviewToken,
) -> Option<Vec<String>> {
    suggestions_from_scopes(&policy.allowed_session_ids, &token.allowed_session_ids)
}

fn suggestions_from_scopes(
    policy_values: &[String],
    token_values: &[String],
) -> Option<Vec<String>> {
    let mut suggestions = Vec::new();
    suggestions.extend(policy_values.iter().cloned());
    suggestions.extend(token_values.iter().cloned());
    if suggestions.is_empty() {
        None
    } else {
        suggestions.sort();
        suggestions.dedup();
        Some(suggestions)
    }
}

pub(super) fn preview_artifact_for_status(
    tunnel: &ServiceTunnel,
    state: &ServiceTunnelRuntimeState,
) -> Option<ServiceTunnelPreviewArtifact> {
    preview_artifact_for(
        tunnel,
        state,
        &ServiceTunnelPreviewDecisionContext {
            run_failed: false,
            manual_approval_required: false,
            now: chrono::Utc::now(),
        },
    )
}

pub(super) fn preview_cleanup_metadata(
    policy: &ServiceTunnelPreviewPolicy,
) -> ServiceTunnelPreviewCleanupMetadata {
    let cleanup_policy = match policy.mode {
        ServiceTunnelPreviewPolicyMode::None => "stop_immediately",
        ServiceTunnelPreviewPolicyMode::Always => "keep_while_running",
        ServiceTunnelPreviewPolicyMode::OnFailure => "keep_on_failure",
        ServiceTunnelPreviewPolicyMode::ManualApproval => "keep_for_manual_approval",
        ServiceTunnelPreviewPolicyMode::KeepAliveUntil => "keep_alive_until",
    };

    ServiceTunnelPreviewCleanupMetadata {
        cleanup_policy: cleanup_policy.to_string(),
        expires_at: policy.keep_alive_until.clone(),
        stop_on_cleanup: true,
    }
}

pub(super) fn parse_rfc3339_utc(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|datetime| datetime.with_timezone(&chrono::Utc))
}
