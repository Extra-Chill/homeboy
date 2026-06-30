use base64::Engine;
use clap::Args;
use serde_json::{Map, Value};

pub type CmdResult<T> = homeboy::core::Result<(T, i32)>;

pub(crate) use crate::core::markdown::escape_markdown_table_cell;

/// Parse a `KEY=value` string into a (key, value) tuple.
/// Used by clap `value_parser` attributes on `--setting` and `--input` flags.
pub fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{s}`"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

/// Parse a `KEY=<json>` string into a (key, serde_json::Value) tuple.
///
/// Used by `--setting-json` for object/array/typed-scalar settings that
/// `--setting`'s string-only coercion can't represent. JSON value can be
/// any well-formed JSON: object, array, string (must be quoted), number,
/// boolean, or null.
///
/// Examples:
///
///   --setting-json bench_env={"BENCH_CORPUS_SIZE":"1000"}
///   --setting-json wp_config_defines={"MARKDOWN_DB_MODE":"primary","WP_DEBUG":true}
///   --setting-json my_array=[1,2,3]
///   --setting-json my_flag=true
///   --setting-json my_string="literal"
pub fn parse_key_json(s: &str) -> Result<(String, serde_json::Value), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=<json>: no `=` found in `{s}`"))?;
    let key = s[..pos].to_string();
    let raw = &s[pos + 1..];
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|e| {
        format!(
            "invalid JSON for setting `{key}`: {e}. Got `{raw}`. \
             Strings must be quoted (`my_str=\"hello\"`); use --setting for unquoted strings."
        )
    })?;
    Ok((key, value))
}

/// Parse the `--runs` repetition count, surfacing a helpful hint when the
/// operator passes a non-integer.
///
/// `--runs` is a numeric repetition count, but operators frequently confuse
/// it with wanting a stable proof label (e.g. `--runs proof-2026-06`). Clap's
/// default `u64` parser would emit a raw `invalid digit found in string`
/// error that gives no guidance. This parser points them at `--run-id`
/// instead.
pub fn parse_runs_count(s: &str) -> Result<u64, String> {
    s.parse::<u64>().map_err(|_| {
        format!(
            "`{s}` is not a valid number. --runs is a numeric repetition count \
             (how many independent substrate spawns to run); use --run-id for a \
             custom proof label."
        )
    })
}

pub struct GlobalArgs {}

/// Shared arguments for dynamic set commands.
#[derive(Args, Default, Debug)]
pub struct DynamicSetArgs {
    /// Entity ID (optional if provided in JSON body)
    pub id: Option<String>,

    /// JSON object to merge into the entity (supports @file and - for stdin)
    #[arg(long, value_name = "JSON")]
    pub json: Option<String>,

    /// Base64-encoded JSON object (bypasses shell escaping issues)
    #[arg(long, value_name = "BASE64")]
    pub base64: Option<String>,

    /// Replace these fields instead of merging arrays
    #[arg(long, value_name = "FIELD")]
    pub replace: Vec<String>,
}

impl DynamicSetArgs {
    /// Get the JSON spec from --base64 or --json.
    /// Priority: --base64 > --json.
    pub fn json_spec(&self) -> Result<Option<String>, homeboy::core::Error> {
        // Base64 takes priority - decode and return
        if let Some(b64) = &self.base64 {
            let decoded_bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| {
                    homeboy::core::Error::validation_invalid_argument(
                        "base64",
                        format!("Invalid base64 encoding: {}", e),
                        None,
                        Some(vec!["Encode with: echo '{...}' | base64".to_string()]),
                    )
                })?;
            let decoded_str = String::from_utf8(decoded_bytes).map_err(|e| {
                homeboy::core::Error::validation_invalid_argument(
                    "base64",
                    format!("Decoded base64 is not valid UTF-8: {}", e),
                    None,
                    None,
                )
            })?;
            return Ok(Some(decoded_str));
        }
        Ok(self.json.clone())
    }
}

// ============================================================================
// JSON Input Parsing (CLI layer)
// ============================================================================

/// Parse the canonical JSON spec for a set-style update.
pub fn merge_json_sources(spec: Option<&str>) -> homeboy::core::Result<Value> {
    let base = if let Some(spec) = spec {
        let raw = homeboy::core::config::read_json_spec_to_string(spec)?;
        serde_json::from_str(&raw).map_err(|e| {
            let hint = if raw.contains('\\') {
                Some(
                    "For patterns with backslashes, use --base64 to bypass shell escaping:\n  \
                     echo '{...}' | base64\n  \
                     homeboy <command> set ID --base64 \"<encoded>\""
                        .to_string(),
                )
            } else {
                None
            };
            homeboy::core::Error::validation_invalid_json(
                e,
                Some("parse JSON spec".to_string()),
                Some(format!(
                    "{}{}",
                    raw.chars().take(200).collect::<String>(),
                    hint.map(|h| format!("\n\nTip: {}", h)).unwrap_or_default()
                )),
            )
        })?
    } else {
        Value::Object(Map::new())
    };

    Ok(base)
}

// ============================================================================
// DynamicSetArgs Processing Helpers
// ============================================================================

/// Merge JSON sources from `DynamicSetArgs` into a single JSON value.
/// Returns `None` if no JSON/base64 input was provided.
pub fn merge_dynamic_args(args: &DynamicSetArgs) -> homeboy::core::Result<Option<Value>> {
    let spec = args.json_spec()?;
    if spec.is_none() {
        return Ok(None);
    }
    Ok(Some(merge_json_sources(spec.as_deref())?))
}

/// Serialize a merged JSON value to a string and compute the full replace
/// fields list (explicit `--replace` flags + auto-detected array fields).
pub fn finalize_set_spec(
    merged: &Value,
    explicit_replace: &[String],
) -> homeboy::core::Result<(String, Vec<String>)> {
    let json_string = homeboy::core::config::to_json_string(merged)?;

    let mut replace_fields = explicit_replace.to_vec();
    for field in homeboy::core::config::collect_array_fields(merged) {
        if !replace_fields.contains(&field) {
            replace_fields.push(field);
        }
    }

    Ok((json_string, replace_fields))
}

pub mod agent_task;
pub(crate) mod agent_task_dispatch;
pub(crate) mod agent_task_summary;
pub mod api;
pub mod audit;
pub mod audit_baseline;
pub mod auth;
pub mod bench;
pub(crate) mod bench_summary;
pub mod build;
pub mod changelog;
pub mod changes;
pub mod ci;
pub mod cleanup;
pub mod component;
pub mod config;
pub mod contract;
pub mod daemon;
pub mod db;
pub mod deploy;
pub mod deps;
pub mod docs;
pub mod extension;
pub mod file;
pub mod fleet;
pub mod fuzz;
pub mod git;
pub mod http;
pub mod issues;
pub mod json_output;
pub mod lint;
pub mod logs;
pub mod observe;
pub mod project;
pub mod raw_output;
pub mod refactor;
pub mod refs;
pub mod release;
pub mod report;
pub mod resources;
pub mod review;
pub mod rig;
pub mod runner;
pub mod runs;
pub(crate) mod runs_proof_summary;
pub(crate) mod runs_summary;
pub mod self_cmd;
pub mod server;
pub mod ssh;
pub mod stack;
pub mod status;
pub mod test;
pub mod trace;
pub mod triage;
pub mod tunnel;
pub mod undo;
pub mod upgrade;
pub mod utils;
pub mod version;
pub mod worktree;

// Command-runtime infrastructure: the routing, adapter, output/response,
// manifest, and summary plumbing that turns a parsed `Commands` value into a
// dispatched, serialized result. This is a deliberate architectural boundary —
// distinct from the per-command modules above, which each own one user-facing
// command — so shared dispatch/runtime plumbing has a single semantic home
// rather than being interleaved with command implementations. Items are
// re-exported below at their original `crate::commands::*` paths (matching
// original visibility) so callers import them unchanged.
mod infra;

pub(crate) use infra::adapter;
pub use infra::cli;
pub(crate) use infra::key_artifacts;
pub use infra::manifest;
pub use infra::output_runtime;
pub use infra::response;
pub use infra::route;
pub(crate) use infra::runs_dossier_summary;
pub use infra::runtime;
pub use infra::source_command;
pub(crate) use infra::summary_json;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_dynamic_args_accepts_explicit_json() {
        let args = DynamicSetArgs {
            id: Some("sandbox".to_string()),
            json: Some(r#"{"auth":{"mode":"key_plus_password_controlmaster"}}"#.to_string()),
            ..Default::default()
        };

        let merged = merge_dynamic_args(&args).unwrap().unwrap();

        assert_eq!(
            merged,
            json!({"auth": {"mode": "key_plus_password_controlmaster"}})
        );
    }

    #[test]
    fn merge_dynamic_args_accepts_base64_json() {
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(r#"{"auth":{"mode":"key_plus_password_controlmaster"}}"#);
        let args = DynamicSetArgs {
            id: Some("sandbox".to_string()),
            base64: Some(encoded),
            ..Default::default()
        };

        let merged = merge_dynamic_args(&args).unwrap().unwrap();

        assert_eq!(
            merged,
            json!({"auth": {"mode": "key_plus_password_controlmaster"}})
        );
    }
}
