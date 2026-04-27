use base64::Engine;
use clap::Args;
use serde_json::{json, Map, Value};

pub type CmdResult<T> = homeboy::Result<(T, i32)>;

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
/// Flag names become JSON keys with no case conversion.
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
    pub fn json_spec(&self) -> Result<Option<String>, homeboy::Error> {
        // Base64 takes priority - decode and return
        if let Some(b64) = &self.base64 {
            let decoded_bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| {
                    homeboy::Error::validation_invalid_argument(
                        "base64",
                        format!("Invalid base64 encoding: {}", e),
                        None,
                        Some(vec!["Encode with: echo '{...}' | base64".to_string()]),
                    )
                })?;
            let decoded_str = String::from_utf8(decoded_bytes).map_err(|e| {
                homeboy::Error::validation_invalid_argument(
                    "base64",
                    format!("Decoded base64 is not valid UTF-8: {}", e),
                    None,
                    None,
                )
            })?;
            return Ok(Some(decoded_str));
        }
        // If spec looks like a flag (--key), it was misrouted — not a JSON spec
        if let Some(ref s) = self.spec {
            if s.starts_with("--") {
                return Ok(self.json.clone());
            }
        }
        Ok(self.json.clone().or_else(|| self.spec.clone()))
    }

    /// Return the full list of trailing key-value args, including any flag
    /// that was misrouted into the `spec` positional by clap.
    ///
    /// When `--` separates trailing args, clap assigns the first positional
    /// after the ID to `spec`. If that value starts with `--`, it's a flag
    /// key that belongs with `extra`.
    pub fn effective_extra(&self) -> Vec<String> {
        match &self.spec {
            Some(s) if s.starts_with("--") => {
                let mut combined = vec![s.clone()];
                combined.extend(self.extra.iter().cloned());
                combined
            }
            _ => self.extra.clone(),
        }
    }
}

// ============================================================================
// JSON Input Parsing (CLI layer)
// ============================================================================

/// Parse --key value pairs into a JSON object.
fn parse_kv_flags(extra: &[String]) -> homeboy::Result<Value> {
    let mut obj = Map::new();
    let mut iter = extra.iter().peekable();

    while let Some(arg) = iter.next() {
        if let Some(key) = arg.strip_prefix("--") {
            let value = iter.next().ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    key,
                    format!("Missing value for flag --{}", key),
                    None,
                    None,
                )
            })?;
            let parsed = parse_value(value);
            obj.insert(key.to_string(), parsed);
        }
    }

    Ok(Value::Object(obj))
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
pub fn merge_json_sources(spec: Option<&str>, extra: &[String]) -> homeboy::Result<Value> {
    let mut base = if let Some(spec) = spec {
        let raw = homeboy::config::read_json_spec_to_string(spec)?;
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
            homeboy::Error::validation_invalid_json(
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
pub fn merge_dynamic_args(args: &DynamicSetArgs) -> homeboy::Result<Option<Value>> {
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
) -> homeboy::Result<(String, Vec<String>)> {
    let json_string = homeboy::config::to_json_string(merged)?;

    let mut replace_fields = explicit_replace.to_vec();
    for field in homeboy::config::collect_array_fields(merged) {
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
pub mod docs;
pub mod extension;
pub mod file;
pub mod fleet;
pub mod git;
pub mod init;
pub mod issues;
pub mod lint;
pub mod logs;
pub mod project;
pub mod refactor;
pub mod release;
pub mod review;
pub mod rig;
pub mod self_cmd;
pub mod server;
pub mod ssh;
pub mod stack;
pub mod status;
pub mod test;
pub mod transfer;
pub mod triage;
pub mod undo;
pub mod upgrade;
pub mod utils;
pub mod validate;
pub mod version;

pub fn run_markdown(
    command: crate::cli_surface::Commands,
    global: &GlobalArgs,
) -> homeboy::Result<(String, i32)> {
    match command {
        crate::cli_surface::Commands::Docs(args) => docs::run_markdown(args),
        crate::cli_surface::Commands::Changelog(args) => changelog::run_markdown(args),
        crate::cli_surface::Commands::Review(args) => review::run_markdown(args, global),
        _ => Err(homeboy::Error::validation_invalid_argument(
            "output_mode",
            "Command does not support markdown output",
            None,
            None,
        )),
    }
}

/// Dispatch a command to its handler and map result to JSON.
macro_rules! dispatch {
    ($args:expr, $global:expr, $extension:ident) => {
        crate::commands::utils::response::map_cmd_result_to_json($extension::run($args, $global))
    };
}

pub fn run_json(
    command: crate::cli_surface::Commands,
    global: &GlobalArgs,
) -> (homeboy::Result<serde_json::Value>, i32) {
    crate::commands::utils::tty::status("homeboy is working...");

    match command {
        // All commands use global context
        crate::cli_surface::Commands::Init(args) => dispatch!(args, global, init),
        crate::cli_surface::Commands::Status(args) => dispatch!(args, global, status),
        crate::cli_surface::Commands::Test(args) => dispatch!(args, global, test),
        crate::cli_surface::Commands::Bench(args) => dispatch!(args, global, bench),
        crate::cli_surface::Commands::Lint(args) => dispatch!(args, global, lint),
        crate::cli_surface::Commands::Project(args) => dispatch!(args, global, project),
        crate::cli_surface::Commands::Ssh(args) => dispatch!(args, global, ssh),
        crate::cli_surface::Commands::Server(args) => dispatch!(args, global, server),
        crate::cli_surface::Commands::Db(args) => dispatch!(args, global, db),
        crate::cli_surface::Commands::File(args) => dispatch!(args, global, file),
        crate::cli_surface::Commands::Fleet(args) => dispatch!(args, global, fleet),
        crate::cli_surface::Commands::Logs(args) => dispatch!(args, global, logs),
        crate::cli_surface::Commands::Transfer(args) => dispatch!(args, global, transfer),
        crate::cli_surface::Commands::Triage(args) => dispatch!(args, global, triage),
        crate::cli_surface::Commands::Deploy(args) => dispatch!(args, global, deploy),
        crate::cli_surface::Commands::Component(args) => dispatch!(args, global, component),
        crate::cli_surface::Commands::Config(args) => dispatch!(args, global, config),
        crate::cli_surface::Commands::Daemon(args) => dispatch!(args, global, daemon),
        crate::cli_surface::Commands::Extension(args) => dispatch!(args, global, extension),
        crate::cli_surface::Commands::Docs(args) => dispatch!(args, global, docs),
        crate::cli_surface::Commands::Changelog(args) => dispatch!(args, global, changelog),
        crate::cli_surface::Commands::Git(args) => dispatch!(args, global, git),
        crate::cli_surface::Commands::Issues(args) => dispatch!(args, global, issues),
        crate::cli_surface::Commands::Version(args) => dispatch!(args, global, version),
        crate::cli_surface::Commands::Build(args) => dispatch!(args, global, build),
        crate::cli_surface::Commands::Validate(args) => dispatch!(args, global, validate),
        crate::cli_surface::Commands::Changes(args) => dispatch!(args, global, changes),
        crate::cli_surface::Commands::Release(args) => dispatch!(args, global, release),
        crate::cli_surface::Commands::Review(args) => dispatch!(args, global, review),
        crate::cli_surface::Commands::Audit(args) => dispatch!(args, global, audit),
        crate::cli_surface::Commands::Refactor(args) => dispatch!(args, global, refactor),
        crate::cli_surface::Commands::Rig(args) => dispatch!(args, global, rig),
        crate::cli_surface::Commands::SelfCmd(args) => dispatch!(args, global, self_cmd),
        crate::cli_surface::Commands::Stack(args) => dispatch!(args, global, stack),
        crate::cli_surface::Commands::Undo(args) => dispatch!(args, global, undo),
        crate::cli_surface::Commands::Auth(args) => dispatch!(args, global, auth),
        crate::cli_surface::Commands::Api(args) => dispatch!(args, global, api),
        crate::cli_surface::Commands::Upgrade(args)
        | crate::cli_surface::Commands::Update(args) => {
            dispatch!(args, global, upgrade)
        }

        // Special case: List uses raw output mode
        crate::cli_surface::Commands::List => {
            let err = homeboy::Error::validation_invalid_argument(
                "output_mode",
                "List command uses raw output mode",
                None,
                None,
            );
            crate::commands::utils::response::map_cmd_result_to_json::<serde_json::Value>(Err(err))
        }
    }
}
