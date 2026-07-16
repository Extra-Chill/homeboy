use serde::Serialize;

use crate::build_identity;

use super::session::{RunnerSession, RunnerStatusReport};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RunnerBinarySource {
    pub role: &'static str,
    pub owner: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_identity: Option<String>,
    pub purpose: &'static str,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RuntimeMaterializationStatus {
    pub runner_id: String,
    pub configured_executable: String,
    pub controller_version: String,
    pub controller_build_identity: String,
    pub controller_cli: RunnerBinarySource,
    pub active_daemon: RunnerBinarySource,
    pub configured_job_binary: RunnerBinarySource,
    pub binary_sources: Vec<RunnerBinarySource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_daemon_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_daemon_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_command_binary_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_command_binary_build_identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_daemon_severity: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_daemon_refresh_command: Option<String>,
    pub version_drift: bool,
}

impl RuntimeMaterializationStatus {
    pub fn for_homeboy_runner(
        runner_id: &str,
        configured_executable: &str,
        status: &RunnerStatusReport,
    ) -> Self {
        let controller = build_identity::current();
        let active_daemon_version = status
            .session
            .as_ref()
            .map(|session| session.homeboy_version.clone());
        let stale_daemon = status.stale_daemon.as_ref();
        let controller_version = controller.version.clone();
        let controller_cli = Self::controller_cli_source(controller.version, controller.display);
        let active_daemon =
            Self::active_daemon_source(&status.session, &active_daemon_version, stale_daemon);
        let configured_job_binary = Self::configured_job_binary_source(configured_executable);
        let binary_sources = vec![
            controller_cli.clone(),
            active_daemon.clone(),
            configured_job_binary.clone(),
        ];
        let version_drift = active_daemon_version
            .as_ref()
            .is_some_and(|version| version != &controller_version);

        Self {
            runner_id: runner_id.to_string(),
            configured_executable: configured_executable.to_string(),
            controller_version,
            controller_build_identity: controller_cli
                .build_identity
                .clone()
                .unwrap_or_else(|| controller_cli.version.clone().unwrap_or_default()),
            controller_cli,
            active_daemon,
            configured_job_binary,
            binary_sources,
            active_daemon_version,
            active_daemon_build_identity: status
                .session
                .as_ref()
                .and_then(|session| session.homeboy_build_identity.clone()),
            job_command_binary_version: stale_daemon
                .map(|warning| warning.job_command_binary_version.clone()),
            job_command_binary_build_identity: stale_daemon
                .and_then(|warning| warning.job_command_binary_build_identity.clone()),
            stale_daemon_severity: stale_daemon.map(|warning| warning.severity),
            stale_daemon_refresh_command: stale_daemon
                .map(|warning| warning.refresh_command.clone()),
            version_drift,
        }
    }

    pub fn stale_daemon_hint(&self) -> Option<String> {
        let active_daemon = self.active_daemon_display()?;
        let job_binary = self.job_command_binary_display()?;
        let severity = self.stale_daemon_severity?;
        let refresh = self.stale_daemon_refresh_command.as_deref()?;
        Some(format!(
            "Runner `{}` stale daemon severity={severity}: active daemon control plane is `{active_daemon}`, but the job command binary is `{job_binary}`. Refresh with `{refresh}` before using runner/Lab status as version evidence.",
            self.runner_id
        ))
    }

    pub fn has_drift(&self) -> bool {
        self.version_drift || self.stale_daemon_severity.is_some()
    }

    fn active_daemon_display(&self) -> Option<&str> {
        self.active_daemon
            .build_identity
            .as_deref()
            .or(self.active_daemon.version.as_deref())
    }

    fn job_command_binary_display(&self) -> Option<&str> {
        self.job_command_binary_build_identity
            .as_deref()
            .or(self.job_command_binary_version.as_deref())
    }

    fn controller_cli_source(version: String, display: String) -> RunnerBinarySource {
        RunnerBinarySource {
            role: "controller_cli",
            owner: "operator_command",
            path: std::env::current_exe()
                .ok()
                .map(|path| path.display().to_string()),
            version: Some(version),
            build_identity: Some(display),
            purpose: "Renders this status output and submits runner jobs; it does not prove what the runner daemon or job command binary supports.",
        }
    }

    fn active_daemon_source(
        session: &Option<RunnerSession>,
        active_daemon_version: &Option<String>,
        stale_daemon: Option<&super::session::RunnerStaleDaemonWarning>,
    ) -> RunnerBinarySource {
        RunnerBinarySource {
            role: "active_daemon",
            owner: "runner_session",
            path: session
                .as_ref()
                .and_then(|session| session.remote_daemon_address.clone()),
            version: stale_daemon
                .map(|warning| warning.active_daemon_control_plane_version.clone())
                .or_else(|| active_daemon_version.clone()),
            build_identity: stale_daemon
                .and_then(|warning| warning.active_daemon_control_plane_build_identity.clone())
                .or_else(|| {
                    session
                        .as_ref()
                        .and_then(|session| session.homeboy_build_identity.clone())
                }),
            purpose: "Accepts connected daemon jobs until the runner is disconnected/reconnected; it can lag behind the configured job binary after refresh-homeboy.",
        }
    }

    fn configured_job_binary_source(configured_executable: &str) -> RunnerBinarySource {
        RunnerBinarySource {
            role: "configured_job_binary",
            owner: "runner_config.settings.homeboy_path",
            path: Some(configured_executable.to_string()),
            version: None,
            build_identity: None,
            purpose: "Binary path selected for runner-side Homeboy subcommands and capability checks; use command_availability_checks to verify required subcommands on the runner.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::RunnerStaleDaemonWarning;
    use crate::runner::{
        RunnerActiveJobState, RunnerSessionRole, RunnerSessionState, RunnerTunnelMode,
    };

    #[test]
    fn current_status_has_binary_sources_without_drift() {
        let report = report(None);

        let status = RuntimeMaterializationStatus::for_homeboy_runner(
            "homeboy-lab",
            "/opt/homeboy/bin/homeboy",
            &report,
        );

        assert_eq!(status.configured_job_binary.role, "configured_job_binary");
        assert_eq!(status.binary_sources.len(), 3);
        assert_eq!(
            status.configured_job_binary.path.as_deref(),
            Some("/opt/homeboy/bin/homeboy")
        );
        assert_eq!(
            status.active_daemon_version.as_deref(),
            Some(homeboy_product_identity::product_version())
        );
        assert_eq!(status.stale_daemon_hint(), None);
    }

    #[test]
    fn stale_status_renders_control_plane_and_job_binary_hint() {
        let report = report(Some(RunnerStaleDaemonWarning::new(
            "homeboy-lab",
            "homeboy 0.259.0".to_string(),
            "homeboy 0.262.0".to_string(),
            Some("homeboy 0.259.0+daemon".to_string()),
            Some("homeboy 0.262.0+binary".to_string()),
        )));

        let status = RuntimeMaterializationStatus::for_homeboy_runner(
            "homeboy-lab",
            "/opt/homeboy/bin/homeboy",
            &report,
        );

        let hint = status.stale_daemon_hint().expect("stale daemon hint");
        assert!(status.has_drift());
        assert!(hint.contains("active daemon control plane"));
        assert!(hint.contains("homeboy 0.259.0+daemon"));
        assert!(hint.contains("job command binary"));
        assert!(hint.contains("homeboy 0.262.0+binary"));
        assert!(hint.contains(
            "homeboy runner disconnect homeboy-lab && homeboy runner connect homeboy-lab"
        ));
    }

    fn report(stale_daemon: Option<RunnerStaleDaemonWarning>) -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: "homeboy-lab".to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: Some(RunnerSession {
                runner_id: "homeboy-lab".to_string(),
                mode: RunnerTunnelMode::DirectSsh,
                role: RunnerSessionRole::Controller,
                server_id: Some("homeboy-lab".to_string()),
                controller_id: None,
                broker_url: None,
                remote_daemon_address: Some("127.0.0.1:7357".to_string()),
                local_port: Some(7357),
                local_url: Some("http://127.0.0.1:7357".to_string()),
                tunnel_pid: Some(123),
                remote_daemon_pid: Some(456),
                remote_daemon_lease_id: Some("lease-456".to_string()),
                homeboy_version: homeboy_product_identity::product_version().to_string(),
                homeboy_build_identity: Some("homeboy current-build".to_string()),
                connected_at: "2026-06-19T00:00:00Z".to_string(),
                worker_identity: None,
                worker_pid: None,
                last_seen_at: None,
                leaseless_recovery_evidence: None,
            }),
            stale_daemon,
            daemon_freshness: None,
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::Available,
            active_job_source: None,
            active_job_error: None,
            session_path: "/tmp/session.json".to_string(),
        }
    }
}
