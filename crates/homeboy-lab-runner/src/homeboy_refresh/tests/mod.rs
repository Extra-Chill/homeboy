#![cfg(test)]

mod part_a;
mod part_b;
mod part_c;

use super::*;

/// The ambient roots, for tests that still isolate by mutating process env.
///
/// Rooted entry points take their root explicitly; a test still running under
/// `with_isolated_home` passes the isolated home this resolves to.
fn ambient_roots() -> homeboy_core::paths::PathRoots {
    homeboy_core::paths::PathRoots::from_environment().expect("ambient path roots")
}

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
    format!("HOMEBOY_REFRESH_SOURCE_SHA={sha}\nHOMEBOY_REFRESH_BINARY_SHA256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\nHOMEBOY_REFRESH_BINARY_PATH=/verified/homeboy\n{{\"data\":{{\"git_commit\":\"{sha}\",\"git_dirty\":false}}}}")
}
