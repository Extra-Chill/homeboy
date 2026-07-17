//! Extension-script fingerprinting for the audit engine, inverted behind a
//! provider.
//!
//! When the core grammar engine cannot fingerprint a file, audit falls back to
//! an extension-provided fingerprint script: it finds the extension that handles
//! the file extension and runs its script, getting back a `FingerprintOutput`.
//! Audit used to do that by calling `homeboy_core::extension::{find_extension_for_file_ext,
//! run_fingerprint_script}` directly, coupling `code_audit` to the extension
//! layer's script runner.
//!
//! Instead, audit defines the slim view it needs (extension → `FingerprintOutput`)
//! plus a provider trait; the extension layer registers an implementation at
//! startup (same pattern as the extension-manifest / component / fixability /
//! runner-evidence / tunnel provider hooks). When no provider is registered —
//! e.g. audit running standalone — the no-op provider yields no output, so audit
//! simply produces no fingerprint for files only an extension script could
//! handle (exactly as it does when no such extension is installed).

use std::sync::Mutex;

use homeboy_audit_contract::FingerprintOutput;

/// The fingerprint-script contract the audit engine depends on. Implemented by
/// the extension layer and registered at startup; audit calls it without
/// depending on the extension script runner.
pub trait FingerprintScriptProvider: Send + Sync {
    /// Fingerprint a file via the extension script registered for `file_extension`
    /// (without leading dot). Returns `None` when no extension handles the
    /// extension or the script produced no output.
    fn fingerprint(
        &self,
        file_extension: &str,
        relative_path: &str,
        content: &str,
    ) -> Option<FingerprintOutput>;
}

/// Default provider used when no extension layer is registered: no output, so
/// audit produces no fingerprint for extension-script-only files (exactly as it
/// does when no such extension is installed).
struct NoopProvider;

impl FingerprintScriptProvider for NoopProvider {
    fn fingerprint(
        &self,
        _file_extension: &str,
        _relative_path: &str,
        _content: &str,
    ) -> Option<FingerprintOutput> {
        None
    }
}

static PROVIDER: Mutex<Option<Box<dyn FingerprintScriptProvider>>> = Mutex::new(None);

/// Register the fingerprint-script provider. Called once at binary startup by
/// the extension layer (via the CLI). Replaces any previously registered
/// provider.
pub fn register_fingerprint_script_provider(provider: Box<dyn FingerprintScriptProvider>) {
    let mut guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(provider);
}

/// Fingerprint a file via the registered extension-script provider.
pub(crate) fn fingerprint_via_script(
    file_extension: &str,
    relative_path: &str,
    content: &str,
) -> Option<FingerprintOutput> {
    with_provider(|p| p.fingerprint(file_extension, relative_path, content))
}

fn with_provider<T>(f: impl FnOnce(&dyn FingerprintScriptProvider) -> T) -> T {
    let guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.as_ref() {
        Some(provider) => f(provider.as_ref()),
        None => f(&NoopProvider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_provider_yields_no_output() {
        assert!(NoopProvider
            .fingerprint("rs", "src/lib.rs", "fn x() {}")
            .is_none());
    }
}
