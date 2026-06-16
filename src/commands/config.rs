use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;

use homeboy::core::defaults::{self, Defaults};

use super::CmdResult;

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Display configuration (merged defaults + file)
    Show {
        /// Show only built-in defaults (ignore homeboy.json)
        #[arg(long)]
        builtin: bool,
    },
    /// Set a configuration value at a JSON pointer path
    Set {
        /// JSON pointer path (e.g., /defaults/deploy/scp_flags)
        pointer: String,
        /// Value to set (JSON)
        value: String,
        /// Treat value as a literal string instead of parsing it as JSON
        #[arg(long)]
        string: bool,
    },
    /// Remove a configuration value at a JSON pointer path
    Remove {
        /// JSON pointer path (e.g., /defaults/deploy/scp_flags)
        pointer: String,
    },
    /// Reset configuration to built-in defaults (deletes homeboy.json)
    Reset,
    /// Show the path to homeboy.json
    Path,
}

#[derive(Debug, Serialize)]
pub struct ConfigOutput {
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    defaults: Option<Defaults>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exists: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pointer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted: Option<bool>,
}

pub fn run(args: ConfigArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<ConfigOutput> {
    match args.command {
        ConfigCommand::Show { builtin } => show(builtin),
        ConfigCommand::Set {
            pointer,
            value,
            string,
        } => set(&pointer, &value, string),
        ConfigCommand::Remove { pointer } => remove(&pointer),
        ConfigCommand::Reset => reset(),
        ConfigCommand::Path => path(),
    }
}

fn show(builtin: bool) -> CmdResult<ConfigOutput> {
    if builtin {
        Ok((
            ConfigOutput {
                command: "config.show".to_string(),
                defaults: Some(defaults::builtin_defaults()),
                config: None,
                path: None,
                exists: None,
                pointer: None,
                value: None,
                deleted: None,
            },
            0,
        ))
    } else {
        let config = defaults::load_config();
        let config = redacted_config_value(&config)?;
        Ok((
            ConfigOutput {
                command: "config.show".to_string(),
                config: Some(config),
                defaults: None,
                path: None,
                exists: None,
                pointer: None,
                value: None,
                deleted: None,
            },
            0,
        ))
    }
}

fn set(pointer: &str, value_str: &str, string: bool) -> CmdResult<ConfigOutput> {
    // Validate pointer format
    if !pointer.starts_with('/') {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "pointer",
            "JSON pointer must start with '/'",
            None,
            None,
        ));
    }

    let value = parse_config_set_value(pointer, value_str, string)?;

    // Load current config (or create default)
    let mut config = defaults::load_config();

    // Convert to JSON, set the value, convert back
    let mut config_json = serde_json::to_value(&config).map_err(|e| {
        homeboy::core::Error::internal_unexpected(format!("Failed to serialize config: {}", e))
    })?;

    // Navigate to the pointer location and set the value
    homeboy::core::config::set_json_pointer(&mut config_json, pointer, value.clone())?;

    // Convert back to HomeboyConfig
    config = serde_json::from_value(config_json).map_err(|e| {
        homeboy::core::Error::validation_invalid_json(
            e,
            Some("deserialize config".to_string()),
            None,
        )
    })?;

    // Save the config
    defaults::save_config(&config)?;
    let redacted_config = redacted_config_value(&config)?;

    Ok((
        ConfigOutput {
            command: "config.set".to_string(),
            config: Some(redacted_config),
            defaults: None,
            path: None,
            exists: None,
            pointer: Some(pointer.to_string()),
            value: Some(value),
            deleted: None,
        },
        0,
    ))
}

fn parse_config_set_value(
    pointer: &str,
    value_str: &str,
    string: bool,
) -> homeboy::core::Result<Value> {
    if string {
        return Ok(Value::String(value_str.to_string()));
    }

    serde_json::from_str(value_str).map_err(|e| {
        let mut err = homeboy::core::Error::validation_invalid_json(
            e,
            Some("parse config set value".to_string()),
            Some(value_str.chars().take(200).collect::<String>()),
        );

        if looks_like_unquoted_string(value_str) {
            let json_string = serde_json::to_string(value_str).unwrap_or_else(|_| "\"...\"".to_string());
            err = err
                .with_hint(format!(
                    "String config values must be JSON strings. Try: homeboy config set {} '{}'",
                    pointer, json_string
                ))
                .with_hint(format!(
                    "Or pass --string to store the value literally: homeboy config set {} {} --string",
                    pointer, value_str
                ));
        }

        err
    })
}

fn looks_like_unquoted_string(value_str: &str) -> bool {
    let value = value_str.trim();
    if value.is_empty() {
        return false;
    }

    let Some(first) = value.chars().next() else {
        return false;
    };

    first.is_ascii_alphabetic() || first == '_' || first == '/' || first == '~'
}

fn remove(pointer: &str) -> CmdResult<ConfigOutput> {
    // Validate pointer format
    if !pointer.starts_with('/') {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "pointer",
            "JSON pointer must start with '/'",
            None,
            None,
        ));
    }

    // Load current config
    let mut config = defaults::load_config();

    // Convert to JSON
    let mut config_json = serde_json::to_value(&config).map_err(|e| {
        homeboy::core::Error::internal_unexpected(format!("Failed to serialize config: {}", e))
    })?;

    // Remove the value at the pointer
    homeboy::core::config::remove_json_pointer(&mut config_json, pointer)?;

    // Convert back to HomeboyConfig
    config = serde_json::from_value(config_json).map_err(|e| {
        homeboy::core::Error::validation_invalid_json(
            e,
            Some("deserialize config".to_string()),
            None,
        )
    })?;

    // Save the config
    defaults::save_config(&config)?;
    let redacted_config = redacted_config_value(&config)?;

    Ok((
        ConfigOutput {
            command: "config.remove".to_string(),
            config: Some(redacted_config),
            defaults: None,
            path: None,
            exists: None,
            pointer: Some(pointer.to_string()),
            value: None,
            deleted: None,
        },
        0,
    ))
}

fn redacted_config_value(config: &defaults::HomeboyConfig) -> homeboy::core::Result<Value> {
    let mut value = serde_json::to_value(config).map_err(|e| {
        homeboy::core::Error::internal_unexpected(format!("Failed to serialize config: {}", e))
    })?;

    if let Some(secrets) = value
        .pointer_mut("/agent_task/secrets")
        .and_then(Value::as_object_mut)
    {
        for source in secrets.values_mut() {
            if let Some(source) = source.as_object_mut() {
                if source.contains_key("value") {
                    source.insert("value".to_string(), Value::String("[redacted]".to_string()));
                }
            }
        }
    }

    Ok(value)
}

fn reset() -> CmdResult<ConfigOutput> {
    let deleted = defaults::reset_config()?;

    Ok((
        ConfigOutput {
            command: "config.reset".to_string(),
            config: None,
            defaults: Some(defaults::builtin_defaults()),
            path: Some(defaults::config_path()?),
            exists: None,
            pointer: None,
            value: None,
            deleted: Some(deleted),
        },
        0,
    ))
}

fn path() -> CmdResult<ConfigOutput> {
    let path = defaults::config_path()?;
    let exists = defaults::config_exists();

    Ok((
        ConfigOutput {
            command: "config.path".to_string(),
            config: None,
            defaults: None,
            path: Some(path),
            exists: Some(exists),
            pointer: None,
            value: None,
            deleted: None,
        },
        0,
    ))
}

// JSON pointer operations (set_json_pointer, remove_json_pointer) are in
// homeboy::core::config — no local implementations needed.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_set_string_mode_stores_literal_string() {
        let value = parse_config_set_value("/settings/wp_codebox_provider", "codex", true)
            .expect("literal string value");

        assert_eq!(value, Value::String("codex".to_string()));
    }

    #[test]
    fn config_set_unquoted_string_error_includes_string_hints() {
        let err = parse_config_set_value("/settings/wp_codebox_provider", "codex", false)
            .expect_err("bare string should not parse as JSON");

        let hints = err
            .hints
            .iter()
            .map(|hint| hint.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(hints.contains("'\"codex\"'"));
        assert!(hints.contains("--string"));
    }

    #[test]
    fn config_set_json_mode_keeps_json_values() {
        let value = parse_config_set_value("/defaults/deploy/scp_flags", "[]", false)
            .expect("json array value");

        assert_eq!(value, Value::Array(Vec::new()));
    }
}
