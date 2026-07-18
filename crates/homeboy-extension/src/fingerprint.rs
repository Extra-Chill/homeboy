use super::manifest::ExtensionManifest;
use homeboy_engine_primitives::command::{wait_with_bounded_output, DEFAULT_CAPTURE_LIMIT_BYTES};

// The fingerprint output schema moved to the homeboy-audit-contract crate
// (#8425) — it's audit's data model, not extension behavior. Re-exported here
// so `extension::fingerprint::*` (and extension's own `pub use`) keep resolving;
// this module retains only `run_fingerprint_script`, which produces them.
pub use homeboy_audit_contract::fingerprint::{
    AggregateConstructionSeam, AggregateLiteral, CallSite, DeadCodeMarker, FingerprintOutput,
    HookRef, UnusedParam,
};

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
///   "imports": ["homeboy_core::error::Result"]
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

    // Invoke the script directly so its shebang resolves the interpreter.
    // Wrapping with `sh -c <script>` bypasses `#!/usr/bin/env bash` and runs
    // under POSIX sh — which breaks scripts using bash-only features. See #1276.
    let output = std::process::Command::new(&script_path)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(input.to_string().as_bytes());
            }
            wait_with_bounded_output(child, DEFAULT_CAPTURE_LIMIT_BYTES).ok()
        })?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).ok()
}
