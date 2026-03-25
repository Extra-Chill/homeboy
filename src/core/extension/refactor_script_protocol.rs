//! refactor_script_protocol — extracted from mod.rs.

use crate::config;
use crate::paths;
use std::path::PathBuf;
use crate::component::Component;
use crate::error::Error;
use crate::error::Result;
use crate::output::MergeOutput;
use std::collections::HashMap;
use serde::Serialize;


/// Run a extension's refactor script with a command.
///
/// The script receives a JSON command on stdin and outputs JSON on stdout.
/// Commands are dispatched by the `command` field. Each command has its own
/// input/output schema.
///
/// Supported commands:
/// - `parse_items`: Parse source file, return all top-level items with boundaries
/// - `resolve_imports`: Given moved items, resolve what imports the destination needs
/// - `adjust_visibility`: Adjust visibility of items crossing module boundaries
/// - `find_related_tests`: Find test functions related to named items
/// - `rewrite_import_path`: Compute the corrected import path for a moved item
pub fn run_refactor_script(
    extension: &ExtensionManifest,
    command: &serde_json::Value,
) -> Option<serde_json::Value> {
    let extension_path = extension.extension_path.as_deref()?;
    let script_rel = extension.refactor_script()?;
    let script_path = std::path::Path::new(extension_path).join(script_rel);

    if !script_path.exists() {
        return None;
    }

    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(script_path.to_string_lossy().as_ref())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(command.to_string().as_bytes());
            }
            child.wait_with_output().ok()
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.is_empty() {
            crate::log_status!("refactor", "Extension script error: {}", stderr.trim());
        }
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&stdout).ok()
}

pub fn extension_path(id: &str) -> PathBuf {
    paths::extension(id).unwrap_or_else(|_| PathBuf::from(id))
}

pub fn available_extension_ids() -> Vec<String> {
    config::list_ids::<ExtensionManifest>().unwrap_or_default()
}
