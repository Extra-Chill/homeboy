use sha2::{Digest, Sha256};
use std::path::PathBuf;

use crate::core::config::ConfigEntity;
use crate::core::error::{Error, Result};
use crate::core::paths;

use super::types::*;
use super::validation::validate_service_tunnel;
use super::{load, save};

pub fn native_preview_token_sha256(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    format!("{digest:x}")
}

pub fn native_preview_token_record(
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

    fn config_path(id: &str) -> Result<PathBuf> {
        Ok(paths::homeboy()?
            .join("service-tunnels")
            .join(format!("{}.json", id)))
    }

    fn validate(&self) -> Result<()> {
        validate_service_tunnel(self)
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
