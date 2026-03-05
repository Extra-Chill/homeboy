//! Shared CLI argument groups for composable command definitions.
//!
//! Commands compose these via `#[command(flatten)]` instead of
//! redeclaring the same flags independently. Each group owns its
//! resolution/apply logic so behavior lives with the args.
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/436

use clap::Args;
use std::path::PathBuf;

use homeboy::component::{self, Component};
use homeboy::error::ErrorCode;

// ============================================================================
// ComponentArgs: --component + --path + resolve()
// ============================================================================

/// Shared args for commands that operate on a single component with optional
/// path override. Replaces the repeated `--component` + `--path` pattern.
///
/// Usage in a command struct:
/// ```ignore
/// #[derive(Args)]
/// pub struct MyArgs {
///     #[command(flatten)]
///     pub component: ComponentArgs,
///     // ... command-specific args
/// }
/// ```
#[derive(Args, Debug, Clone, Default)]
pub struct ComponentArgs {
    /// Component ID (uses its local_path as the root)
    #[arg(short, long)]
    pub component: Option<String>,

    /// Directory path (alternative to --component)
    #[arg(long)]
    pub path: Option<String>,
}

#[allow(dead_code)]
impl ComponentArgs {
    /// Resolve a component, applying path override if provided.
    /// Falls back to CWD auto-discovery when both fields are None.
    pub fn resolve(&self) -> homeboy::Result<Component> {
        let mut comp = component::resolve(self.component.as_deref())?;
        if let Some(ref path) = self.path {
            comp.local_path = path.clone();
        }
        Ok(comp)
    }

    /// Resolve just the root directory path — prefers --path, falls back
    /// to component's local_path via resolve().
    pub fn resolve_root(&self) -> homeboy::Result<PathBuf> {
        if let Some(ref p) = self.path {
            Ok(PathBuf::from(p))
        } else {
            let comp = component::resolve(self.component.as_deref())?;
            component::validate_local_path(&comp)
        }
    }

    /// Load a component by ID, applying path override if provided.
    /// Unlike `resolve()`, this requires an explicit component ID
    /// (no CWD auto-discovery).
    pub fn load(&self) -> homeboy::Result<Component> {
        let id = self.component.as_deref().ok_or_else(|| {
            homeboy::Error::validation_missing_argument(vec!["component".to_string()])
        })?;
        let mut comp = component::load(id)?;
        if let Some(ref path) = self.path {
            comp.local_path = path.clone();
        }
        Ok(comp)
    }
}

// ============================================================================
// PositionalComponentArgs: positional component + --path
// ============================================================================

/// Like ComponentArgs but with the component ID as a required positional arg.
/// For commands where the component is the primary operand (test, lint, audit).
#[derive(Args, Debug, Clone)]
pub struct PositionalComponentArgs {
    /// Component ID
    pub component: String,

    /// Override local_path for this run
    #[arg(long)]
    pub path: Option<String>,
}

impl PositionalComponentArgs {
    /// Load the component, applying path override if provided.
    pub fn load(&self) -> homeboy::Result<Component> {
        if let Some(ref path) = self.path {
            match component::load(&self.component) {
                Ok(mut comp) => {
                    comp.local_path = path.clone();
                    Ok(comp)
                }
                Err(err) if matches!(err.code, ErrorCode::ComponentNotFound) => Ok(Component::new(
                    self.component.clone(),
                    path.clone(),
                    String::new(),
                    None,
                )),
                Err(err) => Err(err),
            }
        } else {
            component::load(&self.component)
        }
    }

    /// Get the component ID.
    pub fn id(&self) -> &str {
        &self.component
    }

    /// Resolve the effective source path (--path override or component's local_path).
    pub fn source_path(&self) -> homeboy::Result<PathBuf> {
        if let Some(ref path) = self.path {
            Ok(PathBuf::from(path))
        } else {
            let comp = component::load(&self.component)?;
            let expanded = shellexpand::tilde(&comp.local_path);
            Ok(PathBuf::from(expanded.as_ref()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_uses_path_when_component_missing() {
        let args = PositionalComponentArgs {
            component: "missing-component".to_string(),
            path: Some("/tmp/homeboy-missing-component".to_string()),
        };

        let loaded = args
            .load()
            .expect("path-based synthetic component should load");

        assert_eq!(loaded.id, "missing-component");
        assert_eq!(loaded.local_path, "/tmp/homeboy-missing-component");
        assert_eq!(loaded.remote_path, "");
    }
}

// ============================================================================
// BaselineArgs: --baseline + --ignore-baseline
// ============================================================================

/// Shared args for commands that support baseline save/compare lifecycle.
/// Used by audit, cleanup, test, and docs audit.
#[derive(Args, Debug, Clone, Default)]
pub struct BaselineArgs {
    /// Save current state as baseline for future comparisons
    #[arg(long)]
    pub baseline: bool,

    /// Skip baseline comparison even if a baseline exists
    #[arg(long)]
    pub ignore_baseline: bool,
}

// ============================================================================
// WriteModeArgs: --write (dry-run by default)
// ============================================================================

/// Shared args for commands that default to dry-run and require `--write`
/// to apply changes (refactor, audit fix).
#[derive(Args, Debug, Clone, Default)]
pub struct WriteModeArgs {
    /// Apply changes to disk (default is dry-run)
    #[arg(long)]
    pub write: bool,
}

#[allow(dead_code)]
impl WriteModeArgs {
    /// Whether this is a dry run (write was NOT specified).
    pub fn is_dry_run(&self) -> bool {
        !self.write
    }
}

// ============================================================================
// DryRunArgs: --dry-run (execute by default)
// ============================================================================

/// Shared args for commands that execute by default and require `--dry-run`
/// to preview (deploy, release, version bump).
#[derive(Args, Debug, Clone, Default)]
pub struct DryRunArgs {
    /// Preview what would happen without making changes
    #[arg(long)]
    pub dry_run: bool,
}

// ============================================================================
// HiddenJsonArgs: --json (hidden compatibility flag)
// ============================================================================

/// Hidden `--json` flag for backward compatibility. Output is JSON by default
/// in all commands, but some users/scripts pass `--json` explicitly.
#[derive(Args, Debug, Clone, Default)]
pub struct HiddenJsonArgs {
    /// Accept --json for compatibility (output is JSON by default)
    #[arg(long, hide = true)]
    pub json: bool,
}

// ============================================================================
// SettingArgs: --setting key=value pairs
// ============================================================================

/// Shared args for commands that accept key=value setting overrides.
/// Used by test and lint commands.
#[derive(Args, Debug, Clone, Default)]
pub struct SettingArgs {
    /// Override settings as key=value pairs
    #[arg(long, value_parser = super::parse_key_val)]
    pub setting: Vec<(String, String)>,
}
