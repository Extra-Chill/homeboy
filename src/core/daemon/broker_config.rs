use serde::Serialize;

use crate::core::error::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerConfigOptions {
    pub listen_addr: String,
    pub binary_path: String,
    pub service_user: String,
    pub service_group: String,
    pub domain: Option<String>,
}

impl Default for BrokerConfigOptions {
    fn default() -> Self {
        Self {
            listen_addr: "127.0.0.1:7421".to_string(),
            binary_path: "/usr/local/bin/homeboy".to_string(),
            service_user: "homeboy".to_string(),
            service_group: "homeboy".to_string(),
            domain: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BrokerConfig {
    pub command: String,
    pub listen_addr: String,
    pub service_name: String,
    pub service_user: String,
    pub service_group: String,
    pub binary_path: String,
    pub daemon_state_path: String,
    pub daemon_jobs_path: String,
    pub safe_exposure: BrokerExposure,
    pub systemd_unit: String,
    pub private_tunnel_examples: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nginx_site: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caddy_site: Option<String>,
    pub operational_commands: Vec<String>,
    pub restart_caveats: Vec<String>,
    pub retention_expectations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct BrokerExposure {
    pub loopback_only: bool,
    pub private_tunnel_safe: bool,
    pub public_reverse_proxy_blocked_by: String,
    pub reason: String,
}

pub fn render_broker_config(options: BrokerConfigOptions) -> Result<BrokerConfig> {
    let listen = parse_loopback_addr(&options.listen_addr)?;
    let host = listen
        .split_once(':')
        .map(|(host, _)| host)
        .unwrap_or(options.listen_addr.as_str());
    let port = listen
        .rsplit_once(':')
        .map(|(_, port)| port)
        .unwrap_or("7421");
    let domain = options.domain.clone();

    Ok(BrokerConfig {
        command: "daemon.broker_config".to_string(),
        listen_addr: listen.to_string(),
        service_name: "homeboy-broker".to_string(),
        service_user: options.service_user.clone(),
        service_group: options.service_group.clone(),
        binary_path: options.binary_path.clone(),
        daemon_state_path: "/var/lib/homeboy/.config/homeboy/daemon/state.json".to_string(),
        daemon_jobs_path: "/var/lib/homeboy/.config/homeboy/daemon/jobs.json".to_string(),
        safe_exposure: BrokerExposure {
            loopback_only: true,
            private_tunnel_safe: true,
            public_reverse_proxy_blocked_by: "https://github.com/Extra-Chill/homeboy/issues/2990".to_string(),
            reason: "Current broker routes do not enforce production auth/pairing, so keep the service on loopback and reach it through a private tunnel until #2990 lands.".to_string(),
        },
        systemd_unit: render_systemd_unit(&options, &listen),
        private_tunnel_examples: vec![
            format!(
                "ssh -N -L {port}:{host}:{port} <vps-user>@<vps-host>"
            ),
            format!(
                "cloudflared tunnel --url http://{listen} # protect with Zero Trust before use"
            ),
            format!(
                "tailscale funnel is not recommended until #2990; use tailnet-only access to http://{listen}"
            ),
        ],
        nginx_site: domain.as_ref().map(|domain| render_nginx_site(domain, &listen)),
        caddy_site: domain.as_ref().map(|domain| render_caddy_site(domain, &listen)),
        operational_commands: vec![
            "systemctl status homeboy-broker".to_string(),
            "journalctl -u homeboy-broker -f".to_string(),
            "homeboy daemon status".to_string(),
            format!("curl -fsS -X POST http://{listen}/runner/jobs/reconcile"),
            format!("curl -fsS http://{listen}/health"),
            "homeboy runner status <runner-id>".to_string(),
        ],
        restart_caveats: vec![
            "The durable job store is reopened from /var/lib/homeboy/.config/homeboy/daemon/jobs.json after restart.".to_string(),
            "Queued remote-runner jobs survive daemon restart; running broker-owned jobs are marked failed as stale.".to_string(),
            "Run POST /runner/jobs/reconcile before broker maintenance to let the broker fail expired reverse-runner claims explicitly.".to_string(),
            "Active reverse-runner claims remain claim-scoped until their lease expires; runners should reclaim after the broker reconciliation window.".to_string(),
            "Use systemctl restart homeboy-broker only when runners can tolerate lease expiry or retry.".to_string(),
        ],
        retention_expectations: vec![
            "Job events are retained in the daemon durable job store with Homeboy's bounded per-job event retention.".to_string(),
            "The store is operational state, not an audit archive; mirror important run evidence into Homeboy observations/artifacts.".to_string(),
            "Back up /var/lib/homeboy/.config/homeboy/daemon/jobs.json before broker maintenance if in-flight jobs matter.".to_string(),
        ],
    })
}

fn parse_loopback_addr(addr: &str) -> Result<String> {
    let parsed = super::parse_bind_addr(addr)?;
    if parsed.port() == 0 {
        return Err(Error::validation_invalid_argument(
            "listen_addr",
            "broker service config requires a stable loopback port",
            Some(addr.to_string()),
            Some(vec!["Use a value like 127.0.0.1:7421".to_string()]),
        ));
    }
    Ok(parsed.to_string())
}

fn render_systemd_unit(options: &BrokerConfigOptions, listen_addr: &str) -> String {
    format!(
        r#"[Unit]
Description=Homeboy reverse runner broker
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User={user}
Group={group}
Environment=HOME=/var/lib/homeboy
Environment=XDG_DATA_HOME=/var/lib/homeboy/.local/share
ExecStart={binary} daemon serve --addr {listen_addr}
Restart=on-failure
RestartSec=5s
StateDirectory=homeboy
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=read-only

[Install]
WantedBy=multi-user.target
"#,
        user = options.service_user,
        group = options.service_group,
        binary = options.binary_path,
    )
}

fn render_nginx_site(domain: &str, listen_addr: &str) -> String {
    format!(
        r#"# Blocked for public Internet use until Homeboy broker auth/pairing lands in #2990.
# Use only behind private network controls or keep this site disabled.
server {{
    listen 443 ssl http2;
    server_name {domain};

    location / {{
        proxy_pass http://{listen_addr};
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    }}
}}
"#
    )
}

fn render_caddy_site(domain: &str, listen_addr: &str) -> String {
    format!(
        r#"# Blocked for public Internet use until Homeboy broker auth/pairing lands in #2990.
# Use only behind private network controls or keep this site disabled.
{domain} {{
    reverse_proxy http://{listen_addr}
}}
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_config_renders_loopback_systemd_service() {
        let config = render_broker_config(BrokerConfigOptions::default()).expect("config");

        assert_eq!(config.listen_addr, "127.0.0.1:7421");
        assert!(config
            .systemd_unit
            .contains("ExecStart=/usr/local/bin/homeboy daemon serve --addr 127.0.0.1:7421"));
        assert!(config.systemd_unit.contains("Restart=on-failure"));
        assert!(config.safe_exposure.loopback_only);
        assert!(config
            .operational_commands
            .iter()
            .any(|command| command.contains("/runner/jobs/reconcile")));
        assert!(config
            .safe_exposure
            .public_reverse_proxy_blocked_by
            .ends_with("/2990"));
        assert!(config.nginx_site.is_none());
    }

    #[test]
    fn broker_config_renders_proxy_snippets_as_blocked_until_auth() {
        let config = render_broker_config(BrokerConfigOptions {
            domain: Some("broker.example.com".to_string()),
            ..BrokerConfigOptions::default()
        })
        .expect("config");

        let nginx = config.nginx_site.expect("nginx");
        let caddy = config.caddy_site.expect("caddy");

        assert!(nginx.contains("broker.example.com"));
        assert!(nginx.contains("proxy_pass http://127.0.0.1:7421"));
        assert!(nginx.contains("#2990"));
        assert!(caddy.contains("reverse_proxy http://127.0.0.1:7421"));
        assert!(caddy.contains("#2990"));
    }

    #[test]
    fn broker_config_rejects_ephemeral_or_public_binds() {
        let ephemeral = render_broker_config(BrokerConfigOptions {
            listen_addr: "127.0.0.1:0".to_string(),
            ..BrokerConfigOptions::default()
        })
        .expect_err("ephemeral port rejected");
        assert!(ephemeral.message.contains("stable loopback port"));

        let public = render_broker_config(BrokerConfigOptions {
            listen_addr: "0.0.0.0:7421".to_string(),
            ..BrokerConfigOptions::default()
        })
        .expect_err("public bind rejected");
        assert!(public.message.contains("loopback"));
    }
}
