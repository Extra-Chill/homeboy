//! Persisted extension setup runtime env.
//!
//! When an extension's `setup` runs on a runner (e.g. a Lab runner), it can
//! discover runtime-derived values — for example a Managed Sandbox core module path
//! resolved from a freshly built checkout. Those values are produced by the
//! extension's env provider but, historically, were only materialized live
//! during the same process that ran setup. A subsequent capability execution
//! (such as `homeboy fuzz run`) on the same runner started a fresh process and
//! re-derived the env from scratch, which could fail if the live derivation
//! depended on transient setup state.
//!
//! This module persists the setup-discovered runtime env to a small per-runner
//! JSON file so capability execution replays the *same effective env* that setup
//! discovered, removing the need for operators to inject runner-local
//! env/settings manually after setup succeeds.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::core::engine::local_files;
use crate::core::error::Result;
use crate::core::paths;

/// Schema marker for the persisted setup-env document.
const SETUP_ENV_SCHEMA: &str = "homeboy/extension-setup-env/v1";

/// Directory (under the machine-local homeboy data root) that stores persisted
/// per-extension setup runtime env documents.
fn setup_env_dir() -> Result<PathBuf> {
    Ok(paths::homeboy_data()?.join("extension-setup-env"))
}

/// Path to the persisted setup-env document for a single extension.
fn setup_env_path(extension_id: &str) -> Result<PathBuf> {
    Ok(setup_env_dir()?.join(format!("{}.json", sanitize_extension_id(extension_id))))
}

/// Keep file names filesystem-safe regardless of extension id formatting.
fn sanitize_extension_id(extension_id: &str) -> String {
    extension_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Persist the setup-discovered runtime env for `extension_id`.
///
/// Stored as a sorted map so replay is deterministic. Passing an empty env
/// removes any previously persisted document so stale values never leak into a
/// later capability execution.
pub(crate) fn persist(extension_id: &str, env: &[(String, String)]) -> Result<()> {
    let path = setup_env_path(extension_id)?;

    if env.is_empty() {
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            crate::core::Error::internal_io(
                error.to_string(),
                Some("create extension setup-env directory".to_string()),
            )
        })?;
    }

    let env_map: BTreeMap<&str, &str> = env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let document = serde_json::json!({
        "schema": SETUP_ENV_SCHEMA,
        "extension_id": extension_id,
        "env": env_map,
    });
    let serialized = serde_json::to_string_pretty(&document).map_err(|error| {
        crate::core::Error::internal_json(
            error.to_string(),
            Some("serialize extension setup env".to_string()),
        )
    })?;

    local_files::write_file_atomic(&path, &serialized, "write extension setup env")
}

/// Load the persisted setup runtime env for `extension_id`.
///
/// Returns sorted `(key, value)` pairs, or an empty vec when nothing is
/// persisted (or the document is unreadable/malformed — replay must never hard
/// fail capability execution).
pub(crate) fn load(extension_id: &str) -> Vec<(String, String)> {
    let Ok(path) = setup_env_path(extension_id) else {
        return Vec::new();
    };
    if !path.exists() {
        return Vec::new();
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(env) = value.get("env").and_then(|env| env.as_object()) else {
        return Vec::new();
    };

    let mut pairs = env
        .iter()
        .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.to_string())))
        .collect::<Vec<_>>();
    pairs.sort_by(|left, right| left.0.cmp(&right.0));
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    #[test]
    fn persist_then_load_round_trips_sorted_env() {
        // Route HOME/XDG isolation through the shared `home_lock()` so this
        // test is globally serialized against every other env-mutating test,
        // rather than racing under a private lock (#6804).
        with_isolated_home(|_| {
            persist(
                "wordpress",
                &[
                    (
                        "HOMEBOY_SAMPLE_RUNTIME_CORE_MODULE".to_string(),
                        "/runner/sample-runtime/core.mjs".to_string(),
                    ),
                    ("ANOTHER".to_string(), "value".to_string()),
                ],
            )
            .expect("persist");

            let loaded = load("wordpress");
            assert_eq!(
                loaded,
                vec![
                    ("ANOTHER".to_string(), "value".to_string()),
                    (
                        "HOMEBOY_SAMPLE_RUNTIME_CORE_MODULE".to_string(),
                        "/runner/sample-runtime/core.mjs".to_string()
                    ),
                ]
            );
        });
    }

    #[test]
    fn load_returns_empty_when_nothing_persisted() {
        with_isolated_home(|_| {
            assert!(load("never-persisted").is_empty());
        });
    }

    #[test]
    fn persist_empty_clears_previous_document() {
        with_isolated_home(|_| {
            persist("wordpress", &[("KEY".to_string(), "value".to_string())]).expect("persist");
            assert!(!load("wordpress").is_empty());

            persist("wordpress", &[]).expect("clear");
            assert!(load("wordpress").is_empty());
        });
    }

    #[test]
    fn sanitizes_extension_id_for_filesystem_safety() {
        // The sanitizer maps every filesystem-unsafe character (here `/`) to
        // `_`; `wp/sandbox` therefore becomes `wp_sandbox`. (#6705's mechanical
        // term rename left a stale expected literal here.)
        assert_eq!(sanitize_extension_id("wp/sandbox"), "wp_sandbox");
        assert_eq!(sanitize_extension_id("wordpress"), "wordpress");
    }
}
