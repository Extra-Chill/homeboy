use clap::Args;
use serde_json::{json, Map, Value};
use std::io::Read;
use std::path::Path;

pub type CmdResult<T> = homeboy::Result<(T, i32)>;

pub(crate) struct GlobalArgs {}

/// Shared arguments for dynamic set commands.
///
/// Allows arbitrary `--key value` pairs that map directly to JSON keys.
/// Flag names become JSON keys with no case conversion.
#[derive(Args, Default, Debug)]
pub struct DynamicSetArgs {
    /// Entity ID (optional if provided in JSON body)
    pub id: Option<String>,

    /// JSON spec (positional, supports @file and - for stdin)
    pub spec: Option<String>,

    /// Explicit JSON spec (takes precedence over positional)
    #[arg(long, value_name = "JSON")]
    pub json: Option<String>,

    /// Replace these fields instead of merging arrays
    #[arg(long, value_name = "FIELD")]
    pub replace: Vec<String>,

    /// Additional key=value flags (e.g., --remote-path /var/www)
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub extra: Vec<String>,
}

impl DynamicSetArgs {
    /// Get the JSON spec from either --json or positional argument
    pub fn json_spec(&self) -> Option<&str> {
        self.json.as_deref().or(self.spec.as_deref())
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

/// Read JSON spec from string, file (@path), or stdin (-).
fn read_json_spec_to_string(spec: &str) -> homeboy::Result<String> {
    use std::io::IsTerminal;

    if spec.trim() == "-" {
        let mut buf = String::new();
        let mut stdin = std::io::stdin();
        if stdin.is_terminal() {
            return Err(homeboy::Error::validation_invalid_argument(
                "json",
                "Cannot read JSON from stdin when stdin is a TTY",
                None,
                None,
            ));
        }
        stdin.read_to_string(&mut buf).map_err(|e| {
            homeboy::Error::internal_io(e.to_string(), Some("read stdin".to_string()))
        })?;
        return Ok(buf);
    }

    if let Some(path) = spec.strip_prefix('@') {
        if path.trim().is_empty() {
            return Err(homeboy::Error::validation_invalid_argument(
                "json",
                "Invalid JSON spec '@' (missing file path)",
                None,
                None,
            ));
        }
        return std::fs::read_to_string(Path::new(path)).map_err(|e| {
            homeboy::Error::internal_io(e.to_string(), Some(format!("read {}", path)))
        });
    }

    Ok(spec.to_string())
}

/// Merge JSON spec with --key value flags. Flags override spec values.
pub fn merge_json_sources(spec: Option<&str>, extra: &[String]) -> homeboy::Result<Value> {
    let mut base = if let Some(spec) = spec {
        let raw = read_json_spec_to_string(spec)?;
        serde_json::from_str(&raw).map_err(|e| {
            homeboy::Error::validation_invalid_json(
                e,
                Some("parse JSON spec".to_string()),
                Some(raw.chars().take(200).collect::<String>()),
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
pub mod git;
pub mod init;
pub mod lint;
pub mod logs;
pub mod module;
pub mod project;
pub mod release;
pub mod server;
pub mod ssh;
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
        crate::Commands::Logs(args) => dispatch!(args, global, logs),
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
