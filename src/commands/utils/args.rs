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

/// Normalize version command arguments.
pub(crate) fn normalize_version_show(args: Vec<String>) -> Vec<String> {
    if args.len() < 3 {
        return args;
    }

    let is_version_cmd = args.get(1).map(|s| s == "version").unwrap_or(false);
    if !is_version_cmd {
        return args;
    }

    let known_subcommands = ["show", "bump", "--help", "-h", "help"];
    let second_arg = args.get(2).map(|s| s.as_str()).unwrap_or("");

    if known_subcommands.contains(&second_arg) || second_arg.starts_with('-') {
        return args;
    }

    let mut result = Vec::with_capacity(args.len() + 1);
    result.push(args[0].clone());
    result.push(args[1].clone());
    result.push("show".to_string());
    result.extend(args[2..].iter().cloned());

    result
}

/// Flags declared on the top-level `Cli` struct as `#[arg(global = true)]`.
///
/// These can appear at any position in the argv (clap routes them to the
/// parent struct regardless), so the trailing-flag normalizer below must
/// treat them as known — otherwise it inserts a `--` separator before them
/// and the value gets eaten by a subcommand's `last = true` capture
/// instead of binding to `Cli::output`. The bug surfaced as homeboy#1532
/// for `--output`.
///
/// Adding a new global flag to `Cli` requires adding the long form here
/// and the equals-form lookup happens automatically.
const GLOBAL_FLAGS: &[&str] = &["--output", "-h", "--help"];

/// Auto-insert '--' separator before unknown flags for trailing_var_arg commands.
pub(crate) fn normalize_trailing_flags(args: Vec<String>) -> Vec<String> {
    let commands: &[(&str, &str, &[&str])] = &[
        (
            "component",
            "set",
            &[
                "--json",
                "--base64",
                "--replace",
                "--version-target",
                "--extension",
                "--help",
                "-h",
            ],
        ),
        (
            "component",
            "edit",
            &["--json", "--base64", "--replace", "--help", "-h"],
        ),
        (
            "component",
            "merge",
            &[
                "--json",
                "--base64",
                "--replace",
                "--version-target",
                "--extension",
                "--help",
                "-h",
            ],
        ),
        (
            "server",
            "set",
            &["--json", "--base64", "--replace", "--help", "-h"],
        ),
        (
            "server",
            "edit",
            &["--json", "--base64", "--replace", "--help", "-h"],
        ),
        (
            "server",
            "merge",
            &["--json", "--base64", "--replace", "--help", "-h"],
        ),
        (
            "fleet",
            "set",
            &["--json", "--base64", "--replace", "--help", "-h"],
        ),
        (
            "fleet",
            "edit",
            &["--json", "--base64", "--replace", "--help", "-h"],
        ),
        (
            "fleet",
            "merge",
            &["--json", "--base64", "--replace", "--help", "-h"],
        ),
        (
            "test",
            "",
            &[
                "--skip-lint",
                "--coverage",
                "--coverage-min",
                "--baseline",
                "--ignore-baseline",
                "--ratchet",
                "--analyze",
                "--drift",
                "--write",
                "--since",
                "--changed-since",
                "--setting",
                "--path",
                "--json-summary",
                "--json",
                "--help",
                "-h",
            ],
        ),
        (
            "bench",
            "",
            &[
                "--iterations",
                "--baseline",
                "--ignore-baseline",
                "--ratchet",
                "--regression-threshold",
                "--setting",
                "--path",
                "--json-summary",
                "--json",
                "--help",
                "-h",
            ],
        ),
        (
            "scaffold",
            "test",
            &["--file", "--write", "--path", "--json", "--help", "-h"],
        ),
        (
            "docs",
            "audit",
            &[
                "--path",
                "--docs-dir",
                "--baseline",
                "--ignore-baseline",
                "--features",
                "--help",
                "-h",
            ],
        ),
        (
            "lint",
            "",
            &[
                "--baseline",
                "--ignore-baseline",
                "--summary",
                "--file",
                "--glob",
                "--changed-only",
                "--changed-since",
                "--errors-only",
                "--sniffs",
                "--exclude-sniffs",
                "--category",
                "--fix",
                "--setting",
                "--path",
                "--json",
                "--help",
                "-h",
            ],
        ),
    ];

    let known_flags = commands.iter().find_map(|(cmd, subcmd, flags)| {
        let matches = if subcmd.is_empty() {
            args.get(1).map(|s| s == *cmd).unwrap_or(false)
        } else {
            args.get(1).map(|s| s == *cmd).unwrap_or(false)
                && args.get(2).map(|s| s == *subcmd).unwrap_or(false)
        };
        if matches {
            Some(*flags)
        } else {
            None
        }
    });

    let Some(known_flags) = known_flags else {
        return args;
    };

    let mut result = Vec::new();
    let mut found_separator = false;
    let mut insert_position: Option<usize> = None;

    // A flag is "known" if the per-subcommand list owns it, OR if it's a
    // top-level Cli global (which clap routes to the parent struct
    // regardless of position). Without the global merge, `--output PATH`
    // placed after the subcommand triggers the `--` insertion and gets
    // eaten by the subcommand's `last = true` capture (homeboy#1532).
    let is_known = |flag: &str| -> bool {
        if known_flags.contains(&flag)
            || known_flags
                .iter()
                .any(|f| flag.starts_with(&format!("{}=", f)))
        {
            return true;
        }
        if GLOBAL_FLAGS.contains(&flag) {
            return true;
        }
        GLOBAL_FLAGS
            .iter()
            .any(|f| flag.starts_with(&format!("{}=", f)))
    };

    for (i, arg) in args.iter().enumerate() {
        if arg == "--" {
            found_separator = true;
        }
        if !found_separator
            && arg.starts_with("--")
            && !is_known(arg.as_str())
            && insert_position.is_none()
        {
            insert_position = Some(i);
        }
        result.push(arg.clone());
    }

    if let Some(pos) = insert_position {
        result.insert(pos, "--".to_string());
    }

    result
}

/// Apply all argument normalizations in sequence.
pub fn normalize(args: Vec<String>) -> Vec<String> {
    let args = normalize_version_show(args);
    normalize_trailing_flags(args)
}

// ============================================================================
// ComponentArgs: --component + --path + resolve()
// ============================================================================

#[derive(Args, Debug, Clone, Default)]
pub struct ComponentArgs {
    #[arg(short, long)]
    pub component: Option<String>,

    #[arg(long)]
    pub path: Option<String>,
}

#[allow(dead_code)]
impl ComponentArgs {
    pub fn resolve(&self) -> homeboy::Result<Component> {
        component::resolve_effective(self.component.as_deref(), self.path.as_deref(), None)
    }

    pub fn resolve_root(&self) -> homeboy::Result<PathBuf> {
        if let Some(ref p) = self.path {
            Ok(PathBuf::from(p))
        } else {
            let comp = component::resolve(self.component.as_deref())?;
            component::validate_local_path(&comp)
        }
    }

    pub fn load(&self) -> homeboy::Result<Component> {
        let id = self.component.as_deref().ok_or_else(|| {
            homeboy::Error::validation_missing_argument(vec!["component".to_string()])
        })?;
        component::resolve_effective(Some(id), self.path.as_deref(), None)
    }
}

// ============================================================================
// PositionalComponentArgs: positional component + --path
// ============================================================================

#[derive(Args, Debug, Clone)]
pub struct PositionalComponentArgs {
    /// Component ID (optional — auto-detected from CWD if omitted)
    pub component: Option<String>,

    #[arg(long)]
    pub path: Option<String>,
}

#[allow(dead_code)]
impl PositionalComponentArgs {
    pub fn load(&self) -> homeboy::Result<Component> {
        component::resolve_effective(self.component.as_deref(), self.path.as_deref(), None)
    }

    pub fn id(&self) -> Option<&str> {
        self.component.as_deref()
    }

    /// Resolve the component ID, falling back to CWD auto-discovery.
    /// Returns the effective component ID string for display/logging.
    pub fn resolve_id(&self) -> homeboy::Result<String> {
        if let Some(ref id) = self.component {
            return Ok(id.clone());
        }
        let component = self.load()?;
        Ok(component.id)
    }
}

#[cfg(test)]
mod positional_tests {
    use super::*;

    #[test]
    fn load_uses_path_when_component_missing() {
        let args = PositionalComponentArgs {
            component: Some("missing-component".to_string()),
            path: Some("/tmp/homeboy-missing-component".to_string()),
        };

        let loaded = args
            .load()
            .expect("path-based synthetic component should load");

        assert_eq!(loaded.id, "missing-component");
        assert_eq!(loaded.local_path, "/tmp/homeboy-missing-component");
        assert_eq!(loaded.remote_path, "");
    }

    #[test]
    fn id_returns_none_when_omitted() {
        let args = PositionalComponentArgs {
            component: None,
            path: None,
        };
        assert!(args.id().is_none());
    }

    #[test]
    fn id_returns_some_when_provided() {
        let args = PositionalComponentArgs {
            component: Some("my-comp".to_string()),
            path: None,
        };
        assert_eq!(args.id(), Some("my-comp"));
    }
}

#[cfg(test)]
mod normalize_tests {
    use super::normalize_trailing_flags;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    /// `--output` placed AFTER a `last = true` subcommand must NOT trigger
    /// the `--` separator insertion — it's a `global = true` flag on the
    /// top-level Cli struct and clap routes it there directly. Inserting
    /// `--` would cause the subcommand's trailing-arg capture to swallow
    /// the value (homeboy#1532).
    #[test]
    fn output_after_subcommand_is_not_separated() {
        let input = argv(&[
            "homeboy",
            "bench",
            "my-comp",
            "--output",
            "/tmp/x.json",
            "--iterations",
            "1",
        ]);
        let expected = input.clone();
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    /// Equals form must round-trip the same way.
    #[test]
    fn output_equals_form_after_subcommand_is_not_separated() {
        let input = argv(&[
            "homeboy",
            "bench",
            "my-comp",
            "--output=/tmp/x.json",
            "--iterations",
            "1",
        ]);
        let expected = input.clone();
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    /// Mirror the bench check for `test` (the other `last = true`
    /// subcommand). Both consumers must accept post-position `--output`.
    #[test]
    fn output_after_test_subcommand_is_not_separated() {
        let input = argv(&[
            "homeboy",
            "test",
            "my-comp",
            "--output",
            "/tmp/y.json",
            "--ratchet",
        ]);
        let expected = input.clone();
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    /// A genuinely unknown flag (not on the per-subcommand allow-list,
    /// not a Cli global) STILL triggers the `--` insertion. This is the
    /// existing trailing-arg passthrough behaviour the normalizer was
    /// designed for; the fix must not regress it.
    #[test]
    fn unknown_flag_after_bench_still_separated() {
        let input = argv(&["homeboy", "bench", "my-comp", "--unknown-flag", "value"]);
        let expected = argv(&[
            "homeboy",
            "bench",
            "my-comp",
            "--",
            "--unknown-flag",
            "value",
        ]);
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    /// `--output` mixed with an unknown flag: separator goes before the
    /// unknown flag, NOT before `--output`. Captures the most realistic
    /// repro from the MDI bench cook (driver script with `--output` plus
    /// dispatcher passthrough flags).
    #[test]
    fn output_plus_unknown_flag_separator_before_unknown_only() {
        let input = argv(&[
            "homeboy",
            "bench",
            "my-comp",
            "--output",
            "/tmp/x.json",
            "--unknown",
            "v",
        ]);
        let expected = argv(&[
            "homeboy",
            "bench",
            "my-comp",
            "--output",
            "/tmp/x.json",
            "--",
            "--unknown",
            "v",
        ]);
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    /// Pre-subcommand position must continue to work — that's the path
    /// the existing tests + production users have relied on.
    #[test]
    fn output_before_subcommand_is_unchanged() {
        let input = argv(&[
            "homeboy",
            "--output",
            "/tmp/x.json",
            "bench",
            "my-comp",
            "--iterations",
            "1",
        ]);
        let expected = input.clone();
        assert_eq!(normalize_trailing_flags(input), expected);
    }
}

// ============================================================================
// BaselineArgs: --baseline + --ignore-baseline + --ratchet
// ============================================================================

/// Shared baseline-lifecycle flags flattened into every command that
/// participates in the baseline engine (audit, lint, test, bench).
///
/// Historically these lived as separate fields on each command's CLI args
/// struct; merging them into one group removes the duplicated
/// `[baseline, ignore_baseline]` and `[json_summary, ratchet]` field
/// patterns the audit detector flags (#1483). Lint has no ratchet semantics
/// today — it simply leaves `ratchet` at the default.
#[derive(Args, Debug, Clone, Default)]
pub struct BaselineArgs {
    /// Persist the current run as the new baseline.
    #[arg(long)]
    pub baseline: bool,

    /// Skip baseline comparison for this run.
    #[arg(long)]
    pub ignore_baseline: bool,

    /// Auto-update the baseline when the current run improves on it.
    #[arg(long)]
    pub ratchet: bool,
}

// ============================================================================
// WriteModeArgs: --write (dry-run by default)
// ============================================================================

#[derive(Args, Debug, Clone, Default)]
pub struct WriteModeArgs {
    #[arg(long)]
    pub write: bool,
}

#[allow(dead_code)]
impl WriteModeArgs {
    pub(crate) fn is_dry_run(&self) -> bool {
        !self.write
    }
}

// ============================================================================
// DryRunArgs: --dry-run (execute by default)
// ============================================================================

#[derive(Args, Debug, Clone, Default)]
pub struct DryRunArgs {
    #[arg(long)]
    pub dry_run: bool,
}

// ============================================================================
// HiddenJsonArgs: --json (hidden compatibility flag)
// ============================================================================

#[derive(Args, Debug, Clone, Default)]
pub struct HiddenJsonArgs {
    #[arg(long, hide = true)]
    pub json: bool,
}

// ============================================================================
// SettingArgs: --setting key=value pairs
// ============================================================================

#[derive(Args, Debug, Clone, Default)]
pub struct SettingArgs {
    #[arg(long, value_parser = crate::commands::parse_key_val)]
    pub setting: Vec<(String, String)>,
}
