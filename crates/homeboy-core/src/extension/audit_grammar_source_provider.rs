//! Extension-side implementation of the audit grammar-source provider.
//!
//! The audit engine (`code_audit`) defines `GrammarSourceProvider` and calls it
//! to locate the directory holding the grammar for a file type, without
//! depending on the extension registry. This module implements that trait by
//! finding the extension registered to fingerprint the file extension and
//! returning its on-disk path (which holds `grammar.toml`/`grammar.json`). It is
//! registered at binary startup by the CLI, mirroring the fingerprint-script /
//! extension-manifest / component / fixability / runner-evidence / tunnel
//! provider hooks.

use std::path::PathBuf;

use crate::code_audit::grammar_source_provider::{
    register_grammar_source_provider, GrammarSourceProvider,
};

struct ExtensionGrammarSourceProvider;

impl GrammarSourceProvider for ExtensionGrammarSourceProvider {
    fn grammar_dir(&self, file_extension: &str) -> Option<PathBuf> {
        let matched = super::find_extension_for_file_ext(file_extension, "fingerprint")?;
        matched.extension_path.map(PathBuf::from)
    }
}

/// Register the extension-backed grammar-source provider. Called once at binary
/// startup by the CLI.
pub fn register() {
    register_grammar_source_provider(Box::new(ExtensionGrammarSourceProvider));
}
