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
    #[command(visible_alias = "get")]
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
    defaults: Option<Value>,
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

pub(crate) fn is_read(args: &ConfigArgs) -> bool {
    matches!(args.command, ConfigCommand::Show { .. })
}

fn show(builtin: bool, pointer: Option<&str>) -> CmdResult<ConfigOutput> {
    if let Some(pointer) = pointer {
        return show_pointer(builtin, pointer);
    }

    if builtin {
        let mut defaults = defaults_value(&defaults::builtin_defaults())?;
        elide_large_install_scripts(&mut defaults, "/install_methods", " --builtin");
        Ok((
            ConfigOutput {
                command: "config.show".to_string(),
                defaults: Some(defaults),
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
        let config = defaults::load_config_for_read()?;
        let mut config = redacted_config_value(&config)?;
        elide_large_install_scripts(&mut config, "/defaults/install_methods", "");
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

/// Threshold, in characters, above which an `upgrade_command` install script
/// is considered too large to usefully appear inline in an unscoped `config
/// show`. Short one-liners (e.g. `brew update && brew upgrade homeboy`) stay
/// inline; the multi-line source/binary bootstrap scripts do not.
const INSTALL_SCRIPT_ELISION_THRESHOLD: usize = 200;

/// Replace long, embedded install/upgrade shell scripts with a short summary
/// in an unscoped `config show` dump.
///
/// `config show` (no pointer) is the natural first command an operator runs
/// to inspect state, but `defaults.install_methods` carries multi-line
/// binary-upgrade and source-build scripts that otherwise dominate the
/// output and bury the settings people actually came to read (#13635). A
/// pointer read of a specific `upgrade_command` still returns the full
/// script verbatim — nothing here changes what data exists, only what an
/// unscoped dump shows by default.
fn elide_large_install_scripts(
    root: &mut Value,
    install_methods_pointer: &str,
    builtin_flag: &str,
) {
    let Some(methods) = root
        .pointer_mut(install_methods_pointer)
        .and_then(Value::as_object_mut)
    else {
        return;
    };

    for (key, method) in methods.iter_mut() {
        let Some(method) = method.as_object_mut() else {
            continue;
        };
        let Some(command) = method.get("upgrade_command").and_then(Value::as_str) else {
            continue;
        };
        if command.chars().count() <= INSTALL_SCRIPT_ELISION_THRESHOLD && !command.contains('\n') {
            continue;
        }

        let chars = command.chars().count();
        let elided = format!(
            "[{chars} chars omitted; run `homeboy config show{builtin_flag} /defaults/install_methods/{key}/upgrade_command` to view the full script]"
        );
        method.insert("upgrade_command".to_string(), Value::String(elided));
    }
}

fn defaults_value(defaults: &Defaults) -> homeboy::core::Result<Value> {
    serde_json::to_value(defaults).map_err(|error| {
        homeboy::core::Error::internal_unexpected(format!(
            "Failed to serialize built-in defaults: {error}"
        ))
    })
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

    let (config, file_value) = if builtin {
        (
            serde_json::to_value(defaults::HomeboyConfig::default()).map_err(|error| {
                homeboy::core::Error::internal_unexpected(format!(
                    "Failed to serialize built-in config: {error}"
                ))
            })?,
            None,
        )
    } else {
        let (config, file_value) = defaults::load_config_and_file_value_for_read(pointer)?;
        (redacted_config_value(&config)?, file_value)
    };
    let value = homeboy::core::config::get_json_pointer(&config, pointer)?
        .cloned()
        .ok_or_else(|| missing_pointer_error(&config, pointer))?;
    let source = if !builtin && file_value.is_some() {
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
            defaults: Some(defaults_value(&defaults::builtin_defaults())?),
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
    use clap::Parser;

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
    fn config_get_is_a_read_only_alias_for_show() {
        let cli = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "config",
            "get",
            "/agent_task/default_backend",
        ])
        .expect("config get parses");
        let crate::cli_surface::Commands::Config(args) = cli.command else {
            panic!("config get must select the config command");
        };

        assert!(is_read(&args));
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

    /// Reproduces #13635: an unscoped `config show` used to dump the entire
    /// multi-line binary-upgrade and source-build scripts inline, burying
    /// settings like `agent_task`/`notifications`/`release_gate` in ~15KB of
    /// install-method shell programs. The actual settings must be readable
    /// without the embedded scripts drowning them out.
    #[test]
    fn unscoped_show_elides_large_install_scripts_but_keeps_real_settings_readable() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let (output, _) = show(false, None).expect("unscoped config show");
            let config = output.config.expect("merged config value");

            // Real settings the user actually came to read remain intact.
            assert!(
                config.get("agent_task").is_some(),
                "agent_task settings must still be present: {config}"
            );
            assert!(
                config.get("notifications").is_some(),
                "notifications settings must still be present: {config}"
            );
            assert!(
                config.get("release_gate").is_some(),
                "release_gate settings must still be present: {config}"
            );

            // The large embedded install/upgrade shell scripts (source build,
            // binary upgrade) no longer dominate the output.
            let source_script = config
                .pointer("/defaults/install_methods/source/upgrade_command")
                .and_then(Value::as_str)
                .expect("source upgrade_command present");
            let binary_script = config
                .pointer("/defaults/install_methods/binary/upgrade_command")
                .and_then(Value::as_str)
                .expect("binary upgrade_command present");
            assert!(
                source_script.len() < INSTALL_SCRIPT_ELISION_THRESHOLD + 200,
                "source upgrade_command should be elided in an unscoped dump: {source_script}"
            );
            assert!(
                binary_script.len() < INSTALL_SCRIPT_ELISION_THRESHOLD + 200,
                "binary upgrade_command should be elided in an unscoped dump: {binary_script}"
            );
            assert!(!source_script.contains('\n'));
            assert!(!binary_script.contains('\n'));

            // The elision must not silently discard data: it names the exact
            // pointer that returns the real script.
            assert!(source_script.contains("/defaults/install_methods/source/upgrade_command"));
            assert!(binary_script.contains("/defaults/install_methods/binary/upgrade_command"));

            // A short, unmodified one-liner install command stays inline.
            let homebrew_script = config
                .pointer("/defaults/install_methods/homebrew/upgrade_command")
                .and_then(Value::as_str)
                .expect("homebrew upgrade_command present");
            assert_eq!(homebrew_script, "brew update && brew upgrade homeboy");

            // Overall output shrinks dramatically now that the scripts are elided.
            let serialized = serde_json::to_string(&config).expect("serialize config");
            assert!(
                serialized.len() < 4000,
                "unscoped config show should be readable, got {} bytes",
                serialized.len()
            );
        });
    }

    /// A pointer read explicitly requesting the script still returns it in
    /// full — eliding only changes what an *unscoped* dump shows by default.
    #[test]
    fn pointer_read_still_returns_the_full_install_script() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let (output, _) =
                show_pointer(false, "/defaults/install_methods/source/upgrade_command")
                    .expect("explicit pointer read of the install script");
            let value = output
                .value
                .expect("script value")
                .as_str()
                .unwrap()
                .to_string();

            assert!(value.contains('\n'), "full script must remain multi-line");
            assert!(
                value.len() > INSTALL_SCRIPT_ELISION_THRESHOLD,
                "an explicit pointer read must not be elided"
            );
        });
    }
}
