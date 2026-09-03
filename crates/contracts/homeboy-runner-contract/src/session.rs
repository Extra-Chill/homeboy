//! Transport-neutral persisted runner session contracts.

use serde::{Deserialize, Serialize};

use crate::RunnerLifecycleOwner;

/// How a runner session is tunneled.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerTunnelMode {
    DirectSsh,
    Reverse,
}

impl RunnerTunnelMode {
    /// This mode rendered for humans: `direct SSH`, `reverse-connected`.
    ///
    /// This deliberately differs from the persisted metadata vocabulary.
    pub fn label(&self) -> &'static str {
        self.labels().0
    }

    /// This mode as its canonical wire string. Tests pin this hand-written
    /// restatement to the derived serde representation.
    pub fn metadata_value(&self) -> &'static str {
        self.labels().1
    }

    fn labels(&self) -> (&'static str, &'static str) {
        match self {
            RunnerTunnelMode::DirectSsh => ("direct SSH", "direct_ssh"),
            RunnerTunnelMode::Reverse => ("reverse-connected", "reverse"),
        }
    }
}

fn default_tunnel_mode() -> RunnerTunnelMode {
    RunnerTunnelMode::DirectSsh
}

/// Which side owns a runner session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerSessionRole {
    Controller,
    Runner,
}

fn default_session_role() -> RunnerSessionRole {
    RunnerSessionRole::Controller
}

/// The connectivity state of a runner session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerSessionState {
    Connected,
    Disconnected,
    Recorded,
}

/// Kernel-derived identity for one local tunnel process instance. This survives
/// controller restart so a recycled PID is never signaled from durable state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "platform", rename_all = "snake_case")]
pub enum RunnerTunnelProcessStartIdentity {
    Linux {
        starttime_ticks: u64,
    },
    Macos {
        start_seconds: u64,
        start_microseconds: u64,
    },
}

/// A controller-owned reverse forward that exposes a controller-local proxy to
/// a direct SSH runner. The URL points at the runner loopback listener and
/// carries no controller credentials.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerProxyForward {
    pub runner_url: String,
    pub tunnel_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel_process_start_identity: Option<RunnerTunnelProcessStartIdentity>,
}

/// A persisted runner session record. `leaseless_recovery_evidence` remains
/// opaque JSON because the runner implementation owns its typed representation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerSession {
    pub runner_id: String,
    #[serde(default = "default_tunnel_mode")]
    pub mode: RunnerTunnelMode,
    #[serde(default = "default_session_role")]
    pub role: RunnerSessionRole,
    pub server_id: Option<String>,
    #[serde(default)]
    pub controller_id: Option<String>,
    #[serde(default)]
    pub broker_url: Option<String>,
    #[serde(default)]
    pub remote_daemon_address: Option<String>,
    #[serde(default)]
    pub local_port: Option<u16>,
    #[serde(default)]
    pub local_url: Option<String>,
    pub tunnel_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tunnel_process_start_identity: Option<RunnerTunnelProcessStartIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_forward: Option<RunnerProxyForward>,
    pub remote_daemon_pid: Option<u32>,
    #[serde(default)]
    pub remote_daemon_lease_id: Option<String>,
    pub homeboy_version: String,
    #[serde(default)]
    pub homeboy_build_identity: Option<String>,
    pub connected_at: String,
    #[serde(default)]
    pub worker_identity: Option<String>,
    #[serde(default)]
    pub worker_pid: Option<u32>,
    #[serde(default)]
    pub last_seen_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaseless_recovery_evidence: Option<serde_json::Value>,
}

impl RunnerSession {
    /// Which side owns this session's lifecycle, derived from its role.
    pub fn lifecycle_owner(&self) -> RunnerLifecycleOwner {
        match self.role {
            RunnerSessionRole::Controller => RunnerLifecycleOwner::Controller,
            RunnerSessionRole::Runner => RunnerLifecycleOwner::Runner,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_session() -> RunnerSession {
        serde_json::from_value(serde_json::json!({
            "runner_id": "runner-1",
            "server_id": null,
            "tunnel_pid": null,
            "remote_daemon_pid": null,
            "homeboy_version": "1.0.0",
            "connected_at": "2026-08-30T00:00:00Z"
        }))
        .expect("deserialize")
    }

    #[test]
    fn session_defaults_and_optional_fields_keep_their_wire_shape() {
        let session = minimal_session();
        assert_eq!(session.mode, RunnerTunnelMode::DirectSsh);
        assert_eq!(session.role, RunnerSessionRole::Controller);
        assert_eq!(session.lifecycle_owner(), RunnerLifecycleOwner::Controller);
        assert_eq!(
            serde_json::to_value(session).expect("serialize"),
            serde_json::json!({
                "runner_id": "runner-1",
                "mode": "direct_ssh",
                "role": "controller",
                "server_id": null,
                "controller_id": null,
                "broker_url": null,
                "remote_daemon_address": null,
                "local_port": null,
                "local_url": null,
                "tunnel_pid": null,
                "remote_daemon_pid": null,
                "remote_daemon_lease_id": null,
                "homeboy_version": "1.0.0",
                "homeboy_build_identity": null,
                "connected_at": "2026-08-30T00:00:00Z",
                "worker_identity": null,
                "worker_pid": null,
                "last_seen_at": null
            })
        );
    }

    #[test]
    fn complete_session_keeps_nested_identity_and_recovery_data() {
        let value = serde_json::json!({
            "runner_id": "runner-1",
            "mode": "reverse",
            "role": "runner",
            "server_id": "server-1",
            "controller_id": "controller-1",
            "broker_url": "https://broker.example.test",
            "remote_daemon_address": "127.0.0.1:9000",
            "local_port": 9001,
            "local_url": "http://127.0.0.1:9001",
            "tunnel_pid": 101,
            "tunnel_process_start_identity": {
                "platform": "linux",
                "starttime_ticks": 200
            },
            "proxy_forward": {
                "runner_url": "http://127.0.0.1:9002",
                "tunnel_pid": 102,
                "tunnel_process_start_identity": {
                    "platform": "macos",
                    "start_seconds": 300,
                    "start_microseconds": 400
                }
            },
            "remote_daemon_pid": 103,
            "remote_daemon_lease_id": "lease-1",
            "homeboy_version": "1.0.0",
            "homeboy_build_identity": "build-1",
            "connected_at": "2026-08-30T00:00:00Z",
            "worker_identity": "worker-1",
            "worker_pid": 104,
            "last_seen_at": "2026-08-30T00:01:00Z",
            "leaseless_recovery_evidence": { "reason": "state-loss" }
        });

        let session = serde_json::from_value::<RunnerSession>(value.clone()).expect("deserialize");
        assert_eq!(session.lifecycle_owner(), RunnerLifecycleOwner::Runner);
        assert_eq!(serde_json::to_value(session).expect("serialize"), value);
    }

    #[test]
    fn session_enum_vocabulary_stays_canonical() {
        for (mode, wire, label) in [
            (RunnerTunnelMode::DirectSsh, "direct_ssh", "direct SSH"),
            (RunnerTunnelMode::Reverse, "reverse", "reverse-connected"),
        ] {
            assert_eq!(serde_json::to_value(&mode).expect("serialize"), wire);
            assert_eq!(mode.metadata_value(), wire);
            assert_eq!(mode.label(), label);
        }

        for (state, wire) in [
            (RunnerSessionState::Connected, "connected"),
            (RunnerSessionState::Disconnected, "disconnected"),
            (RunnerSessionState::Recorded, "recorded"),
        ] {
            assert_eq!(serde_json::to_value(state).expect("serialize"), wire);
        }
    }
}
