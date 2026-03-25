//! helpers — extracted from mod.rs.

use crate::component::Component;
use crate::error::Error;
use crate::error::Result;
use crate::output::MergeOutput;
use std::collections::HashMap;
use std::path::PathBuf;
use serde::Serialize;
use crate::core::extension::FingerprintOutput;
use crate::core::extension::extension_path;
use crate::core::extension::ExtensionCapability;


pub(crate) fn manifest_has_capability(manifest: &ExtensionManifest, capability: ExtensionCapability) -> bool {
    match capability {
        ExtensionCapability::Lint => manifest.has_lint(),
        ExtensionCapability::Test => manifest.has_test(),
        ExtensionCapability::Build => manifest.has_build(),
    }
}

/// Run a extension's fingerprint script on file content.
///
/// The script receives a JSON object on stdin:
/// ```json
/// {"file_path": "src/core/foo.rs", "content": "...file content..."}
/// ```
///
/// The script must output a JSON object on stdout matching the FileFingerprint schema:
/// ```json
/// {
///   "methods": ["foo", "bar"],
///   "type_name": "MyStruct",
///   "implements": ["SomeTrait"],
///   "registrations": [],
///   "namespace": null,
///   "imports": ["crate::error::Result"]
/// }
/// ```
pub fn run_fingerprint_script(
    extension: &ExtensionManifest,
    file_path: &str,
    content: &str,
) -> Option<FingerprintOutput> {
    let extension_path = extension.extension_path.as_deref()?;
    let script_rel = extension.fingerprint_script()?;
    let script_path = std::path::Path::new(extension_path).join(script_rel);

    if !script_path.exists() {
        return None;
    }

    let input = serde_json::json!({
        "file_path": file_path,
        "content": content,
    });

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(script_path.to_string_lossy().as_ref())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(input.to_string().as_bytes());
            }
            child.wait_with_output().ok()
        })?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).ok()
}
