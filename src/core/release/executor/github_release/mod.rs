//! GitHub Release helper result builders and probes.
//!
//! Split across focused submodules:
//! - [`run`] — the `github.release` step entry point and lifecycle driver.
//! - [`notes`] — release-body construction, generated-notes probes, footers.
//! - [`results`] — `ReleaseStepResult` builders for each outcome.
//! - [`repair`] — manual-recovery command builders, hints, and logging.
//! - [`gh_cli`] — `gh` CLI probes, environment, and command construction.

mod gh_cli;
mod notes;
mod repair;
mod results;
mod run;

#[cfg(test)]
mod tests;

pub(crate) use run::run_github_release;

// `gh` availability/auth/existence probes are reused by the sibling `tagging`
// step to gate tag-availability preflight, so they are part of the module's
// non-test surface.
pub(crate) use gh_cli::{gh_is_authenticated, gh_is_available, gh_release_exists};

// Re-exports consumed by the in-crate test suites — both this module's own
// `tests/` and the parent `executor` module's tests, which reference these via
// `github_release::<name>`. Gated on `cfg(test)` so they do not widen the
// module's non-test surface.
#[cfg(test)]
pub(crate) use crate::core::release::types::ReleaseStepResult;
#[cfg(test)]
pub(crate) use gh_cli::{github_cli_env, github_release_artifact_paths};
#[cfg(test)]
pub(crate) use notes::{
    fallback_release_notes, github_changelog_url, github_generated_notes_start_tag,
    github_release_notes_start_tag, replace_full_changelog_footer, GitHubReleaseBody,
};
#[cfg(test)]
pub(crate) use repair::{
    gh_auth_failure_message, github_release_repair_commands,
    github_release_repair_commands_with_proxy, GitHubReleaseRepairCommands,
};
#[cfg(test)]
pub(crate) use results::{create_failed_result, not_created_result, upload_failed_result};
