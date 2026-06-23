use crate::core::error::{Error, Result};
use crate::core::plan::{
    HomeboyPlan, PlanArtifact, PlanKind, PlanStep, PlanStepStatus, PlanValues,
};

use super::types::{
    PreviewIngressInstallCheck, PreviewIngressInstallCheckStatus, PreviewIngressInstallOptions,
    PreviewIngressInstallPlan, PreviewIngressInstallStatusPlan, PreviewIngressServeSpec,
    PreviewIngressWrite,
};

pub fn render_install_plan(
    options: PreviewIngressInstallOptions,
) -> Result<PreviewIngressInstallPlan> {
    let normalized = normalize_install_options(options)?;
    let dns_probe_host = dns_probe_host(&normalized.public_host_pattern, &normalized.domain);
    let local_status_url = format!("http://{}/_homeboy/preview-ingress/status", normalized.bind);
    let public_status_url = format!("https://{}/_homeboy/preview-ingress/status", dns_probe_host);

    let mut install_plan = PreviewIngressInstallPlan {
        command: "tunnel.preview_ingress.install".to_string(),
        plan: HomeboyPlan::default(),
        server_id: normalized.server_id.clone(),
        domain: normalized.domain.clone(),
        public_host_pattern: normalized.public_host_pattern.clone(),
        dns_probe_host: dns_probe_host.clone(),
        bind: normalized.bind.clone(),
        service_name: normalized.service_name.clone(),
        identity: normalized.identity.clone(),
        binary_path: normalized.binary_path.clone(),
        local_status_url: local_status_url.clone(),
        public_status_url: public_status_url.clone(),
        dry_run: true,
        applied: false,
        writes: vec![
            PreviewIngressWrite {
                path: format!("/etc/systemd/system/{}.service", normalized.service_name),
                description: "systemd unit for the Homeboy preview ingress daemon".to_string(),
            },
            PreviewIngressWrite {
                path: format!("/etc/nginx/sites-available/{}", normalized.service_name),
                description: "optional Nginx reverse proxy snippet for the wildcard preview host"
                    .to_string(),
            },
            PreviewIngressWrite {
                path: format!("/etc/caddy/sites/{}.caddy", normalized.service_name),
                description: "optional Caddy reverse proxy snippet for the wildcard preview host"
                    .to_string(),
            },
        ],
        systemd_unit: render_systemd_unit(&normalized),
        nginx_site: render_nginx_site(&normalized),
        caddy_site: render_caddy_site(&normalized),
        install_commands: install_commands(&normalized),
        status_commands: install_status_commands(&normalized, &dns_probe_host),
        rollback_commands: rollback_commands(&normalized),
        smoke_checks: vec![
            format!("getent hosts {dns_probe_host}"),
            format!("curl -fsS {local_status_url}"),
            format!("curl -fsS {public_status_url}"),
        ],
        required_operator_config: required_operator_config(&normalized),
        secrets_policy: vec![
            "Do not put token material in the systemd unit or proxy snippets.".to_string(),
            "Store ingress pairing/client secrets through Homeboy secret/config surfaces before enabling live routes.".to_string(),
            "This generated plan contains only non-secret operator configuration.".to_string(),
        ],
    };
    install_plan.plan = install_homeboy_plan(&install_plan);

    Ok(install_plan)
}

pub fn render_install_status_plan(
    options: PreviewIngressInstallOptions,
) -> Result<PreviewIngressInstallStatusPlan> {
    let normalized = normalize_install_options(options)?;
    let dns_probe_host = dns_probe_host(&normalized.public_host_pattern, &normalized.domain);
    let local_status_url = format!("http://{}/_homeboy/preview-ingress/status", normalized.bind);
    let public_status_url = format!("https://{}/_homeboy/preview-ingress/status", dns_probe_host);
    let commands = install_status_commands(&normalized, &dns_probe_host);

    let mut status_plan = PreviewIngressInstallStatusPlan {
        command: "tunnel.preview_ingress.install_status".to_string(),
        plan: HomeboyPlan::default(),
        server_id: normalized.server_id,
        domain: normalized.domain,
        public_host_pattern: normalized.public_host_pattern,
        dns_probe_host,
        bind: normalized.bind,
        service_name: normalized.service_name,
        local_status_url,
        public_status_url,
        probed: false,
        checks: commands
            .iter()
            .map(|command| PreviewIngressInstallCheck {
                name: install_check_name(command),
                command: command.clone(),
                status: PreviewIngressInstallCheckStatus::Planned,
                exit_code: None,
                stdout: None,
                stderr: None,
            })
            .collect(),
        status_commands: commands,
    };
    status_plan.plan = install_status_homeboy_plan(&status_plan);

    Ok(status_plan)
}

fn install_homeboy_plan(install: &PreviewIngressInstallPlan) -> HomeboyPlan {
    let mut plan = HomeboyPlan::builder_for_description(
        PlanKind::Custom,
        format!("preview ingress install for {}", install.server_id),
    )
    .mode("preview")
    .inputs(
        PlanValues::new()
            .string("command", &install.command)
            .string("server_id", &install.server_id)
            .string("domain", &install.domain)
            .string("public_host_pattern", &install.public_host_pattern)
            .string("dns_probe_host", &install.dns_probe_host)
            .string("bind", &install.bind)
            .string("service_name", &install.service_name)
            .string("service_user", &install.identity.service_user)
            .string("service_group", &install.identity.service_group)
            .string("binary_path", &install.binary_path),
    )
    .policy_value("would_mutate", serde_json::json!(false))
    .policy_value("dry_run", serde_json::json!(install.dry_run))
    .policy_value("applied", serde_json::json!(install.applied))
    .policy_value("secrets", serde_json::json!(&install.secrets_policy))
    .policy_value(
        "required_operator_config",
        serde_json::json!(&install.required_operator_config),
    )
    .steps(install_plan_steps(install))
    .summarize()
    .build();

    plan.artifacts = install_plan_artifacts(install);
    plan.hints = vec![
        "Preview only: this command renders operator work and does not write remote files."
            .to_string(),
    ];
    plan
}

fn install_status_homeboy_plan(status: &PreviewIngressInstallStatusPlan) -> HomeboyPlan {
    let mut plan = HomeboyPlan::builder_for_description(
        PlanKind::Custom,
        format!("preview ingress install status for {}", status.server_id),
    )
    .mode("preview")
    .inputs(
        PlanValues::new()
            .string("command", &status.command)
            .string("server_id", &status.server_id)
            .string("domain", &status.domain)
            .string("public_host_pattern", &status.public_host_pattern)
            .string("dns_probe_host", &status.dns_probe_host)
            .string("bind", &status.bind)
            .string("service_name", &status.service_name),
    )
    .policy_value("would_mutate", serde_json::json!(false))
    .policy_value("probed", serde_json::json!(status.probed))
    .steps(install_status_plan_steps(status))
    .summarize()
    .build();

    plan.artifacts = vec![plan_artifact(
        "preview_ingress.status_commands",
        None,
        "command_list",
        vec![
            (
                "description",
                serde_json::json!("non-mutating status commands"),
            ),
            ("commands", serde_json::json!(&status.status_commands)),
        ],
    )];
    plan.hints = vec![
        "Status plan only: checks are rendered for operators and are not executed here."
            .to_string(),
    ];
    plan
}

fn install_plan_steps(install: &PreviewIngressInstallPlan) -> Vec<PlanStep> {
    let write_steps = install.writes.iter().enumerate().map(|(index, write)| {
        PlanStep::ready(
            format!("preview_ingress.write.{}", plan_slug(&write.path, index)),
            "preview_ingress.write",
        )
        .label(format!("Plan write: {}", write.description))
        .inputs(
            PlanValues::new()
                .string("path", &write.path)
                .string("description", &write.description),
        )
        .build()
    });

    let grouped_steps = [
        (
            "preview_ingress.install_commands",
            "preview_ingress.command_group",
            "Plan install commands",
            serde_json::json!(&install.install_commands),
        ),
        (
            "preview_ingress.status_commands",
            "preview_ingress.command_group",
            "Plan status commands",
            serde_json::json!(&install.status_commands),
        ),
        (
            "preview_ingress.rollback_commands",
            "preview_ingress.rollback",
            "Plan rollback commands",
            serde_json::json!(&install.rollback_commands),
        ),
        (
            "preview_ingress.smoke_checks",
            "preview_ingress.smoke_check",
            "Plan smoke checks",
            serde_json::json!(&install.smoke_checks),
        ),
        (
            "preview_ingress.operator_config",
            "preview_ingress.operator_config",
            "Plan required operator config",
            serde_json::json!(&install.required_operator_config),
        ),
    ]
    .into_iter()
    .map(|(id, kind, label, values)| {
        PlanStep::ready(id, kind)
            .label(label)
            .inputs(PlanValues::new().json("items", values))
            .build()
    });

    write_steps.chain(grouped_steps).collect()
}

fn install_status_plan_steps(status: &PreviewIngressInstallStatusPlan) -> Vec<PlanStep> {
    status
        .checks
        .iter()
        .enumerate()
        .map(|(index, check)| {
            PlanStep::builder(
                format!(
                    "preview_ingress.status_check.{}",
                    plan_slug(&check.name, index)
                ),
                "preview_ingress.status_check",
                match check.status {
                    PreviewIngressInstallCheckStatus::Planned => PlanStepStatus::Ready,
                    PreviewIngressInstallCheckStatus::Passed => PlanStepStatus::Success,
                    PreviewIngressInstallCheckStatus::Failed => PlanStepStatus::Failed,
                },
            )
            .label(format!("Check {}", check.name))
            .inputs(
                PlanValues::new()
                    .string("name", &check.name)
                    .string("command", &check.command),
            )
            .output_value("exit_code", serde_json::json!(check.exit_code))
            .output_value("stdout", serde_json::json!(check.stdout))
            .output_value("stderr", serde_json::json!(check.stderr))
            .build()
        })
        .collect()
}

fn install_plan_artifacts(install: &PreviewIngressInstallPlan) -> Vec<PlanArtifact> {
    let mut artifacts = vec![
        plan_artifact(
            "preview_ingress.systemd_unit",
            Some(format!(
                "/etc/systemd/system/{}.service",
                install.service_name
            )),
            "systemd_unit",
            vec![
                ("description", serde_json::json!("systemd unit preview")),
                ("content", serde_json::json!(&install.systemd_unit)),
            ],
        ),
        plan_artifact(
            "preview_ingress.nginx_site",
            Some(format!(
                "/etc/nginx/sites-available/{}",
                install.service_name
            )),
            "nginx_site",
            vec![
                ("description", serde_json::json!("Nginx site preview")),
                ("content", serde_json::json!(&install.nginx_site)),
            ],
        ),
        plan_artifact(
            "preview_ingress.caddy_site",
            Some(format!("/etc/caddy/sites/{}.caddy", install.service_name)),
            "caddy_site",
            vec![
                ("description", serde_json::json!("Caddy site preview")),
                ("content", serde_json::json!(&install.caddy_site)),
            ],
        ),
    ];

    artifacts.extend([
        plan_artifact(
            "preview_ingress.install_commands",
            None,
            "command_list",
            vec![("commands", serde_json::json!(&install.install_commands))],
        ),
        plan_artifact(
            "preview_ingress.status_commands",
            None,
            "command_list",
            vec![("commands", serde_json::json!(&install.status_commands))],
        ),
        plan_artifact(
            "preview_ingress.rollback_commands",
            None,
            "command_list",
            vec![("commands", serde_json::json!(&install.rollback_commands))],
        ),
        plan_artifact(
            "preview_ingress.smoke_checks",
            None,
            "smoke_checks",
            vec![("commands", serde_json::json!(&install.smoke_checks))],
        ),
    ]);

    artifacts
}

fn plan_artifact(
    id: impl Into<String>,
    path: Option<String>,
    artifact_type: impl Into<String>,
    data: Vec<(&'static str, serde_json::Value)>,
) -> PlanArtifact {
    PlanArtifact {
        id: id.into(),
        path,
        artifact_type: Some(artifact_type.into()),
        data: data
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    }
}

fn plan_slug(value: &str, fallback: usize) -> String {
    let slug = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    if slug.is_empty() {
        format!("item-{fallback}")
    } else {
        slug.chars().take(72).collect()
    }
}

pub(crate) fn validate_serve_spec(spec: &PreviewIngressServeSpec) -> Result<()> {
    if spec.bind.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "bind",
            "bind address is required",
            None,
            None,
        ));
    }
    if spec.domain.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "domain",
            "operator domain is required",
            None,
            None,
        ));
    }
    if spec.public_host_pattern.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "public_host_pattern",
            "public host pattern is required",
            None,
            None,
        ));
    }
    Ok(())
}

fn normalize_install_options(
    mut options: PreviewIngressInstallOptions,
) -> Result<PreviewIngressInstallOptions> {
    options.domain = trim_scheme(&options.domain)
        .trim_end_matches('/')
        .to_string();
    options.public_host_pattern = options.public_host_pattern.trim().to_string();

    if options.server_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "server",
            "preview ingress install requires a configured Homeboy server id",
            None,
            Some(vec![
                "Create one with: homeboy server create <id> --host <host> --user <user>"
                    .to_string(),
            ]),
        ));
    }
    if options.domain.is_empty() || options.domain.contains('/') {
        return Err(Error::validation_invalid_argument(
            "domain",
            "domain must be a bare operator domain such as example.com",
            Some(options.domain),
            None,
        ));
    }
    if !options.public_host_pattern.contains(&options.domain) {
        return Err(Error::validation_invalid_argument(
            "public_host_pattern",
            "public host pattern must include the operator domain",
            Some(options.public_host_pattern),
            None,
        ));
    }
    if !options.public_host_pattern.contains('*') {
        return Err(Error::validation_invalid_argument(
            "public_host_pattern",
            "public host pattern must include a wildcard label for preview routes",
            Some(options.public_host_pattern),
            Some(vec![format!(
                "Use a value like '*-tunnel.{}'",
                options.domain
            )]),
        ));
    }
    if !options.bind.starts_with("127.") && !options.bind.starts_with("[::1]") {
        return Err(Error::validation_invalid_argument(
            "bind",
            "preview ingress should bind to loopback and be exposed by the reverse proxy",
            Some(options.bind),
            Some(vec!["Use a value like 127.0.0.1:7350".to_string()]),
        ));
    }
    Ok(options)
}

fn trim_scheme(value: &str) -> &str {
    value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))
        .unwrap_or(value)
}

fn dns_probe_host(public_host_pattern: &str, domain: &str) -> String {
    let host = public_host_pattern
        .replace('*', "homeboy-health")
        .replace("..", ".")
        .trim_end_matches('.')
        .trim_start_matches('.')
        .to_string();
    if host.is_empty() {
        format!("homeboy-health.{domain}")
    } else {
        host
    }
}

fn render_systemd_unit(options: &PreviewIngressInstallOptions) -> String {
    format!(
        r#"[Unit]
Description=Homeboy preview ingress
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User={user}
Group={group}
Environment=HOME=/var/lib/homeboy
Environment=XDG_DATA_HOME=/var/lib/homeboy/.local/share
ExecStart={binary} tunnel preview-ingress serve --bind {bind} --domain {domain} --public-host-pattern '{public_host_pattern}' --token-sha256-env HOMEBOY_PREVIEW_TUNNEL_TOKEN_SHA256
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
        user = options.identity.service_user,
        group = options.identity.service_group,
        binary = options.binary_path,
        bind = options.bind,
        domain = options.domain,
        public_host_pattern = options.public_host_pattern,
    )
}

fn render_nginx_site(options: &PreviewIngressInstallOptions) -> String {
    format!(
        r#"server {{
    listen 443 ssl http2;
    server_name {public_host_pattern};

    location / {{
        proxy_pass http://{bind};
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    }}
}}
"#,
        public_host_pattern = options.public_host_pattern,
        bind = options.bind,
    )
}

fn render_caddy_site(options: &PreviewIngressInstallOptions) -> String {
    format!(
        r#"{public_host_pattern} {{
    reverse_proxy http://{bind}
}}
"#,
        public_host_pattern = options.public_host_pattern,
        bind = options.bind,
    )
}

fn install_commands(options: &PreviewIngressInstallOptions) -> Vec<String> {
    vec![
        "sudo install -d -m 0755 /etc/systemd/system".to_string(),
        format!(
            "sudo install -m 0644 <rendered-unit> /etc/systemd/system/{}.service",
            options.service_name
        ),
        "sudo systemctl daemon-reload".to_string(),
        format!("sudo systemctl enable --now {}", options.service_name),
        "install either the rendered Nginx or Caddy reverse proxy snippet, then reload that proxy"
            .to_string(),
    ]
}

fn install_status_commands(
    options: &PreviewIngressInstallOptions,
    dns_probe_host: &str,
) -> Vec<String> {
    vec![
        format!("systemctl is-active {}", options.service_name),
        format!("systemctl status {} --no-pager", options.service_name),
        format!(
            "curl -fsS http://{}/_homeboy/preview-ingress/status",
            options.bind
        ),
        format!("getent hosts {dns_probe_host}"),
        format!(
            "curl -fsS https://{}/_homeboy/preview-ingress/status",
            dns_probe_host
        ),
    ]
}

fn rollback_commands(options: &PreviewIngressInstallOptions) -> Vec<String> {
    vec![
        format!("sudo systemctl disable --now {}", options.service_name),
        format!(
            "sudo rm -f /etc/systemd/system/{}.service",
            options.service_name
        ),
        "sudo systemctl daemon-reload".to_string(),
        "remove the installed Nginx/Caddy site snippet and reload the proxy".to_string(),
    ]
}

fn required_operator_config(options: &PreviewIngressInstallOptions) -> Vec<String> {
    vec![
        format!(
            "Homeboy server `{}` with SSH access to the target VPS",
            options.server_id
        ),
        format!(
            "Wildcard DNS for `{}` pointing at the VPS public address",
            options.public_host_pattern
        ),
        "TLS certificate coverage for the wildcard preview host pattern".to_string(),
        format!(
            "A Homeboy binary installed at `{}` on the VPS",
            options.binary_path
        ),
        format!(
            "System user/group `{}`/`{}` available on the VPS",
            options.identity.service_user, options.identity.service_group
        ),
        "A proxy choice: install the rendered Nginx or Caddy snippet, not both".to_string(),
    ]
}

fn install_check_name(command: &str) -> String {
    if command.starts_with("systemctl is-active") {
        "systemd_active".to_string()
    } else if command.starts_with("systemctl status") {
        "systemd_status".to_string()
    } else if command.starts_with("curl -fsS http://") {
        "local_status".to_string()
    } else if command.starts_with("getent hosts") {
        "dns".to_string()
    } else if command.starts_with("curl -fsS https://") {
        "public_status".to_string()
    } else {
        "command".to_string()
    }
}
