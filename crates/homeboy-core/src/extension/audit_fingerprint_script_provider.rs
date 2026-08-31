//! Extension-side implementation of the audit fingerprint-script provider.
//!
//! The audit engine (`code_audit`) defines `FingerprintScriptProvider` and calls
//! it to fingerprint files that the core grammar engine cannot handle, without
//! depending on the extension script runner. This module implements that trait
//! by finding the extension registered for the file extension and running its
//! fingerprint script. It is registered at binary startup by the CLI, mirroring
//! the extension-manifest / component / fixability / runner-evidence / tunnel
//! provider hooks.

use homeboy_audit_contract::FingerprintOutput;
use homeboy_extension_contract::api::v1::FINGERPRINT_FILE_CAPABILITY_PREFIX;

use homeboy_core::code_audit::fingerprint_script_provider::{
    register_fingerprint_script_provider, FingerprintScriptProvider,
};

struct ExtensionFingerprintScriptProvider;

impl FingerprintScriptProvider for ExtensionFingerprintScriptProvider {
    fn fingerprint(
        &self,
        file_extension: &str,
        relative_path: &str,
        content: &str,
    ) -> Option<FingerprintOutput> {
        let capability_id = format!("{FINGERPRINT_FILE_CAPABILITY_PREFIX}{file_extension}");
        let extension_id =
            crate::extension::resolve::find_installed_capability_provider(&capability_id)?;
        let matched = crate::extension::catalog::load_extension(&extension_id).ok()?;
        super::fingerprint::run_fingerprint_script(&matched, relative_path, content)
    }
}

/// Register the extension-backed fingerprint-script provider. Called once at
/// binary startup by the CLI.
pub fn register() {
    register_fingerprint_script_provider(Box::new(ExtensionFingerprintScriptProvider));
}
