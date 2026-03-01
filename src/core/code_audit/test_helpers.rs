//! Shared test helpers for code_audit tests.
//!
//! Provides factory functions for building domain types with sensible defaults,
//! reducing boilerplate across test modules.

use super::checks::CheckStatus;
use super::ConventionReport;

/// Build a `ConventionReport` with sensible defaults for testing.
///
/// Sets `status` to `Clean`, `total_files` to 3, `confidence` to 1.0,
/// and leaves `conforming`, `outliers`, and optional fields empty.
pub fn make_convention(
    name: &str,
    glob: &str,
    methods: &[&str],
    registrations: &[&str],
) -> ConventionReport {
    ConventionReport {
        name: name.to_string(),
        glob: glob.to_string(),
        status: CheckStatus::Clean,
        expected_methods: methods.iter().map(|s| s.to_string()).collect(),
        expected_registrations: registrations.iter().map(|s| s.to_string()).collect(),
        expected_interfaces: vec![],
        expected_namespace: None,
        expected_imports: vec![],
        conforming: vec![],
        outliers: vec![],
        total_files: 3,
        confidence: 1.0,
    }
}
