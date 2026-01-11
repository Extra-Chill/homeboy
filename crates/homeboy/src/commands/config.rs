use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy_core::config::{
    find_config_key, parse_config_value, AppPaths, ConfigManager, ConfigValueType,
    KNOWN_CONFIG_KEYS,
};
use homeboy_core::json::{
    read_json_file, remove_json_pointer, set_json_pointer, write_json_file_pretty,
};

use super::CmdResult;

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    command: ConfigCommand,
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Print the Homeboy config file path
    Path,

    /// Show current global config.json
    Show,

    /// Set a known global config key
    Set {
        /// Known key name (see `keys`)
        key: String,

        /// Value (string) or comma-separated string list
        value: String,
    },

    /// Unset a known global config key
    Unset {
        /// Known key name (see `keys`)
        key: String,
    },

    /// List known global config keys
    Keys,

    /// Set a JSON value using a JSON pointer
    SetJson {
        /// JSON pointer (e.g. `/activeProjectId`)
        pointer: String,

        /// JSON value (must be valid JSON)
        value: String,

        /// Allow pointers not in the known-key registry
        #[arg(long)]
        allow_unknown: bool,
    },
}

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "camelCase")]
pub enum ConfigOutput {
    Path {
        path: String,
    },
    Show {
        path: String,
        config: homeboy_core::config::AppConfig,
    },
    Keys {
        keys: Vec<ConfigKeyOutput>,
    },
    Set {
        path: String,
        key: String,
        pointer: String,
        value: serde_json::Value,
    },
    Unset {
        path: String,
        key: String,
        pointer: String,
    },
    SetJson {
        path: String,
        pointer: String,
        value: serde_json::Value,
        allow_unknown: bool,
        recognized_key: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigKeyOutput {
    pub name: String,
    pub pointer: String,
    pub value_type: String,
}

pub fn run(args: ConfigArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<ConfigOutput> {
    match args.command {
        ConfigCommand::Path => {
            let path = AppPaths::config()?;
            Ok((
                ConfigOutput::Path {
                    path: path.to_string_lossy().to_string(),
                },
                0,
            ))
        }
        ConfigCommand::Show => {
            let path = AppPaths::config()?;
            let config = ConfigManager::load_app_config()?;
            Ok((
                ConfigOutput::Show {
                    path: path.to_string_lossy().to_string(),
                    config,
                },
                0,
            ))
        }
        ConfigCommand::Keys => {
            let keys = KNOWN_CONFIG_KEYS
                .iter()
                .map(|k| ConfigKeyOutput {
                    name: k.name.to_string(),
                    pointer: k.pointer.to_string(),
                    value_type: match k.value_type {
                        ConfigValueType::String => "string".to_string(),
                        ConfigValueType::StringArray => "stringArray".to_string(),
                    },
                })
                .collect();

            Ok((ConfigOutput::Keys { keys }, 0))
        }
        ConfigCommand::Set { key, value } => {
            let Some(known) = find_config_key(&key) else {
                return Err(homeboy_core::Error::config(format!(
                    "Unknown config key: {}. Use `homeboy config keys`.",
                    key
                )));
            };

            let json_value = parse_config_value(known.value_type, &value)?;

            let path = AppPaths::config()?;
            AppPaths::ensure_directories()?;

            let mut root = if path.exists() {
                read_json_file(&path)?
            } else {
                serde_json::Value::Object(serde_json::Map::new())
            };

            set_json_pointer(&mut root, known.pointer, json_value.clone())?;
            write_json_file_pretty(&path, &root)?;

            Ok((
                ConfigOutput::Set {
                    path: path.to_string_lossy().to_string(),
                    key,
                    pointer: known.pointer.to_string(),
                    value: json_value,
                },
                0,
            ))
        }
        ConfigCommand::Unset { key } => {
            let Some(known) = find_config_key(&key) else {
                return Err(homeboy_core::Error::config(format!(
                    "Unknown config key: {}. Use `homeboy config keys`.",
                    key
                )));
            };

            let path = AppPaths::config()?;
            if !path.exists() {
                return Err(homeboy_core::Error::config(format!(
                    "Config file not found: {}",
                    path.display()
                )));
            }

            let mut root = read_json_file(&path)?;
            remove_json_pointer(&mut root, known.pointer)?;
            write_json_file_pretty(&path, &root)?;

            Ok((
                ConfigOutput::Unset {
                    path: path.to_string_lossy().to_string(),
                    key,
                    pointer: known.pointer.to_string(),
                },
                0,
            ))
        }
        ConfigCommand::SetJson {
            pointer,
            value,
            allow_unknown,
        } => {
            let recognized_key = homeboy_core::config::find_config_key_by_pointer(&pointer)
                .map(|k| k.name.to_string());

            if recognized_key.is_none() && !allow_unknown {
                return Err(homeboy_core::Error::config(format!(
                    "Unknown JSON pointer: {}. Pass --allow-unknown to override.",
                    pointer
                )));
            }

            let json_value: serde_json::Value = serde_json::from_str(&value)
                .map_err(|e| homeboy_core::Error::config(format!("Invalid JSON value: {}", e)))?;

            let path = AppPaths::config()?;
            AppPaths::ensure_directories()?;

            let mut root = if path.exists() {
                read_json_file(&path)?
            } else {
                serde_json::Value::Object(serde_json::Map::new())
            };

            set_json_pointer(&mut root, &pointer, json_value.clone())?;
            write_json_file_pretty(&path, &root)?;

            Ok((
                ConfigOutput::SetJson {
                    path: path.to_string_lossy().to_string(),
                    pointer,
                    value: json_value,
                    allow_unknown,
                    recognized_key,
                },
                0,
            ))
        }
    }
}
