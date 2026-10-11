use homeboy_engine_primitives::content_hash;

use homeboy_core::config::ConfigEntity;
use homeboy_core::error::{Error, Result};

use super::types::*;
use super::validation::{validate_service_tunnel, validate_service_tunnel_in_root};
use super::{load, save};

#[allow(
    dead_code,
    reason = "no production caller; exercised by tunnel_tests / preview_ingress_tests"
)]
pub(crate) fn native_preview_token_sha256(token: &str) -> String {
    content_hash::sha256_hex(token.as_bytes())
}

#[allow(
    dead_code,
    reason = "no production caller; exercised by tunnel_tests / preview_ingress_tests"
)]
pub(crate) fn native_preview_token_record(
    id: impl Into<String>,
    token: &str,
) -> ServiceTunnelNativePreviewToken {
    ServiceTunnelNativePreviewToken {
        id: id.into(),
        token_sha256: native_preview_token_sha256(token),
        allowed_clients: Vec::new(),
        allowed_public_hosts: Vec::new(),
        allowed_session_ids: Vec::new(),
        revoked: false,
        expires_at: None,
    }
}

impl ConfigEntity for ServiceTunnel {
    const ENTITY_TYPE: &'static str = "service_tunnel";
    const DIR_NAME: &'static str = "service-tunnels";

    fn id(&self) -> &str {
        &self.id
    }

    fn set_id(&mut self, id: String) {
        self.id = id;
    }

    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::service_tunnel_not_found(id, suggestions)
    }

    // `config_path` is deliberately not overridden, and neither is
    // `config_path_in_root`. The override this replaces was
    // `paths::homeboy()?.join("service-tunnels").join("{id}.json")` — byte for
    // byte what the trait default produces, because `DIR_NAME` is already
    // `"service-tunnels"`. It was this crate's only ambient root resolution.
    //
    // Deleting it matters twice over. It moved the reach to
    // `ConfigEntity::config_dir`, the single place the campaign roots it once
    // for every entity; and because the rooted default now resolves
    // `{config_root}/service-tunnels/{id}.json`, an ambient override spelling
    // the same path from process-global state would have *shadowed* the root
    // the generic CRUD layer supplies — resolving correctly under ambient use
    // and silently wrongly under an injected root (#7505). `Runner`, `Fleet`,
    // and `Schedule` rely on the same default for the same reason.

    fn validate_in_root(&self, config_root: &std::path::Path) -> Result<()> {
        validate_service_tunnel_in_root(config_root, self)
    }

    fn aliases(&self) -> &[String] {
        &self.aliases
    }
}

pub fn expose(spec: ExposeServiceTunnelSpec) -> Result<ServiceTunnel> {
    let server_id = if spec.runner_local || is_runner_local_server_id(&spec.server_id) {
        RUNNER_LOCAL_SERVICE_SERVER_ID.to_string()
    } else {
        spec.server_id
    };
    let tunnel = ServiceTunnel {
        id: spec.id,
        aliases: Vec::new(),
        description: spec.description,
        server_id,
        target: spec.target,
        scheme: spec.scheme,
        local_host: default_local_host(),
        local_port: spec.local_port,
        auth: spec.auth,
        policy: spec.policy,
    };
    validate_service_tunnel(&tunnel)?;
    save(&tunnel)?;
    load(&tunnel.id)
}
