//! Crate-internal shell argument quoting used by lab-runner command builders.
//!
//! This re-exports [`homeboy_engine_primitives::shell::shell_arg`], which uses an
//! *allowlist* policy: a value is emitted verbatim only when every character is
//! in a known-safe set (ASCII alphanumerics and `-_./:=@`), otherwise it is
//! single-quoted. An empty value renders as `''` so it is never silently dropped
//! from a rendered command. It is intentionally distinct from
//! [`homeboy_core::error::posix_quote_arg`], which uses a shell-metacharacter
//! *denylist* and therefore quotes a different set of inputs.
pub(crate) use homeboy_engine_primitives::shell::shell_arg;
