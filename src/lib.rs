/// Macro for prefixed status logging to stderr (only when stderr is a terminal).
///
/// Usage:
/// ```ignore
/// log_status!("deploy", "Uploading {} to {}", artifact, server);
/// log_status!("release", "Version bumped to {}", version);
/// ```
#[macro_export]
macro_rules! log_status {
    ($prefix:expr, $($arg:tt)*) => {
        if ::std::io::IsTerminal::is_terminal(&::std::io::stderr()) {
            eprintln!(concat!("[", $prefix, "] {}"), format_args!($($arg)*));
        }
    };
}

extern crate self as homeboy;

pub mod cli_runtime;
pub mod cli_surface;
pub mod command_contract;
#[doc(hidden)]
pub mod commands;
// Core engine now lives in the homeboy-core crate. Re-exported as `core` so the
// existing `crate::core::*` call sites across commands / command_contract / the
// CLI runtime are unchanged.
pub use homeboy_core as core;
pub mod extensions;
pub mod help_topics;

/// Test-only fixtures and hermetic process contexts.
///
/// This is public so integration tests can use the same isolation contract as
/// unit tests. It is hidden from normal API documentation and has no role in
/// production command execution.
///
/// The hermetic test harness now lives in `homeboy-core` (behind its
/// `test-support` feature) so both the core crate's tests and the root binary's
/// tests share one isolation implementation. This module re-exports it so
/// existing `crate::test_support::*` call sites keep working, and registers the
/// root-only command-layer cache reset hook the first time it is touched. (#8400)
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    #[doc(inline)]
    pub use homeboy_core::test_support::{
        env_lock, git_fixture_output, run_git_fixture_command, serve_public_artifact_base_once,
        set_command_layer_reset_hook, shared_committed_git_repo_fixture, shared_git_repo_fixture,
        write_source_extension, ArtifactRootOverrideGuard,
    };

    use std::sync::Once;
    use tempfile::TempDir;

    static REGISTER_COMMAND_LAYER_RESET: Once = Once::new();

    /// Register the CLI-layer cache reset (entity-suggestion cache) that the
    /// core harness cannot call directly without depending upward on
    /// `commands`. Invoked from the isolated-home entry point below.
    fn ensure_command_layer_reset_registered() {
        REGISTER_COMMAND_LAYER_RESET.call_once(|| {
            homeboy_core::test_support::set_command_layer_reset_hook(|| {
                crate::commands::utils::entity_suggest::reset_entity_suggestion_cache_for_test();
            });
        });
    }

    /// Root-side wrapper around [`homeboy_core::test_support::with_isolated_home`]
    /// that guarantees the command-layer reset hook is registered first, so the
    /// entity-suggestion cache is cleared during root-binary test isolation.
    pub fn with_isolated_home<R>(body: impl FnOnce(&TempDir) -> R) -> R {
        ensure_command_layer_reset_registered();
        homeboy_core::test_support::with_isolated_home(body)
    }

    /// Root-side wrapper around
    /// [`homeboy_core::test_support::with_isolated_audit_home`] with the same
    /// command-layer reset registration guarantee.
    pub fn with_isolated_audit_home<R>(body: impl FnOnce(&TempDir) -> R) -> R {
        ensure_command_layer_reset_registered();
        homeboy_core::test_support::with_isolated_audit_home(body)
    }
}

/// Helper for `#[serde(skip_serializing_if = "is_zero")]` on `usize` fields.
pub fn is_zero(v: &usize) -> bool {
    *v == 0
}

/// Helper for `#[serde(skip_serializing_if = "is_zero_u32")]` on `u32` fields.
pub fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}
