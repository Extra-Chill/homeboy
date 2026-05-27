use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::core::config::{self, ConfigEntity};
use crate::core::error::{Error, Result};
use crate::core::paths;
use crate::core::server;
use crate::core::{CreateOutput, MergeOutput, RemoveResult};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnel {
    #[serde(skip)]
    pub id: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    pub server_id: String,
    pub target: ServiceTunnelTarget,

    #[serde(default = "default_scheme")]
    pub scheme: String,
    #[serde(default = "default_local_host")]
    pub local_host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_port: Option<u16>,

    pub auth: ServiceTunnelAuth,
    pub policy: ServiceTunnelPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTunnelAuthMode {
    BearerEnv,
    HeaderEnv,
    BasicEnv,
    MutualTls,
    SshOnly,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelAuth {
    pub mode: ServiceTunnelAuthMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelTarget {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTunnelExposure {
    PrivateLoopback,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceTunnelPolicy {
    #[serde(default = "default_exposure")]
    pub exposure: ServiceTunnelExposure,
    #[serde(default = "default_true")]
    pub require_auth: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_clients: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ServiceTunnelStatus {
    pub service_id: String,
    pub declared: bool,
    pub running: bool,
    pub lifecycle: String,
    pub local_url: String,
    pub remote_target: String,
    pub policy: ServiceTunnelPolicy,
}

pub struct ExposeServiceTunnelSpec {
    pub id: String,
    pub server_id: String,
    pub target: ServiceTunnelTarget,
    pub scheme: String,
    pub local_port: Option<u16>,
    pub auth: ServiceTunnelAuth,
    pub policy: ServiceTunnelPolicy,
    pub description: Option<String>,
}

fn default_scheme() -> String {
    "http".to_string()
}

fn default_local_host() -> String {
    "127.0.0.1".to_string()
}

fn default_true() -> bool {
    true
}

fn default_exposure() -> ServiceTunnelExposure {
    ServiceTunnelExposure::PrivateLoopback
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

entity_crud!(ServiceTunnel; list_ids, merge);

pub fn expose(spec: ExposeServiceTunnelSpec) -> Result<ServiceTunnel> {
    let tunnel = ServiceTunnel {
        id: spec.id,
        aliases: Vec::new(),
        description: spec.description,
        server_id: spec.server_id,
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

pub fn status(id: &str) -> Result<ServiceTunnelStatus> {
    let tunnel = load(id)?;
    Ok(service_tunnel_status(&tunnel))
}

pub fn local_url(id: &str) -> Result<String> {
    let tunnel = load(id)?;
    Ok(local_url_for(&tunnel))
}

fn service_tunnel_status(tunnel: &ServiceTunnel) -> ServiceTunnelStatus {
    ServiceTunnelStatus {
        service_id: tunnel.id.clone(),
        declared: true,
        running: false,
        lifecycle: "declared".to_string(),
        local_url: local_url_for(tunnel),
        remote_target: format!("{}:{}", tunnel.target.host, tunnel.target.port),
        policy: tunnel.policy.clone(),
    }
}

fn local_url_for(tunnel: &ServiceTunnel) -> String {
    match tunnel.local_port {
        Some(port) => format!("{}://{}:{}", tunnel.scheme, tunnel.local_host, port),
        None => format!("{}://{}:<auto>", tunnel.scheme, tunnel.local_host),
    }
}

fn validate_service_tunnel(tunnel: &ServiceTunnel) -> Result<()> {
    if !server::exists(&tunnel.server_id) {
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
    if tunnel.local_host != "127.0.0.1" && tunnel.local_host != "localhost" {
        return Err(Error::validation_invalid_argument(
            "local_host",
            "service tunnels may only bind to loopback hosts",
            Some(tunnel.id.clone()),
            Some(vec!["127.0.0.1".to_string(), "localhost".to_string()]),
        ));
    }
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::server::Server;
    use crate::test_support;
    use std::collections::HashMap;

    fn create_server() {
        crate::core::server::save(&Server {
            id: "private-host".to_string(),
            aliases: Vec::new(),
            host: "private.example.test".to_string(),
            user: "tester".to_string(),
            port: 22,
            identity_file: None,
            kind: None,
            auth: None,
            env: HashMap::new(),
            runner: None,
        })
        .expect("save server");
    }

    #[test]
    fn expose_records_private_loopback_declaration_without_running_tunnel() {
        test_support::with_isolated_home(|_| {
            create_server();

            let tunnel = expose(ExposeServiceTunnelSpec {
                id: "context-a8c".to_string(),
                server_id: "private-host".to_string(),
                target: ServiceTunnelTarget {
                    host: "127.0.0.1".to_string(),
                    port: 7331,
                },
                scheme: "http".to_string(),
                local_port: Some(8831),
                auth: ServiceTunnelAuth {
                    mode: ServiceTunnelAuthMode::BearerEnv,
                    env_var: Some("CONTEXTA8C_TOKEN".to_string()),
                    header: Some("Authorization".to_string()),
                },
                policy: ServiceTunnelPolicy {
                    exposure: ServiceTunnelExposure::PrivateLoopback,
                    require_auth: true,
                    allowed_clients: vec!["wp-runtime".to_string()],
                },
                description: Some("Private MCP service".to_string()),
            })
            .expect("expose service");

            assert_eq!(tunnel.id, "context-a8c");
            let report = status("context-a8c").expect("status");
            assert!(report.declared);
            assert!(!report.running);
            assert_eq!(report.local_url, "http://127.0.0.1:8831");
        });
    }

    #[test]
    fn validation_rejects_auth_mode_without_env_var() {
        test_support::with_isolated_home(|_| {
            create_server();
            let err = expose(ExposeServiceTunnelSpec {
                id: "bad".to_string(),
                server_id: "private-host".to_string(),
                target: ServiceTunnelTarget {
                    host: "127.0.0.1".to_string(),
                    port: 7331,
                },
                scheme: "http".to_string(),
                local_port: None,
                auth: ServiceTunnelAuth {
                    mode: ServiceTunnelAuthMode::BearerEnv,
                    env_var: None,
                    header: None,
                },
                policy: ServiceTunnelPolicy {
                    exposure: ServiceTunnelExposure::PrivateLoopback,
                    require_auth: true,
                    allowed_clients: Vec::new(),
                },
                description: None,
            })
            .expect_err("missing auth env should fail");

            assert_eq!(err.code, crate::core::ErrorCode::ValidationInvalidArgument);
            assert!(err.message.contains("auth.env_var"));
        });
    }
}
