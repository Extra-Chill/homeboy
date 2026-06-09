use std::collections::HashMap;

use crate::core::observation::LAB_OFFLOAD_METADATA_ENV;

const SETTINGS_DIAGNOSTICS_SCHEMA: &str = "homeboy/lab-offload-settings-env/v1";

pub(super) fn forward_env_if_present(env: &mut HashMap<String, String>, name: &str) {
    if let Ok(value) = std::env::var(name) {
        if !value.trim().is_empty() {
            env.insert(name.to_string(), value);
        }
    }
}

pub(super) fn forward_release_ci_env(env: &mut HashMap<String, String>) {
    for name in ["GITHUB_ACTIONS", "RELEASE_BLOCKING_COMMANDS"] {
        forward_env_if_present(env, name);
    }
}

pub(super) fn build_lab_offload_env(lab_metadata: &serde_json::Value) -> HashMap<String, String> {
    HashMap::from([(
        LAB_OFFLOAD_METADATA_ENV.to_string(),
        serde_json::to_string(lab_metadata).unwrap_or_default(),
    )])
}

pub(super) fn settings_env_diagnostics(
    normalized_args: &[String],
    forwarded_env: &HashMap<String, String>,
) -> serde_json::Value {
    let settings = parsed_setting_args(normalized_args)
        .into_iter()
        .map(|setting| {
            let env_name = format!("HOMEBOY_SETTINGS_{}", setting.key.to_uppercase());
            let redacted = should_redact_setting(&setting.key, &setting.value);
            serde_json::json!({
                "key": setting.key,
                "source": setting.source,
                "env_name": env_name,
                "forwarded_to_runner": true,
                "forwarded_as": "argv",
                "remote_export_expected": true,
                "value_preview": redacted_value_preview(&setting.value, redacted),
                "redacted": redacted,
            })
        })
        .collect::<Vec<_>>();

    let mut env_names = forwarded_env.keys().cloned().collect::<Vec<_>>();
    env_names.sort();
    let forwarded_environment = env_names
        .into_iter()
        .map(|name| {
            serde_json::json!({
                "name": name,
                "forwarded_to_runner": true,
                "value_preview": "<redacted>",
                "redacted": true,
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "schema": SETTINGS_DIAGNOSTICS_SCHEMA,
        "settings": settings,
        "forwarded_environment": forwarded_environment,
    })
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedSettingArg {
    source: &'static str,
    key: String,
    value: String,
}

fn parsed_setting_args(args: &[String]) -> Vec<ParsedSettingArg> {
    let mut parsed = Vec::new();
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--setting" || arg == "--setting-json" {
            if let Some(raw) = args.get(index + 1) {
                if let Some(setting) = parse_setting_pair(setting_source(arg), raw) {
                    parsed.push(setting);
                }
            }
            index += 2;
            continue;
        }

        if let Some(raw) = arg.strip_prefix("--setting=") {
            if let Some(setting) = parse_setting_pair("setting", raw) {
                parsed.push(setting);
            }
        } else if let Some(raw) = arg.strip_prefix("--setting-json=") {
            if let Some(setting) = parse_setting_pair("setting_json", raw) {
                parsed.push(setting);
            }
        }

        index += 1;
    }

    parsed
}

fn setting_source(arg: &str) -> &'static str {
    if arg == "--setting-json" {
        "setting_json"
    } else {
        "setting"
    }
}

fn parse_setting_pair(source: &'static str, raw: &str) -> Option<ParsedSettingArg> {
    let (key, value) = raw.split_once('=')?;
    if key.trim().is_empty() {
        return None;
    }

    Some(ParsedSettingArg {
        source,
        key: key.to_string(),
        value: value.to_string(),
    })
}

fn should_redact_setting(key: &str, value: &str) -> bool {
    let lower_key = key.to_ascii_lowercase();
    let lower_value = value.to_ascii_lowercase();
    [
        "secret",
        "token",
        "password",
        "passwd",
        "credential",
        "authorization",
        "auth",
        "cookie",
        "api_key",
        "apikey",
        "private_key",
    ]
    .iter()
    .any(|needle| lower_key.contains(needle) || lower_value.contains(needle))
}

fn redacted_value_preview(value: &str, redacted: bool) -> String {
    if redacted {
        return "<redacted>".to_string();
    }

    const MAX_PREVIEW_CHARS: usize = 160;
    if value.chars().count() <= MAX_PREVIEW_CHARS {
        return value.to_string();
    }

    let mut preview = value.chars().take(MAX_PREVIEW_CHARS).collect::<String>();
    preview.push_str("...");
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EnvVarGuard {
        name: &'static str,
        prior: Option<String>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let prior = std::env::var(name).ok();
            std::env::set_var(name, value);
            Self { name, prior }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[test]
    fn parsed_setting_args_reads_split_and_equals_forms() {
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--setting".to_string(),
            "wp_codebox_bin=/tmp/codebox.js".to_string(),
            "--setting-json={\"ignored\":true}".to_string(),
            "--setting-json".to_string(),
            "retries=3".to_string(),
            "--setting=mode=fast".to_string(),
        ];

        assert_eq!(
            parsed_setting_args(&args),
            vec![
                ParsedSettingArg {
                    source: "setting",
                    key: "wp_codebox_bin".to_string(),
                    value: "/tmp/codebox.js".to_string(),
                },
                ParsedSettingArg {
                    source: "setting_json",
                    key: "retries".to_string(),
                    value: "3".to_string(),
                },
                ParsedSettingArg {
                    source: "setting",
                    key: "mode".to_string(),
                    value: "fast".to_string(),
                },
            ]
        );
    }

    #[test]
    fn settings_env_diagnostics_reports_expected_env_names_and_redacts_secrets() {
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--setting".to_string(),
            "wp_codebox_bin=/tmp/codebox.js".to_string(),
            "--setting".to_string(),
            "api_token=secret-value".to_string(),
        ];
        let mut env = HashMap::new();
        env.insert(
            LAB_OFFLOAD_METADATA_ENV.to_string(),
            "{\"schema\":\"homeboy/lab-offload/v1\"}".to_string(),
        );

        let diagnostics = settings_env_diagnostics(&args, &env);

        assert_eq!(diagnostics["schema"], SETTINGS_DIAGNOSTICS_SCHEMA);
        assert_eq!(diagnostics["settings"][0]["key"], "wp_codebox_bin");
        assert_eq!(
            diagnostics["settings"][0]["env_name"],
            "HOMEBOY_SETTINGS_WP_CODEBOX_BIN"
        );
        assert_eq!(
            diagnostics["settings"][0]["value_preview"],
            "/tmp/codebox.js"
        );
        assert_eq!(diagnostics["settings"][0]["forwarded_as"], "argv");
        assert_eq!(diagnostics["settings"][0]["remote_export_expected"], true);
        assert_eq!(
            diagnostics["settings"][1]["env_name"],
            "HOMEBOY_SETTINGS_API_TOKEN"
        );
        assert_eq!(diagnostics["settings"][1]["value_preview"], "<redacted>");
        assert_eq!(diagnostics["settings"][1]["redacted"], true);
        assert_eq!(
            diagnostics["forwarded_environment"][0]["name"],
            LAB_OFFLOAD_METADATA_ENV
        );
        assert_eq!(
            diagnostics["forwarded_environment"][0]["value_preview"],
            "<redacted>"
        );
    }

    #[test]
    fn forward_release_ci_env_preserves_release_gate_context() {
        let _github_actions = EnvVarGuard::set("GITHUB_ACTIONS", "true");
        let _blocking = EnvVarGuard::set("RELEASE_BLOCKING_COMMANDS", "lint,test");
        let mut env = HashMap::new();

        forward_release_ci_env(&mut env);

        assert_eq!(env.get("GITHUB_ACTIONS").map(String::as_str), Some("true"));
        assert_eq!(
            env.get("RELEASE_BLOCKING_COMMANDS").map(String::as_str),
            Some("lint,test")
        );
    }
}
