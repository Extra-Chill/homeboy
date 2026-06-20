//! Controller→runner path remapping for Lab offload arguments.
//!
//! Lab offload syncs controller-local directories to the runner and records
//! local→remote pairs. These helpers rewrite absolute controller paths embedded
//! in CLI arguments and JSON payloads to their synced remote equivalents.

use serde_json::Value;
use std::iter::Peekable;
use std::path::Path;
use std::slice::Iter;

/// A local -> remote path pair produced by Lab workspace sync, used to remap
/// controller-side absolute paths embedded in a `--provider-config` payload to
/// the synced locations on the runner.
#[derive(Debug, Clone)]
pub(in crate::core::runner) struct LabPathRemap {
    pub local: String,
    pub remote: String,
}

/// Order remap pairs most-specific-first so a longer local prefix wins over a
/// shorter one it is nested under (and, for equal-length locals, the longer
/// remote wins). Every Lab arg rewriter must remap against this ordering so the
/// same controller path always resolves to the same remote path regardless of
/// which flag carried it.
pub(super) fn order_mappings_by_specificity(mappings: &[LabPathRemap]) -> Vec<&LabPathRemap> {
    let mut ordered: Vec<&LabPathRemap> = mappings.iter().collect();
    ordered.sort_by_key(|mapping| {
        (
            std::cmp::Reverse(mapping.local.len()),
            std::cmp::Reverse(mapping.remote.len()),
        )
    });
    ordered
}

/// Walk `args` honoring the `--` passthrough boundary (everything after `--` is
/// copied verbatim) and delegate each pre-passthrough argument to `rewrite`.
///
/// `rewrite` receives the current argument, a peekable iterator over the
/// remaining arguments (so it can consume the value of a two-token `--flag
/// value` pair), and the output buffer it must push its result onto. This is the
/// single shared scaffold behind every Lab flag-value rewriter; only the
/// per-flag matching differs between callers.
pub(super) fn rewrite_flag_value_args<F>(args: &[String], mut rewrite: F) -> Vec<String>
where
    F: FnMut(&str, &mut Peekable<Iter<'_, String>>, &mut Vec<String>),
{
    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    while let Some(arg) = iter.next() {
        if passthrough {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            out.push(arg.clone());
            continue;
        }
        rewrite(arg, &mut iter, &mut out);
    }
    out
}

/// Fallible variant of [`rewrite_flag_value_args`] for rewriters whose per-flag
/// handler can fail (e.g. when materializing an `@file` spec). Short-circuits on
/// the first error.
pub(super) fn try_rewrite_flag_value_args<F>(
    args: &[String],
    mut rewrite: F,
) -> crate::core::Result<Vec<String>>
where
    F: FnMut(&str, &mut Peekable<Iter<'_, String>>, &mut Vec<String>) -> crate::core::Result<()>,
{
    let mut out = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    while let Some(arg) = iter.next() {
        if passthrough {
            out.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            out.push(arg.clone());
            continue;
        }
        rewrite(arg, &mut iter, &mut out)?;
    }
    Ok(out)
}

pub(in crate::core::runner) fn remap_path_settings_in_args(
    args: &[String],
    mappings: &[LabPathRemap],
) -> Vec<String> {
    if mappings.is_empty() {
        return args.to_vec();
    }

    let ordered = order_mappings_by_specificity(mappings);

    rewrite_flag_value_args(args, |arg, iter, out| {
        if arg == "--setting" {
            out.push(arg.to_string());
            if let Some(raw) = iter.next() {
                out.push(remap_path_setting_pair(raw, &ordered));
            }
            return;
        }
        if arg == "--setting-json" {
            out.push(arg.to_string());
            if let Some(raw) = iter.next() {
                out.push(remap_path_json_setting_pair(raw, &ordered));
            }
            return;
        }
        if let Some(raw) = arg.strip_prefix("--setting=") {
            out.push(format!(
                "--setting={}",
                remap_path_setting_pair(raw, &ordered)
            ));
            return;
        }
        if let Some(raw) = arg.strip_prefix("--setting-json=") {
            out.push(format!(
                "--setting-json={}",
                remap_path_json_setting_pair(raw, &ordered)
            ));
            return;
        }
        out.push(arg.to_string());
    })
}

pub(super) fn remap_path_setting_pair(raw: &str, mappings: &[&LabPathRemap]) -> String {
    let Some((key, value)) = raw.split_once('=') else {
        return raw.to_string();
    };
    remap_local_path(value, mappings)
        .map(|remapped| format!("{key}={remapped}"))
        .unwrap_or_else(|| raw.to_string())
}

pub(super) fn remap_path_json_setting_pair(raw: &str, mappings: &[&LabPathRemap]) -> String {
    let Some((key, value)) = raw.split_once('=') else {
        return raw.to_string();
    };
    let mut value: Value = match serde_json::from_str(value) {
        Ok(value) => value,
        Err(_) => return remap_path_setting_pair(raw, mappings),
    };
    remap_paths_in_value(&mut value, mappings);
    serde_json::to_string(&value)
        .map(|value| format!("{key}={value}"))
        .unwrap_or_else(|_| raw.to_string())
}

pub(super) fn remap_paths_in_value(value: &mut Value, mappings: &[&LabPathRemap]) {
    match value {
        Value::String(text) => {
            if let Some(remapped) = remap_local_path(text, mappings) {
                *text = remapped;
            }
        }
        Value::Array(items) => {
            for item in items {
                remap_paths_in_value(item, mappings);
            }
        }
        Value::Object(map) => {
            for (_, item) in map.iter_mut() {
                remap_paths_in_value(item, mappings);
            }
        }
        _ => {}
    }
}

/// Replace a leading known local path with its remote equivalent. Matches whole
/// path or path-prefix boundaries (so `/a/b` does not match `/a/bc`).
pub(super) fn remap_local_path(text: &str, mappings: &[&LabPathRemap]) -> Option<String> {
    if let Some(remapped) = remap_existing_canonical_path(text, mappings) {
        return Some(remapped);
    }

    for mapping in mappings {
        if mapping.local.is_empty() {
            continue;
        }
        if text == mapping.local {
            return Some(mapping.remote.clone());
        }
        let prefix = format!("{}/", mapping.local.trim_end_matches('/'));
        if let Some(rest) = text.strip_prefix(&prefix) {
            return Some(format!("{}/{}", mapping.remote.trim_end_matches('/'), rest));
        }
    }
    None
}

fn remap_existing_canonical_path(text: &str, mappings: &[&LabPathRemap]) -> Option<String> {
    if !is_controller_path_like(text) {
        return None;
    }
    let expanded = shellexpand::tilde(text).to_string();
    let canonical = Path::new(&expanded).canonicalize().ok()?;
    let canonical = canonical.to_string_lossy().to_string();
    for mapping in mappings {
        if canonical == mapping.local {
            return Some(mapping.remote.clone());
        }
        let prefix = format!("{}/", mapping.local.trim_end_matches('/'));
        if let Some(rest) = canonical.strip_prefix(&prefix) {
            return Some(format!("{}/{}", mapping.remote.trim_end_matches('/'), rest));
        }
    }
    None
}

fn is_controller_path_like(value: &str) -> bool {
    value.starts_with('/') || value.starts_with("~/")
}
