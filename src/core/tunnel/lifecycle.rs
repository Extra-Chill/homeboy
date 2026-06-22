use std::fs::{self, File};
use std::process::Stdio;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::core::error::{Error, ErrorCode, Result};
use crate::core::paths;

use super::preview::preview_artifact_for_status;
use super::readiness::{check_runtime_health, check_runtime_readiness, wait_until_ready};
use super::runtime::{
    backend_process_is_running, ensure_supervised_process_still_running, load_runtime_state,
    local_url_for, process_group_id_for, refresh_runtime_state, remove_runtime_state,
    resolve_health_url, runtime_evidence, runtime_state_is_running, save_runtime_state,
    shell_command, terminate_backend_state, terminate_runtime_state,
};
use super::types::*;
use super::validation::{validate_backend_spec, validate_loopback_host, validate_service_tunnel};
use super::{load, save};

pub fn status(id: &str) -> Result<ServiceTunnelStatus> {
    let tunnel = load(id)?;
    service_tunnel_status(&tunnel)
}

pub fn start(spec: StartServiceTunnelSpec) -> Result<ServiceTunnelStatus> {
    let mut tunnel = load_or_materialize_start_tunnel(&spec)?;
    validate_backend_spec(&spec)?;

    let existing = load_runtime_state(&tunnel.id)?;
    if let Some(state) = existing {
        if runtime_state_is_running(&state) {
            return Err(Error::validation_invalid_argument(
                "service",
                "service tunnel is already running; stop it before starting again",
                Some(tunnel.id),
                None,
            ));
        }
    }

    if let Some(host) = spec.host {
        validate_loopback_host(&host, &tunnel.id)?;
        tunnel.local_host = host;
    }
    if let Some(port) = spec.port {
        if port == 0 {
            return Err(Error::validation_invalid_argument(
                "port",
                "local port must be greater than zero",
                Some(tunnel.id),
                None,
            ));
        }
        tunnel.local_port = Some(port);
    }
    if let Some(scheme) = spec.scheme {
        tunnel.scheme = scheme;
    }
    validate_service_tunnel(&tunnel)?;
    save(&tunnel)?;

    let runtime_dir = paths::service_tunnel_runtime_dir(&tunnel.id)?;
    fs::create_dir_all(&runtime_dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some(runtime_dir.display().to_string())))?;
    let stdout_path = runtime_dir.join("stdout.log");
    let stderr_path = runtime_dir.join("stderr.log");
    let stdout = File::create(&stdout_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(stdout_path.display().to_string())))?;
    let stderr = File::create(&stderr_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(stderr_path.display().to_string())))?;

    let mut command = shell_command(&spec.command);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }

    let mut child = command.spawn().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("start service tunnel {}", tunnel.id)),
        )
    })?;
    let pid = child.id();
    let process_group_id = process_group_id_for(pid);
    let health_url = resolve_health_url(&tunnel, spec.health_url, spec.health_path);
    let state = ServiceTunnelRuntimeState {
        preview_identity: ServiceTunnelPreviewIdentity {
            service_id: tunnel.id.clone(),
            public_url: spec.backend_public_url.clone(),
        },
        pid,
        process: ServiceTunnelProcessDescriptor {
            process_group_id,
            command: ServiceTunnelCommandSpec {
                command: spec.command,
                cwd: spec.cwd.map(|path| path.display().to_string()),
                env_keys: spec.env.keys().cloned().collect(),
            },
        },
        started_at: chrono::Utc::now().to_rfc3339(),
        local_url: local_url_for(&tunnel),
        health_url,
        logs: ServiceTunnelLogPaths {
            stdout_path: stdout_path.display().to_string(),
            stderr_path: stderr_path.display().to_string(),
        },
        backend: spec.backend,
        backend_process: None,
        source_run_id: spec.source_run_id,
        source_workflow_id: spec.source_workflow_id,
        readiness_kind: spec.readiness_kind,
        readiness_checks: spec.readiness_checks,
    };
    save_runtime_state(&state)?;
    if let Err(error) = wait_until_ready(&state, spec.readiness_timeout_secs) {
        terminate_runtime_state(&state)?;
        remove_runtime_state(&state.preview_identity.service_id)?;
        return Err(error);
    }

    if let Err(error) = ensure_supervised_process_still_running(&state, &mut child) {
        terminate_runtime_state(&state)?;
        remove_runtime_state(&state.preview_identity.service_id)?;
        return Err(error);
    }

    let state = match start_backend_if_needed(state, &tunnel, spec.backend_command) {
        Ok(state) => state,
        Err(error) => {
            if let Some(state) = load_runtime_state(&tunnel.id)? {
                terminate_runtime_state(&state)?;
                remove_runtime_state(&state.preview_identity.service_id)?;
            }
            return Err(error);
        }
    };

    if let Err(error) = ensure_supervised_process_still_running(&state, &mut child) {
        terminate_backend_state(&state)?;
        terminate_runtime_state(&state)?;
        remove_runtime_state(&state.preview_identity.service_id)?;
        return Err(error);
    }

    save_runtime_state(&state)?;
    status(&tunnel.id)
}

pub(super) fn load_or_materialize_start_tunnel(
    spec: &StartServiceTunnelSpec,
) -> Result<ServiceTunnel> {
    match load(&spec.id) {
        Ok(tunnel) => Ok(tunnel),
        Err(error) if error.code == ErrorCode::ServiceTunnelNotFound => {
            materialize_runner_local_start_tunnel(spec)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn materialize_runner_local_start_tunnel(
    spec: &StartServiceTunnelSpec,
) -> Result<ServiceTunnel> {
    let host = spec.host.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "host",
            "starting an undeclared runner-local service tunnel requires --host",
            Some(spec.id.clone()),
            Some(vec!["Pass --host 127.0.0.1 or declare the service with `homeboy tunnel service expose` before starting it.".to_string()]),
        )
    })?;
    validate_loopback_host(host, &spec.id)?;
    let port = spec.port.ok_or_else(|| {
        Error::validation_invalid_argument(
            "port",
            "starting an undeclared runner-local service tunnel requires --port",
            Some(spec.id.clone()),
            Some(vec!["Pass the local service port or declare the service with `homeboy tunnel service expose` before starting it.".to_string()]),
        )
    })?;
    if port == 0 {
        return Err(Error::validation_invalid_argument(
            "port",
            "local port must be greater than zero",
            Some(spec.id.clone()),
            None,
        ));
    }

    let tunnel = ServiceTunnel {
        id: spec.id.clone(),
        aliases: Vec::new(),
        description: Some("Runner-local service materialized by tunnel service start".to_string()),
        server_id: RUNNER_LOCAL_SERVICE_SERVER_ID.to_string(),
        target: ServiceTunnelTarget {
            host: host.to_string(),
            port,
        },
        scheme: spec.scheme.clone().unwrap_or_else(default_scheme),
        local_host: host.to_string(),
        local_port: Some(port),
        auth: ServiceTunnelAuth {
            mode: ServiceTunnelAuthMode::SshOnly,
            env_var: None,
            header: None,
        },
        policy: ServiceTunnelPolicy {
            exposure: ServiceTunnelExposure::PrivateLoopback,
            require_auth: true,
            allowed_clients: Vec::new(),
            preview: ServiceTunnelPreviewPolicy::default(),
            native_preview_auth: Default::default(),
        },
    };
    validate_service_tunnel(&tunnel)?;
    save(&tunnel)?;
    load(&tunnel.id)
}

pub fn stop(id: &str) -> Result<ServiceTunnelStatus> {
    let tunnel = load(id)?;
    if let Some(state) = load_runtime_state(id)? {
        terminate_backend_state(&state)?;
        terminate_runtime_state(&state)?;
        remove_runtime_state(id)?;
    }
    service_tunnel_status(&tunnel)
}

pub fn local_url(id: &str) -> Result<String> {
    let tunnel = load(id)?;
    Ok(local_url_for(&tunnel))
}

fn service_tunnel_status(tunnel: &ServiceTunnel) -> Result<ServiceTunnelStatus> {
    let live = refresh_runtime_state(&tunnel.id)?;
    let running = live.as_ref().is_some_and(|live| live.running);
    let backend_running = live.as_ref().is_some_and(|live| live.backend_running);
    let state = live.map(|live| live.state);
    let degraded_reason = if !running && backend_running {
        Some("local-origin-process-exited".to_string())
    } else {
        None
    };
    let health = state.as_ref().map(check_runtime_health);
    let readiness = state.as_ref().map(check_runtime_readiness);
    let evidence = state.as_ref().map(runtime_evidence);
    let process = state.as_ref().map(|state| ServiceTunnelProcessStatus {
        pid: state.pid,
        process: state.process.clone(),
        running,
        started_at: state.started_at.clone(),
    });
    let backend = state.as_ref().map(|state| ServiceTunnelBackendStatus {
        backend: state.backend.clone(),
        active: backend_running || state.preview_identity.public_url.is_some(),
        active_reason: if backend_running && !running {
            Some("backend-process-running-after-local-origin-exit".to_string())
        } else if backend_running {
            Some("backend-process-running".to_string())
        } else if state.preview_identity.public_url.is_some() {
            Some("public-url-declared".to_string())
        } else {
            None
        },
        process: state.backend_process.as_ref().map(|backend| {
            let running = backend_process_is_running(backend);
            ServiceTunnelProcessStatus {
                pid: backend.pid,
                process: backend.process.clone(),
                running,
                started_at: backend.started_at.clone(),
            }
        }),
        evidence: state
            .backend_process
            .as_ref()
            .map(|backend| backend.logs.clone()),
    });
    let public_url = state
        .as_ref()
        .and_then(|state| state.preview_identity.public_url.clone());
    let preview = state
        .as_ref()
        .and_then(|state| preview_artifact_for_status(tunnel, state));
    Ok(ServiceTunnelStatus {
        preview_identity: ServiceTunnelPreviewIdentity {
            service_id: tunnel.id.clone(),
            public_url,
        },
        declared: true,
        running,
        lifecycle: if degraded_reason.is_some() {
            "degraded"
        } else if running {
            "running"
        } else {
            "declared"
        }
        .to_string(),
        degraded_reason,
        local_url: local_url_for(tunnel),
        remote_target: format!("{}:{}", tunnel.target.host, tunnel.target.port),
        policy: tunnel.policy.clone(),
        process,
        health,
        readiness,
        evidence,
        tunnel_backend: backend,
        preview,
    })
}

fn start_backend_if_needed(
    mut state: ServiceTunnelRuntimeState,
    tunnel: &ServiceTunnel,
    backend_command: Option<String>,
) -> Result<ServiceTunnelRuntimeState> {
    if !matches!(state.backend, ServiceTunnelTunnelBackend::Command) {
        return Ok(state);
    }

    let command_string = backend_command.unwrap_or_default();
    let runtime_dir = paths::service_tunnel_runtime_dir(&tunnel.id)?;
    let stdout_path = runtime_dir.join("backend-stdout.log");
    let stderr_path = runtime_dir.join("backend-stderr.log");
    let stdout = File::create(&stdout_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(stdout_path.display().to_string())))?;
    let stderr = File::create(&stderr_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(stderr_path.display().to_string())))?;

    let mut command = shell_command(&command_string);
    command
        .env("HOMEBOY_SERVICE_ID", &tunnel.id)
        .env("HOMEBOY_SERVICE_LOCAL_URL", &state.local_url);
    if let Some(public_url) = &state.preview_identity.public_url {
        command.env("HOMEBOY_TUNNEL_PUBLIC_URL", public_url);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            libc::setpgid(0, 0);
            Ok(())
        });
    }

    let child = command.spawn().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("start service tunnel backend {}", tunnel.id)),
        )
    })?;
    let pid = child.id();
    state.backend_process = Some(ServiceTunnelBackendProcessState {
        pid,
        process: ServiceTunnelProcessDescriptor {
            process_group_id: process_group_id_for(pid),
            command: ServiceTunnelCommandSpec {
                command: command_string,
                cwd: None,
                env_keys: vec![
                    "HOMEBOY_SERVICE_ID".to_string(),
                    "HOMEBOY_SERVICE_LOCAL_URL".to_string(),
                    "HOMEBOY_TUNNEL_PUBLIC_URL".to_string(),
                ],
            },
        },
        started_at: chrono::Utc::now().to_rfc3339(),
        logs: ServiceTunnelLogPaths {
            stdout_path: stdout_path.display().to_string(),
            stderr_path: stderr_path.display().to_string(),
        },
    });
    Ok(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn start_spec(id: &str) -> StartServiceTunnelSpec {
        StartServiceTunnelSpec {
            id: id.to_string(),
            command: "node server.js".to_string(),
            cwd: None,
            env: BTreeMap::new(),
            host: Some("127.0.0.1".to_string()),
            port: Some(48631),
            scheme: Some("http".to_string()),
            health_url: None,
            health_path: None,
            readiness_timeout_secs: 1,
            backend: ServiceTunnelTunnelBackend::None,
            backend_command: None,
            backend_public_url: None,
            source_run_id: None,
            source_workflow_id: None,
            readiness_kind: ServiceTunnelReadinessKind::Process,
            readiness_checks: Vec::new(),
        }
    }

    #[test]
    fn start_materializes_runner_local_service_without_server_declaration() {
        crate::test_support::with_isolated_home(|_| {
            let tunnel = materialize_runner_local_start_tunnel(&start_spec("preview-service"))
                .expect("materialize runner-local service");

            assert_eq!(tunnel.id, "preview-service");
            assert_eq!(tunnel.server_id, RUNNER_LOCAL_SERVICE_SERVER_ID);
            assert_eq!(tunnel.target.host, "127.0.0.1");
            assert_eq!(tunnel.target.port, 48631);
            assert_eq!(tunnel.local_port, Some(48631));
            assert!(load("preview-service").is_ok());
        });
    }

    #[test]
    fn undeclared_runner_local_service_requires_host_and_port() {
        crate::test_support::with_isolated_home(|_| {
            let mut missing_host = start_spec("missing-host");
            missing_host.host = None;
            let err =
                materialize_runner_local_start_tunnel(&missing_host).expect_err("host required");
            assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
            assert!(err.message.contains("requires --host"));

            let mut missing_port = start_spec("missing-port");
            missing_port.port = None;
            let err =
                materialize_runner_local_start_tunnel(&missing_port).expect_err("port required");
            assert_eq!(err.code, ErrorCode::ValidationInvalidArgument);
            assert!(err.message.contains("requires --port"));
        });
    }
}
