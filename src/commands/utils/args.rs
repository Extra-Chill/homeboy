//! Shared CLI argument groups for composable command definitions.
//!
//! Commands compose these via `#[command(flatten)]` instead of
//! redeclaring the same flags independently. Each group owns its
//! resolution/apply logic so behavior lives with the args.
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/436

use clap::{Arg, ArgAction, Args, Command, CommandFactory};
use std::path::PathBuf;

use crate::cli_surface::Cli;
use homeboy::core::component::{self, Component};

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

const EXPLICIT_PASSTHROUGH_SENTINEL: &str = "__homeboy_explicit_passthrough__";

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliFlagSpec {
    flag: String,
    takes_value: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PassthroughCommand {
    Bench,
    Test,
}

impl PassthroughCommand {
    fn path(self) -> &'static [&'static str] {
        match self {
            PassthroughCommand::Bench => &["bench"],
            PassthroughCommand::Test => &["test"],
        }
    }
}

/// Strip Homeboy-owned flags from runner passthrough args.
///
/// Clap's `last = true` capture can include flags that also parsed into named
/// Homeboy fields when those flags appear after a positional. Keeping this
/// policy next to the trailing-arg normalizer makes command-owned flags easier
/// to update without drifting separate bench/test filters.
pub(crate) fn filter_passthrough_args(command: PassthroughCommand, args: &[String]) -> Vec<String> {
    if let Some(index) = args
        .iter()
        .position(|arg| arg == EXPLICIT_PASSTHROUGH_SENTINEL)
    {
        return args[index + 1..].to_vec();
    }

    let owned_flags = known_cli_flags_for_path(command.path()).unwrap_or_default();
    let mut filtered = Vec::new();
    let mut skip_next = false;

    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }

        if owned_flags
            .iter()
            .any(|flag| !flag.takes_value && flag.flag == *arg)
        {
            continue;
        }

        let is_value_flag = owned_flags.iter().any(|flag| {
            if !flag.takes_value {
                return false;
            }
            if arg.starts_with(&format!("{}=", flag.flag)) {
                return true;
            }
            if arg == &flag.flag {
                skip_next = true;
                return true;
            }
            false
        });

        if is_value_flag {
            continue;
        }

        filtered.push(arg.clone());
    }

    filtered
}

/// Auto-insert '--' separator before unknown flags for trailing_var_arg commands.
pub(crate) fn normalize_trailing_flags(args: Vec<String>) -> Vec<String> {
    let Some(path) = trailing_normalization_path(&args) else {
        return args;
    };
    let explicit_passthrough = path == ["test"] || path == ["bench"];

    let Some(known_flags) = known_cli_flags_for_path(&path) else {
        return args;
    };

    let mut result = Vec::new();
    let mut found_separator = false;
    let mut insert_position: Option<usize> = None;

    let is_known = |flag: &str| -> bool {
        known_flags
            .iter()
            .any(|f| flag == f.flag || flag.starts_with(&format!("{}=", f.flag)))
    };

    for (i, arg) in args.iter().enumerate() {
        if arg == "--" {
            found_separator = true;
            result.push(arg.clone());
            if explicit_passthrough {
                result.push(EXPLICIT_PASSTHROUGH_SENTINEL.to_string());
            }
            continue;
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

fn trailing_normalization_path(args: &[String]) -> Option<Vec<&'static str>> {
    let path = match (
        args.get(1).map(String::as_str),
        args.get(2).map(String::as_str),
    ) {
        (Some("component"), Some("set" | "edit" | "merge")) => vec!["component", "set"],
        (Some("server"), Some("set" | "edit" | "merge")) => vec!["server", "set"],
        (Some("fleet"), Some("set" | "edit" | "merge")) => vec!["fleet", "set"],
        (Some("test"), _) => vec!["test"],
        (Some("bench"), _) => vec!["bench"],
        (Some("lint"), _) => vec!["lint"],
        _ => return None,
    };
    Some(path)
}

fn known_cli_flags_for_path(path: &[&str]) -> Option<Vec<CliFlagSpec>> {
    let root = Cli::command();
    let mut flags = command_flag_specs(&root);
    let mut command = &root;

    for segment in path {
        command = find_subcommand(command, segment)?;
        flags.extend(command_flag_specs(command));
    }

    Some(flags)
}

fn find_subcommand<'a>(command: &'a Command, name: &str) -> Option<&'a Command> {
    command
        .get_subcommands()
        .find(|subcommand| subcommand.get_name() == name)
}

fn command_flag_specs(command: &Command) -> Vec<CliFlagSpec> {
    command
        .get_arguments()
        .flat_map(|arg| {
            let takes_value = arg_takes_value(arg);
            let mut flags = Vec::new();
            if let Some(long) = arg.get_long() {
                flags.push(CliFlagSpec {
                    flag: format!("--{}", long),
                    takes_value,
                });
            }
            if let Some(short) = arg.get_short() {
                flags.push(CliFlagSpec {
                    flag: format!("-{}", short),
                    takes_value,
                });
            }
            flags
        })
        .chain([
            CliFlagSpec {
                flag: "--help".to_string(),
                takes_value: false,
            },
            CliFlagSpec {
                flag: "-h".to_string(),
                takes_value: false,
            },
        ])
        .collect()
}

fn arg_takes_value(arg: &Arg) -> bool {
    matches!(arg.get_action(), ArgAction::Set | ArgAction::Append)
}

/// Apply all argument normalizations in sequence.
pub fn normalize(args: Vec<String>) -> Vec<String> {
    let args = normalize_version_show(args);
    let args = normalize_trace_compare_variant_scenario(args);
    normalize_trailing_flags(args)
}

fn normalize_trace_compare_variant_scenario(args: Vec<String>) -> Vec<String> {
    if args.get(1).map(|arg| arg.as_str()) != Some("trace")
        || args.get(2).map(|arg| arg.as_str()) != Some("compare-variant")
    {
        return args;
    }

    let already_has_positional_scenario = args.get(3).is_some_and(|arg| !arg.starts_with('-'));
    if already_has_positional_scenario {
        return args;
    }

    let mut result = Vec::with_capacity(args.len());
    let mut scenario = None;
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--scenario" {
            if let Some(value) = args.get(index + 1) {
                scenario = Some(value.clone());
                index += 2;
                continue;
            }
        } else if let Some(value) = arg.strip_prefix("--scenario=") {
            scenario = Some(value.to_string());
            index += 1;
            continue;
        }
        result.push(arg.clone());
        index += 1;
    }

    if let Some(scenario) = scenario {
        result.insert(3, scenario);
    }
    result
}

// ============================================================================
// ComponentArgs: --component + --path + resolve()
// ============================================================================

#[derive(Args, Debug, Clone, Default)]
pub struct ComponentArgs {
    #[arg(short, long)]
    pub component: Option<String>,

    /// Override the component checkout path for this invocation
    #[arg(long)]
    pub path: Option<String>,
}

#[allow(dead_code)]
impl ComponentArgs {
    pub fn resolve(&self) -> homeboy::core::Result<Component> {
        component::resolve_effective(self.component.as_deref(), self.path.as_deref(), None)
    }

    pub fn resolve_root(&self) -> homeboy::core::Result<PathBuf> {
        if let Some(ref p) = self.path {
            Ok(PathBuf::from(p))
        } else {
            let comp = component::resolve(self.component.as_deref())?;
            component::validate_local_path(&comp)
        }
    }

    pub fn load(&self) -> homeboy::core::Result<Component> {
        let id = self.component.as_deref().ok_or_else(|| {
            homeboy::core::Error::validation_missing_argument(vec!["component".to_string()])
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

    /// Override the component checkout path for this invocation
    #[arg(long)]
    pub path: Option<String>,
}

// ============================================================================
// ExtensionOverrideArgs: one-shot extension selection
// ============================================================================

#[derive(Args, Debug, Clone, Default)]
pub struct ExtensionOverrideArgs {
    /// One-shot extension override for the current invocation
    #[arg(long = "extension", value_name = "ID")]
    pub extensions: Vec<String>,
}

#[allow(dead_code)]
impl PositionalComponentArgs {
    pub fn load(&self) -> homeboy::core::Result<Component> {
        component::resolve_effective(self.component.as_deref(), self.path.as_deref(), None)
    }

    pub fn id(&self) -> Option<&str> {
        self.component.as_deref()
    }

    /// Resolve the component ID, falling back to CWD auto-discovery.
    /// Returns the effective component ID string for display/logging.
    pub fn resolve_id(&self) -> homeboy::core::Result<String> {
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
    use super::{normalize, normalize_trailing_flags, EXPLICIT_PASSTHROUGH_SENTINEL};

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

    /// `bench` owns `--rig` and `--ignore-default-baseline`; they must
    /// stay on clap's named-argument path, not get swallowed by the
    /// trailing runner-args capture.
    #[test]
    fn bench_rig_flags_after_component_are_not_separated() {
        let input = argv(&[
            "homeboy",
            "bench",
            "my-comp",
            "--rig",
            "candidate",
            "--iterations",
            "1",
            "--ignore-default-baseline",
        ]);
        let expected = input.clone();
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    /// Rig-pinned bench commonly omits the positional component because the
    /// rig declares `bench.default_component`. Scenario/profile selectors in
    /// that shape must still bind to BenchRunArgs rather than being captured
    /// as extension passthrough args.
    #[test]
    fn bench_rig_selector_flags_without_component_are_not_separated() {
        let input = argv(&[
            "homeboy",
            "bench",
            "--rig",
            "studio-agent-sdk",
            "--scenario",
            "studio-agent-runtime",
            "--iterations",
            "1",
            "--runs",
            "1",
            "--json-summary",
            "--ignore-default-baseline",
        ]);
        let expected = input.clone();
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    #[test]
    fn bench_rig_profile_flag_without_component_is_not_separated() {
        let input = argv(&[
            "homeboy",
            "bench",
            "--rig",
            "studio-agent-sdk",
            "--profile",
            "smoke",
            "--iterations=1",
        ]);
        let expected = input.clone();
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    #[test]
    fn bench_force_hot_after_subcommand_is_not_separated() {
        let input = argv(&[
            "homeboy",
            "bench",
            "--rig",
            "studio-bfb",
            "--iterations",
            "1",
            "--force-hot",
            "--setting",
            "studio_site_build_prompt_variant=astro-docs-content-collection",
        ]);
        let expected = input.clone();
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    #[test]
    fn trace_compare_variant_scenario_flag_becomes_positional() {
        let input = argv(&[
            "homeboy",
            "trace",
            "compare-variant",
            "--rig",
            "studio",
            "--scenario",
            "studio-app-create-site",
            "--overlay",
            "overlays/change.patch",
            "--output-dir",
            ".homeboy/experiments/change",
        ]);
        let expected = argv(&[
            "homeboy",
            "trace",
            "compare-variant",
            "studio-app-create-site",
            "--rig",
            "studio",
            "--overlay",
            "overlays/change.patch",
            "--output-dir",
            ".homeboy/experiments/change",
        ]);
        assert_eq!(normalize(input), expected);
    }

    #[test]
    fn lint_json_summary_after_component_is_not_separated() {
        let input = argv(&["homeboy", "lint", "my-comp", "--json-summary"]);
        let expected = input.clone();
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    /// `bench` owns shared-state/concurrency. They must remain named CLI
    /// flags even when placed after the positional component.
    #[test]
    fn bench_shared_state_flags_after_component_are_not_separated() {
        let input = argv(&[
            "homeboy",
            "bench",
            "my-comp",
            "--shared-state",
            "/tmp/homeboy-bench",
            "--concurrency=4",
            "--rig-concurrency",
            "2",
        ]);
        let expected = input.clone();
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    /// `bench` owns run-level repetition. It must remain a named CLI flag
    /// even when placed after the positional component.
    #[test]
    fn bench_runs_flag_after_component_is_not_separated() {
        let input = argv(&[
            "homeboy",
            "bench",
            "my-comp",
            "--runs",
            "5",
            "--iterations",
            "1",
        ]);
        let expected = input.clone();
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    /// `bench` owns warmup control. It must remain a named CLI flag even
    /// when placed after the positional component so clap can reject
    /// negative values instead of passing them through to the runner.
    #[test]
    fn bench_warmup_flag_after_component_is_not_separated() {
        let input = argv(&[
            "homeboy",
            "bench",
            "my-comp",
            "--warmup",
            "-1",
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

    #[test]
    fn test_owned_flag_after_component_and_explicit_passthrough_stay_distinct() {
        let input = argv(&[
            "homeboy",
            "test",
            "my-comp",
            "--changed-since",
            "origin/main",
            "--",
            "--filter=SmokeTest",
        ]);
        let expected = argv(&[
            "homeboy",
            "test",
            "my-comp",
            "--changed-since",
            "origin/main",
            "--",
            EXPLICIT_PASSTHROUGH_SENTINEL,
            "--filter=SmokeTest",
        ]);
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    #[test]
    fn bench_owned_flag_after_component_and_explicit_passthrough_stay_distinct() {
        let input = argv(&[
            "homeboy",
            "bench",
            "my-comp",
            "--iterations",
            "1",
            "--",
            "--filter=Scenario",
        ]);
        let expected = argv(&[
            "homeboy",
            "bench",
            "my-comp",
            "--iterations",
            "1",
            "--",
            EXPLICIT_PASSTHROUGH_SENTINEL,
            "--filter=Scenario",
        ]);
        assert_eq!(normalize_trailing_flags(input), expected);
    }

    #[test]
    fn explicit_passthrough_preserves_homeboy_like_runner_flags() {
        let args = argv(&[
            EXPLICIT_PASSTHROUGH_SENTINEL,
            "--coverage",
            "--baseline",
            "runner-value",
        ]);

        assert_eq!(
            super::filter_passthrough_args(super::PassthroughCommand::Test, &args),
            argv(&["--coverage", "--baseline", "runner-value"])
        );
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
// SettingArgs: --setting key=value + --setting-json key=<json>
// ============================================================================

/// Settings overrides flattened into every command that runs an extension
/// capability (test, bench, lint, build, validate).
///
/// Two flags by design:
///
/// - `--setting key=value` (string-coerced): the original "set this string
///   override" path. Values are always strings, mirroring how operators
///   typically configure settings interactively. Existing callers
///   unchanged.
///
/// - `--setting-json key=<json>` (typed): for object/array/typed-scalar
///   settings that `--setting`'s string-only coercion can't represent.
///   Required for any setting whose dispatcher consumer expects a JSON
///   object (the wordpress extension's `wp_config_defines` and `bench_env`
///   are the motivating cases). String coercion of an object value
///   produces `"{\"key\":\"value\"}"` — a string containing JSON, not a
///   JSON object — which downstream `jq -c '.field'` extractions then
///   pass through as a string, breaking the substitution that expects an
///   object.
///
/// When both flags target the same key, `--setting-json` wins (it's
/// strictly more expressive and was specified later in the merge order).
#[derive(Args, Debug, Clone, Default)]
pub struct SettingArgs {
    #[arg(long, value_parser = crate::commands::parse_key_val)]
    pub setting: Vec<(String, String)>,

    /// Typed-JSON setting override. Repeatable.
    ///
    /// Format: `--setting-json key=<json>`, where `<json>` is any
    /// well-formed JSON value (object, array, string [must be quoted],
    /// number, boolean, null). For string values use `--setting`; this
    /// flag exists for object/array/typed-scalar settings that string
    /// coercion can't represent.
    ///
    /// Examples:
    ///
    ///   --setting-json bench_env='{"BENCH_CORPUS_SIZE":"1000"}'
    ///   --setting-json wp_config_defines='{"MARKDOWN_DB_MODE":"primary"}'
    ///   --setting-json my_flag=true
    #[arg(long = "setting-json", value_parser = crate::commands::parse_key_json)]
    pub setting_json: Vec<(String, serde_json::Value)>,
}
