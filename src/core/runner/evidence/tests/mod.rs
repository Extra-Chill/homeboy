#![cfg(test)]

use crate::core::runner::RunnerKind;
use crate::core::server::{RunnerPolicy, RunnerSettings};

use super::super::Runner;

mod artifact;
mod helpers;
mod mirror;

pub(super) fn ssh_runner() -> Runner {
    Runner {
        id: "lab".to_string(),
        kind: RunnerKind::Ssh,
        server_id: Some("srv".to_string()),
        workspace_root: Some("/srv/homeboy".to_string()),
        settings: RunnerSettings {
            daemon: true,
            ..Default::default()
        },
        env: Default::default(),
        secret_env: Default::default(),
        resources: Default::default(),
        policy: RunnerPolicy::default(),
    }
}
