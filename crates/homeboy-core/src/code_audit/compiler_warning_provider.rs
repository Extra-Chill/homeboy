//! Extension-provided compiler warnings for the audit engine, inverted behind a
//! provider.
//!
//! The `compiler_warnings` detector surfaces compiler/checker warnings (dead
//! code, unused imports, unused variables) as audit findings. It gets those
//! warnings by running extension-owned compiler-warning scripts. Audit used to
//! do that by calling `crate::extension::{extensions_for_compiler_warning_contract,
//! run_compiler_warning_contract_script}` directly, coupling `code_audit` to the
//! extension script runner and its manifest types.
//!
//! Instead, audit defines the slim view it needs (a root directory → a list of
//! warnings) plus a provider trait; the extension layer registers an
//! implementation at startup that finds the extensions declaring a
//! compiler-warning script, runs them, and parses their output (same pattern as
//! the fingerprint-script / grammar-source / component / fixability /
//! extension-manifest / runner-evidence provider hooks). When no provider is
//! registered — e.g. audit running standalone — the no-op provider yields no
//! warnings, so the detector produces no findings (exactly as when no extension
//! ships a compiler-warning script).

use std::path::Path;
use std::sync::Mutex;

/// One compiler warning surfaced by an extension's compiler-warning script,
/// reduced to the fields the audit detector maps into a finding.
#[derive(Debug, Clone)]
pub struct AuditCompilerWarning {
    /// Warning code, e.g. `unused_imports`.
    pub code: String,
    /// Human-readable warning message.
    pub message: String,
    /// Component-relative file the warning applies to.
    pub file: String,
    /// Optional remediation suggestion.
    pub suggestion: Option<String>,
}

/// The compiler-warning contract the audit engine depends on. Implemented by the
/// extension layer and registered at startup; audit calls it without depending
/// on the extension script runner.
pub trait CompilerWarningProvider: Send + Sync {
    /// Run the compiler-warning scripts of every extension declaring one for the
    /// component rooted at `root`, returning their warnings. Returns an empty
    /// vec when no extension ships such a script.
    fn compiler_warnings(&self, root: &Path) -> Vec<AuditCompilerWarning>;
}

/// Default provider used when no extension layer is registered: no warnings, so
/// the detector produces no findings (exactly as when no extension ships a
/// compiler-warning script).
struct NoopProvider;

impl CompilerWarningProvider for NoopProvider {
    fn compiler_warnings(&self, _root: &Path) -> Vec<AuditCompilerWarning> {
        Vec::new()
    }
}

static PROVIDER: Mutex<Option<Box<dyn CompilerWarningProvider>>> = Mutex::new(None);

/// Register the compiler-warning provider. Called once at binary startup by the
/// extension layer (via the CLI). Replaces any previously registered provider.
pub fn register_compiler_warning_provider(provider: Box<dyn CompilerWarningProvider>) {
    let mut guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(provider);
}

/// Collect compiler warnings for `root` via the registered provider.
pub(crate) fn compiler_warnings_for_root(root: &Path) -> Vec<AuditCompilerWarning> {
    with_provider(|p| p.compiler_warnings(root))
}

fn with_provider<T>(f: impl FnOnce(&dyn CompilerWarningProvider) -> T) -> T {
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
    fn noop_provider_yields_no_warnings() {
        assert!(NoopProvider.compiler_warnings(Path::new("/tmp")).is_empty());
    }
}
