#![cfg(test)]

mod part_a;
mod part_b;

use super::*;
use crate::{RunnerSession, RunnerSessionRole, RunnerTunnelMode};
use homeboy_core::test_support;

pub(super) fn ssh_bootstrap_plan() -> HomeboyBinaryRefreshPlan {
    HomeboyBinaryRefreshPlan {
        runner_id: "lab-local".to_string(),
        mode: "materialize".to_string(),
        source: Some("source".to_string()),
        git_ref: Some("main".to_string()),
        target_dir: Some("/runner/homeboy".to_string()),
        binary_path: "/verified/homeboy".to_string(),
        script: String::new(),
        reconnect: false,
        followup_commands: Vec::new(),
    }
}

pub(super) fn verified_bootstrap_output(sha: &str) -> String {
    format!("HOMEBOY_REFRESH_SOURCE_SHA={sha}\n{{\"data\":{{\"git_commit\":\"{sha}\",\"git_dirty\":false}}}}")
}
