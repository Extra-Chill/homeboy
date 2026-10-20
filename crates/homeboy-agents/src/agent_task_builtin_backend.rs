//! Cook's built-in coding backend (#15932).
//!
//! Pi is Cook's coding backend. Its routes bill through the Roadie credential
//! pool, so Cook selects it when no policy names a backend and the provider
//! catalog declares it. A backend that no installed provider declares is not
//! aliased to Pi: it fails provider resolution with a remediation that names
//! Pi, instead of dispatching.

/// The built-in coding backend Cook selects when no policy names one and the
/// provider catalog declares it.
pub const BUILTIN_DEFAULT_BACKEND: &str = "pi";

/// Remediation appended to an unresolvable backend so stale config names the
/// replacement route instead of only reporting the missing provider.
pub fn builtin_backend_remediation(backend: &str) -> String {
    format!(
        "Pi is the coding backend: replace `{}` with `{BUILTIN_DEFAULT_BACKEND}` in agent_task.default_backend, agent_task.rotation entries, and --backend (Pi routes bill through the Roadie credential pool, for example `roadie/anthropic-claude-sonnet-5-5`)",
        backend.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remediation_names_the_builtin_backend_and_the_stale_value() {
        let remediation = builtin_backend_remediation(" retired-runtime ");
        assert!(remediation.contains("`retired-runtime`"), "{remediation}");
        assert!(remediation.contains("`pi`"), "{remediation}");
    }
}
