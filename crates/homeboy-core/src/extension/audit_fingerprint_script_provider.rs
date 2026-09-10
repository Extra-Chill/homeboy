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
use homeboy_extension_contract::api::v1::{
    ExtensionApiInvokeRequest, EXTENSION_API_INVOKE_REQUEST_SCHEMA, EXTENSION_API_V1,
    FINGERPRINT_FILE_CAPABILITY_PREFIX,
};

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
        let response = crate::extension::invoke::invoke_api(&ExtensionApiInvokeRequest {
            schema: EXTENSION_API_INVOKE_REQUEST_SCHEMA.to_string(),
            api_version: EXTENSION_API_V1,
            extension_id,
            capability_id,
            working_directory: std::env::current_dir().ok()?.to_string_lossy().into_owned(),
            input: serde_json::json!({
                "file_path": relative_path,
                "content": content,
            }),
        });
        serde_json::from_value(response.output?).ok()
    }
}

/// Register the extension-backed fingerprint-script provider. Called once at
/// binary startup by the CLI.
pub fn register() {
    register_fingerprint_script_provider(Box::new(ExtensionFingerprintScriptProvider));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_script(path: &std::path::Path, content: &str) {
        fs::write(path, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).unwrap();
        }
    }

    #[test]
    fn fingerprint_capability_uses_v1_and_preserves_optional_fallbacks() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extension_dir = home.path().join(".config/homeboy/extensions/sample");
            fs::create_dir_all(extension_dir.join("scripts")).unwrap();
            fs::write(
                extension_dir.join("sample.json"),
                r#"{"name":"Sample","version":"1.0.0","scripts":{"fingerprint":"scripts/fingerprint.sh"},"provides":{"file_extensions":["sample"]}}"#,
            )
            .unwrap();
            let script = extension_dir.join("scripts/fingerprint.sh");
            let provider = ExtensionFingerprintScriptProvider;

            write_script(
                &script,
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{\"methods\":[\"decide\"]}'\n",
            );
            let output = provider
                .fingerprint("sample", "policy.sample", "fn decide() {}")
                .expect("fingerprint output");
            assert_eq!(output.methods, ["decide"]);

            write_script(&script, "#!/bin/sh\nprintf 'not json'\n");
            assert!(provider
                .fingerprint("sample", "policy.sample", "invalid")
                .is_none());

            write_script(
                &script,
                "#!/bin/sh\nprintf 'fingerprint failed' >&2\nexit 7\n",
            );
            assert!(provider
                .fingerprint("sample", "policy.sample", "failed")
                .is_none());
        });
    }
}
