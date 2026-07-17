//! Extension-provided grammar sources for the audit engine, inverted behind a
//! provider.
//!
//! The core grammar engine fingerprints a file by loading a grammar
//! (`grammar.toml`/`grammar.json`) shipped by the extension that handles the
//! file's extension. Audit used to resolve that by calling
//! `homeboy_core::extension::find_extension_for_file_ext` directly and reading the
//! matched manifest's `extension_path`, coupling `code_audit` to the extension
//! registry.
//!
//! Instead, audit defines the slim view it needs (file extension → the directory
//! that holds the grammar) plus a provider trait; the extension layer registers
//! an implementation at startup (same pattern as the fingerprint-script /
//! extension-manifest / component / fixability / runner-evidence / tunnel
//! provider hooks). When no provider is registered — e.g. audit running
//! standalone — the no-op provider yields no directory, so audit simply loads no
//! extension grammar (exactly as it does when no such extension is installed)
//! and falls back to its built-in grammars / extension fingerprint scripts.

use std::path::PathBuf;
use std::sync::Mutex;

/// The grammar-source contract the audit engine depends on. Implemented by the
/// extension layer and registered at startup; audit calls it without depending
/// on the extension registry.
pub trait GrammarSourceProvider: Send + Sync {
    /// Return the directory holding the grammar for files with `file_extension`
    /// (without leading dot), i.e. the extension path of the extension
    /// registered to fingerprint that file type. Returns `None` when no
    /// extension handles the file extension or it declares no path on disk.
    fn grammar_dir(&self, file_extension: &str) -> Option<PathBuf>;
}

/// Default provider used when no extension layer is registered: no directory, so
/// audit loads no extension grammar (exactly as when no such extension is
/// installed).
struct NoopProvider;

impl GrammarSourceProvider for NoopProvider {
    fn grammar_dir(&self, _file_extension: &str) -> Option<PathBuf> {
        None
    }
}

static PROVIDER: Mutex<Option<Box<dyn GrammarSourceProvider>>> = Mutex::new(None);

/// Register the grammar-source provider. Called once at binary startup by the
/// extension layer (via the CLI). Replaces any previously registered provider.
pub fn register_grammar_source_provider(provider: Box<dyn GrammarSourceProvider>) {
    let mut guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(provider);
}

/// Resolve the grammar directory for `file_extension` via the registered
/// provider.
pub(crate) fn grammar_dir_for_ext(file_extension: &str) -> Option<PathBuf> {
    with_provider(|p| p.grammar_dir(file_extension))
}

fn with_provider<T>(f: impl FnOnce(&dyn GrammarSourceProvider) -> T) -> T {
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
    fn noop_provider_yields_no_directory() {
        assert!(NoopProvider.grammar_dir("rs").is_none());
    }
}
