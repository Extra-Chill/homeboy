use base64::Engine;
use clap::Args;
use serde_json::{json, Map, Value};

pub type CmdResult<T> = homeboy::core::Result<(T, i32)>;

pub(crate) fn escape_markdown_table_cell(value: &str) -> String {
    value.replace('|', "\\|")
}

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

pub struct GlobalArgs {}

/// Shared arguments for dynamic set commands.
///
/// Allows arbitrary `--key value` pairs that map directly to JSON keys.
/// Also accepts positional `key=value` pairs for copy-pasteable remediation
/// commands. Flag names become JSON keys with no case conversion.
///
/// # Combining --json with dynamic flags
///
/// When using both `--json` and dynamic `--key value` flags, you MUST add
/// an explicit `--` separator before the dynamic flags:
///
/// ```sh
/// # Correct: explicit separator before dynamic flags
/// homeboy component set my-component --json '{"type":"plugin"}' -- --extract_command "unzip -o artifact.zip"
///
/// # Incorrect: will fail with "unexpected argument"
/// homeboy component set my-component --json '{"type":"plugin"}' --extract_command "unzip -o artifact.zip"
/// ```
///
/// This is required because without the positional JSON spec, the parser
/// cannot determine where dynamic trailing arguments begin.
#[derive(Args, Default, Debug)]
pub struct DynamicSetArgs {
    /// Entity ID (optional if provided in JSON body)
    pub id: Option<String>,

    /// JSON spec (positional, supports @file and - for stdin)
    pub spec: Option<String>,

    /// Explicit JSON spec (takes precedence over positional)
    #[arg(long, value_name = "JSON")]
    pub json: Option<String>,

    /// Base64-encoded JSON spec (bypasses shell escaping issues)
    #[arg(long, value_name = "BASE64")]
    pub base64: Option<String>,

    /// Replace these fields instead of merging arrays
    #[arg(long, value_name = "FIELD")]
    pub replace: Vec<String>,

    /// Dynamic key=value flags (e.g., --remote_path /var/www).
    /// When combined with --json, add '--' separator first:
    /// `homeboy component set ID --json '{}' -- --key value`
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub extra: Vec<String>,
}

impl DynamicSetArgs {
    /// Get the JSON spec from --base64, --json, or positional argument.
    /// Priority: --base64 > --json > positional spec
    ///
    /// If the positional `spec` looks like a flag (starts with `--`), it was
    /// misrouted by clap after a `--` separator and is not a JSON spec.
    /// Use `effective_extra()` to recover it as a key-value flag.
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
        // If spec looks like a dynamic set argument, it was misrouted — not a JSON spec
        if let Some(ref s) = self.spec {
            if is_dynamic_set_arg(s) {
                return Ok(self.json.clone());
            }
        }
        Ok(self.json.clone().or_else(|| self.spec.clone()))
    }

    /// Return the full list of trailing key-value args, including any flag
    /// that was misrouted into the `spec` positional by clap.
    ///
    /// When `--` separates trailing args, clap assigns the first positional
    /// after the ID to `spec`. If that value is a dynamic set argument, it
    /// belongs with `extra`.
    pub fn effective_extra(&self) -> Vec<String> {
        match &self.spec {
            Some(s) if is_dynamic_set_arg(s) => {
                let mut combined = vec![s.clone()];
                combined.extend(self.extra.iter().cloned());
                combined
            }
            _ => self.extra.clone(),
        }
    }
}

fn is_dynamic_set_arg(arg: &str) -> bool {
    arg.starts_with("--") || parse_key_value_arg(arg).is_some()
}

// ============================================================================
// JSON Input Parsing (CLI layer)
// ============================================================================

/// Parse --key value and key=value pairs into a JSON object.
fn parse_kv_flags(extra: &[String]) -> homeboy::core::Result<Value> {
    let mut obj = Map::new();
    let mut iter = extra.iter().peekable();

    while let Some(arg) = iter.next() {
        if let Some((key, value)) = parse_key_value_arg(arg) {
            insert_path_value(&mut obj, &key, parse_value(&value));
        } else if let Some(key) = arg.strip_prefix("--") {
            let value = iter.next().ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    key,
                    format!("Missing value for flag --{}", key),
                    None,
                    None,
                )
            })?;
            let parsed = parse_value(value);
            insert_path_value(&mut obj, key, parsed);
        }
    }

    Ok(Value::Object(obj))
}

fn parse_key_value_arg(arg: &str) -> Option<(String, String)> {
    let (key, value) = arg.split_once('=')?;
    if key.is_empty() || key.starts_with('-') {
        return None;
    }
    Some((key.to_string(), value.to_string()))
}

fn insert_path_value(obj: &mut Map<String, Value>, key: &str, value: Value) {
    let mut parts = key.split('.').filter(|part| !part.is_empty()).peekable();
    let Some(first) = parts.next() else {
        return;
    };

    if parts.peek().is_none() {
        obj.insert(first.to_string(), value);
        return;
    }

    let mut current = obj
        .entry(first.to_string())
        .or_insert_with(|| Value::Object(Map::new()));

    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            if let Value::Object(map) = current {
                map.insert(part.to_string(), value);
            }
            return;
        }

        if !current.is_object() {
            *current = Value::Object(Map::new());
        }
        let Value::Object(map) = current else {
            return;
        };
        current = map
            .entry(part.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
}

/// Parse a string value into appropriate JSON type.
/// Order: JSON literal → bool → number → string
fn parse_value(s: &str) -> Value {
    // Try JSON first (handles arrays, objects, quoted strings)
    if let Ok(v) = serde_json::from_str(s) {
        return v;
    }
    // Try bool
    if s == "true" {
        return json!(true);
    }
    if s == "false" {
        return json!(false);
    }
    // Try number
    if let Ok(n) = s.parse::<i64>() {
        return json!(n);
    }
    if let Ok(n) = s.parse::<f64>() {
        return json!(n);
    }
    // Default to string
    json!(s)
}

/// Merge JSON spec with --key value flags. Flags override spec values.
pub fn merge_json_sources(spec: Option<&str>, extra: &[String]) -> homeboy::core::Result<Value> {
    let mut base = if let Some(spec) = spec {
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

    if !extra.is_empty() {
        let flags = parse_kv_flags(extra)?;
        if let (Value::Object(base_obj), Value::Object(flags_obj)) = (&mut base, flags) {
            for (k, v) in flags_obj {
                base_obj.insert(k, v);
            }
        }
    }

    Ok(base)
}

// ============================================================================
// DynamicSetArgs Processing Helpers
// ============================================================================

/// Merge JSON sources from `DynamicSetArgs` into a single JSON value.
/// Returns `None` if no JSON/base64/key-value input was provided.
pub fn merge_dynamic_args(args: &DynamicSetArgs) -> homeboy::core::Result<Option<Value>> {
    let spec = args.json_spec()?;
    let extra = args.effective_extra();
    if spec.is_none() && extra.is_empty() {
        return Ok(None);
    }
    Ok(Some(merge_json_sources(spec.as_deref(), &extra)?))
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

pub mod api;
pub mod audit;
pub mod auth;
pub mod bench;
pub mod build;
pub mod changelog;
pub mod changes;
pub mod cli;
pub mod component;
pub mod config;
pub mod daemon;
pub mod db;
pub mod deploy;
pub mod deps;
pub mod docs;
pub mod doctor;
pub mod extension;
pub mod file;
pub mod fleet;
pub mod git;
pub mod http;
pub mod issues;
pub mod json_output;
pub mod lint;
pub mod logs;
pub mod observe;
pub mod output_artifact;
pub mod project;
pub mod raw_output;
pub mod refactor;
pub mod release;
pub mod report;
pub mod review;
pub mod rig;
pub mod runner;
pub mod runs;
pub mod self_cmd;
pub mod server;
pub mod ssh;
pub mod stack;
pub mod status;
pub mod test;
pub mod trace;
pub mod triage;
pub mod undo;
pub mod upgrade;
pub mod utils;
pub mod version;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_dynamic_args_accepts_positional_key_value_pair() {
        let args = DynamicSetArgs {
            id: Some("sandbox".to_string()),
            spec: Some("auth.mode=key_plus_password_controlmaster".to_string()),
            ..Default::default()
        };

        let merged = merge_dynamic_args(&args).unwrap().unwrap();

        assert_eq!(
            merged,
            json!({"auth": {"mode": "key_plus_password_controlmaster"}})
        );
    }

    #[test]
    fn merge_dynamic_args_accepts_dotted_flag_path() {
        let args = DynamicSetArgs {
            id: Some("sandbox".to_string()),
            spec: Some("--auth.mode".to_string()),
            extra: vec!["key_plus_password_controlmaster".to_string()],
            ..Default::default()
        };

        let merged = merge_dynamic_args(&args).unwrap().unwrap();

        assert_eq!(
            merged,
            json!({"auth": {"mode": "key_plus_password_controlmaster"}})
        );
    }
}
