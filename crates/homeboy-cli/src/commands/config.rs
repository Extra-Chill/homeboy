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
        /// JSON pointer path to read (e.g., /notifications/default_transport)
        pointer: Option<String>,
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
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted: Option<bool>,
}

pub fn run(args: ConfigArgs) -> CmdResult<ConfigOutput> {
    match args.command {
        ConfigCommand::Show { builtin, pointer } => show(builtin, pointer.as_deref()),
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

fn show(builtin: bool, pointer: Option<&str>) -> CmdResult<ConfigOutput> {
    if let Some(pointer) = pointer {
        return show_pointer(builtin, pointer);
    }

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
                source: None,
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
                source: None,
                deleted: None,
            },
            0,
        ))
    }
}

fn show_pointer(builtin: bool, pointer: &str) -> CmdResult<ConfigOutput> {
    if !pointer.starts_with('/') {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "pointer",
            "JSON pointer must start with '/'",
            None,
            None,
        ));
    }

    let config = if builtin {
        serde_json::to_value(defaults::HomeboyConfig::default()).map_err(|error| {
            homeboy::core::Error::internal_unexpected(format!(
                "Failed to serialize built-in config: {error}"
            ))
        })?
    } else {
        redacted_config_value(&defaults::load_config())?
    };
    let value = homeboy::core::config::get_json_pointer(&config, pointer)?
        .cloned()
        .ok_or_else(|| missing_pointer_error(&config, pointer))?;
    let source = if !builtin && defaults::config_file_value(pointer).is_some() {
        "file"
    } else {
        "builtin"
    };
    let path = (source == "file").then(defaults::config_path).transpose()?;

    Ok((
        ConfigOutput {
            command: "config.show".to_string(),
            config: None,
            defaults: None,
            path,
            exists: None,
            pointer: Some(pointer.to_string()),
            value: Some(value),
            source: Some(source.to_string()),
            deleted: None,
        },
        0,
    ))
}

fn missing_pointer_error(config: &Value, pointer: &str) -> homeboy::core::Error {
    let suggestions = nearby_pointer_paths(config, pointer);
    let mut error = homeboy::core::Error::config_missing_key(pointer, None);
    if !suggestions.is_empty() {
        error = error.with_hint(format!("Nearby valid paths: {}", suggestions.join(", ")));
    }
    error
}

fn nearby_pointer_paths(config: &Value, pointer: &str) -> Vec<String> {
    let parent = pointer
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("");
    let Ok(Some(parent)) = homeboy::core::config::get_json_pointer(config, parent) else {
        return Vec::new();
    };

    match parent {
        Value::Object(values) => values
            .keys()
            .take(5)
            .map(|key| {
                format!(
                    "{}/{}",
                    pointer_parent_display(pointer),
                    escape_pointer_token(key)
                )
            })
            .collect(),
        Value::Array(values) => (0..values.len().min(5))
            .map(|index| format!("{}/{}", pointer_parent_display(pointer), index))
            .collect(),
        _ => Vec::new(),
    }
}

fn pointer_parent_display(pointer: &str) -> &str {
    pointer
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("")
}

fn escape_pointer_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
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
    homeboy::core::worktree_provider::validate_configured_worktree_creation_contracts(&config)?;

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
            source: None,
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
    homeboy::core::worktree_provider::validate_configured_worktree_creation_contracts(&config)?;

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
            source: None,
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
            source: None,
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
            source: None,
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
        let value = parse_config_set_value("/settings/provider", "example", true)
            .expect("literal string value");

        assert_eq!(value, Value::String("example".to_string()));
    }

    #[test]
    fn config_set_unquoted_string_error_includes_string_hints() {
        let err = parse_config_set_value("/settings/provider", "example", false)
            .expect_err("bare string should not parse as JSON");

        let hints = err
            .hints
            .iter()
            .map(|hint| hint.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(hints.contains("'\"example\"'"));
        assert!(hints.contains("--string"));
    }

    #[test]
    fn config_set_json_mode_keeps_json_values() {
        let value = parse_config_set_value("/defaults/deploy/scp_flags", "[]", false)
            .expect("json array value");

        assert_eq!(value, Value::Array(Vec::new()));
    }

    #[test]
    fn pointer_read_supports_nested_objects_and_arrays() {
        let config = serde_json::json!({
            "notifications": { "default_transport": "discord" },
            "defaults": { "version_candidates": [{ "file": "Cargo.toml" }] },
        });

        assert_eq!(
            homeboy::core::config::get_json_pointer(&config, "/notifications")
                .expect("valid pointer"),
            Some(&serde_json::json!({ "default_transport": "discord" }))
        );
        assert_eq!(
            homeboy::core::config::get_json_pointer(&config, "/defaults/version_candidates/0/file")
                .expect("valid pointer"),
            Some(&serde_json::json!("Cargo.toml"))
        );
    }

    #[test]
    fn missing_pointer_is_typed_and_lists_nearby_paths() {
        let config = serde_json::json!({
            "notifications": { "default_transport": "discord" },
        });

        let error = missing_pointer_error(&config, "/notifications/default_transprot");

        assert_eq!(
            error.code,
            homeboy::core::error::ErrorCode::ConfigMissingKey
        );
        assert_eq!(error.details["key"], "/notifications/default_transprot");
        assert!(
            error
                .hints
                .iter()
                .any(|hint| hint.message.contains("/notifications/default_transport")),
            "missing-pointer guidance must name a nearby valid path: {error:?}"
        );
    }

    #[test]
    fn pointer_read_redacts_secret_values() {
        let config: defaults::HomeboyConfig = serde_json::from_value(serde_json::json!({
            "agent_task": {
                "secrets": {
                    "api_token": { "source": "literal", "value": "secret" }
                }
            }
        }))
        .expect("config with secret");
        let redacted = redacted_config_value(&config).expect("redact config");

        assert_eq!(
            homeboy::core::config::get_json_pointer(
                &redacted,
                "/agent_task/secrets/api_token/value"
            )
            .expect("valid pointer"),
            Some(&serde_json::json!("[redacted]"))
        );
    }

    #[test]
    fn pointer_read_reports_file_and_builtin_ownership() {
        homeboy::core::test_support::with_isolated_home(|home| {
            let path = home.path().join(".config/homeboy/homeboy.json");
            std::fs::create_dir_all(path.parent().expect("config parent")).expect("config dir");
            std::fs::write(
                path,
                r#"{"notifications":{"default_transport":"discord.run-completion"}}"#,
            )
            .expect("config file");

            let (file, _) = show_pointer(false, "/notifications/default_transport")
                .expect("file-backed pointer read");
            assert_eq!(file.source.as_deref(), Some("file"));
            assert!(file.path.is_some());
            assert_eq!(
                file.value,
                Some(serde_json::json!("discord.run-completion"))
            );

            let (builtin, _) = show_pointer(false, "/defaults/deploy/scp_flags")
                .expect("builtin-backed pointer read");
            assert_eq!(builtin.source.as_deref(), Some("builtin"));
            assert_eq!(builtin.path, None);

            let (builtin_only, _) =
                show_pointer(true, "/defaults/deploy/scp_flags").expect("builtin pointer read");
            assert_eq!(builtin_only.source.as_deref(), Some("builtin"));
            assert_eq!(builtin_only.path, None);
        });
    }

    #[test]
    fn config_set_rejects_an_incomplete_active_worktree_provider() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let error = set(
                "/worktree_providers/dmc",
                r#"{"enabled":true,"apply_enabled":true,"commands":{"ensure":["provider","ensure","{handle}"]}}"#,
                false,
            )
            .expect_err("an active provider needs a postcondition lookup");

            assert!(error.message.contains("worktree provider `dmc`"));
            assert_eq!(
                error.details["worktree_provider_missing_required_capabilities"],
                serde_json::json!(["resolve_or_list"])
            );
            assert!(error.hints.iter().any(|hint| hint.message.contains(
                "/worktree_providers/dmc/commands/resolve or /worktree_providers/dmc/commands/list"
            )));
            assert!(
                !defaults::config_exists(),
                "invalid config must not persist"
            );
        });
    }

    #[test]
    fn config_set_allows_a_staged_inactive_worktree_provider() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let (output, status) = set(
                "/worktree_providers/dmc",
                r#"{"enabled":true,"apply_enabled":false,"commands":{"ensure":["provider","ensure","{handle}"]}}"#,
                false,
            )
            .expect("an inactive provider can be configured in stages");

            assert_eq!(status, 0);
            assert_eq!(
                output
                    .config
                    .as_ref()
                    .and_then(|config| config.pointer("/worktree_providers/dmc/apply_enabled")),
                Some(&serde_json::json!(false))
            );
            assert!(defaults::config_exists(), "staged config must persist");
        });
    }

    #[test]
    fn config_set_rejects_an_active_worktree_provider_missing_ensure() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let error = set(
                "/worktree_providers/dmc",
                r#"{"enabled":true,"apply_enabled":true,"commands":{"resolve":["provider","resolve","{handle}"]}}"#,
                false,
            )
            .expect_err("an active provider needs ensure");

            assert_eq!(
                error.details["worktree_provider_missing_required_capabilities"],
                serde_json::json!(["ensure"])
            );
            assert!(error.hints.iter().any(|hint| {
                hint.message
                    .contains("/worktree_providers/dmc/commands/ensure")
            }));
            assert!(
                !defaults::config_exists(),
                "invalid config must not persist"
            );
        });
    }

    #[test]
    fn config_remove_rejects_removing_an_active_provider_lookup() {
        homeboy::core::test_support::with_isolated_home(|_| {
            set(
                "/worktree_providers/dmc",
                r#"{"enabled":true,"apply_enabled":true,"commands":{"ensure":["provider","ensure","{handle}"],"resolve":["provider","resolve","{handle}"]}}"#,
                false,
            )
            .expect("complete active provider");

            let error = remove("/worktree_providers/dmc/commands/resolve")
                .expect_err("removing the active provider lookup must fail");

            assert_eq!(
                error.details["worktree_provider_missing_required_capabilities"],
                serde_json::json!(["resolve_or_list"])
            );
            assert!(
                defaults::config_file_value("/worktree_providers/dmc/commands/resolve").is_some(),
                "the rejected removal must not persist"
            );
        });
    }

    #[test]
    fn config_set_rejects_activating_a_staged_incomplete_worktree_provider() {
        homeboy::core::test_support::with_isolated_home(|_| {
            set(
                "/worktree_providers/dmc",
                r#"{"enabled":true,"apply_enabled":false,"commands":{"ensure":["provider","ensure","{handle}"]}}"#,
                false,
            )
            .expect("staged incomplete provider");

            let error = set("/worktree_providers/dmc/apply_enabled", "true", false)
                .expect_err("activating an incomplete staged provider must fail");

            assert_eq!(
                error.details["worktree_provider_missing_required_capabilities"],
                serde_json::json!(["resolve_or_list"])
            );
            assert_eq!(
                defaults::config_file_value("/worktree_providers/dmc/apply_enabled"),
                Some(serde_json::json!(false)),
                "the rejected activation must not persist"
            );
        });
    }
}
