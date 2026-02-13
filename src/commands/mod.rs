use base64::Engine;
use clap::Args;
use serde_json::{json, Map, Value};

pub type CmdResult<T> = homeboy::Result<(T, i32)>;

pub(crate) struct GlobalArgs {}

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
/// homeboy component set my-component --json '{"type":"plugin"}' -- --build_command "npm run build"
///
/// # Incorrect: will fail with "unexpected argument"
/// homeboy component set my-component --json '{"type":"plugin"}' --build_command "npm run build"
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
    pub fn json_spec(&self) -> Result<Option<String>, homeboy::Error> {
        // Base64 takes priority - decode and return
        if let Some(b64) = &self.base64 {
            let decoded_bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(
                |e| {
                    homeboy::Error::validation_invalid_argument(
                        "base64",
                        format!("Invalid base64 encoding: {}", e),
                        None,
                        Some(vec!["Encode with: echo '{...}' | base64".to_string()]),
                    )
                },
            )?;
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
        Ok(self.json.clone().or_else(|| self.spec.clone()))
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

pub mod api;
pub mod auth;
pub mod build;
pub mod changelog;
pub mod changes;
pub mod cli;
pub mod component;
pub mod config;
pub mod db;
pub mod deploy;
pub mod docs;
pub mod file;
pub mod fleet;
pub mod git;
pub mod init;
pub mod lint;
pub mod logs;
pub mod module;
pub mod project;
pub mod release;
pub mod server;
pub mod ssh;
pub mod transfer;
pub mod test;
pub mod upgrade;
pub mod version;

pub(crate) fn run_markdown(
    command: crate::Commands,
    _global: &GlobalArgs,
) -> homeboy::Result<(String, i32)> {
    match command {
        crate::Commands::Docs(args) => docs::run_markdown(args),
        crate::Commands::Changelog(args) => changelog::run_markdown(args),
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
    ($args:expr, $module:ident) => {
        crate::output::map_cmd_result_to_json($module::run_json($args))
    };
    ($args:expr, $global:expr, $module:ident) => {
        crate::output::map_cmd_result_to_json($module::run($args, $global))
    };
}

pub(crate) fn run_json(
    command: crate::Commands,
    global: &GlobalArgs,
) -> (homeboy::Result<serde_json::Value>, i32) {
    crate::tty::status("homeboy is working...");

    match command {
        // Commands without global context
        crate::Commands::Init(args) => dispatch!(args, init),
        crate::Commands::Test(args) => dispatch!(args, test),
        crate::Commands::Lint(args) => dispatch!(args, lint),

        // Commands with global context
        crate::Commands::Project(args) => dispatch!(args, global, project),
        crate::Commands::Ssh(args) => dispatch!(args, global, ssh),
        crate::Commands::Server(args) => dispatch!(args, global, server),
        crate::Commands::Db(args) => dispatch!(args, global, db),
        crate::Commands::File(args) => dispatch!(args, global, file),
        crate::Commands::Fleet(args) => dispatch!(args, global, fleet),
        crate::Commands::Logs(args) => dispatch!(args, global, logs),
        crate::Commands::Transfer(args) => dispatch!(args, global, transfer),
        crate::Commands::Deploy(args) => dispatch!(args, global, deploy),
        crate::Commands::Component(args) => dispatch!(args, global, component),
        crate::Commands::Config(args) => dispatch!(args, global, config),
        crate::Commands::Module(args) => dispatch!(args, global, module),
        crate::Commands::Docs(args) => dispatch!(args, global, docs),
        crate::Commands::Changelog(args) => dispatch!(args, global, changelog),
        crate::Commands::Git(args) => dispatch!(args, global, git),
        crate::Commands::Version(args) => dispatch!(args, global, version),
        crate::Commands::Build(args) => dispatch!(args, global, build),
        crate::Commands::Changes(args) => dispatch!(args, global, changes),
        crate::Commands::Release(args) => dispatch!(args, global, release),
        crate::Commands::Auth(args) => dispatch!(args, global, auth),
        crate::Commands::Api(args) => dispatch!(args, global, api),
        crate::Commands::Upgrade(args) | crate::Commands::Update(args) => {
            dispatch!(args, global, upgrade)
        }

        // Special case: List uses raw output mode
        crate::Commands::List => {
            let err = homeboy::Error::validation_invalid_argument(
                "output_mode",
                "List command uses raw output mode",
                None,
                None,
            );
            crate::output::map_cmd_result_to_json::<serde_json::Value>(Err(err))
        }
    }
}
