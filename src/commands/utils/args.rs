//! Shared CLI argument groups for composable command definitions.
//!
//! Commands compose these via `#[command(flatten)]` instead of
//! redeclaring the same flags independently. Each group owns its
//! resolution/apply logic so behavior lives with the args.
//!
//! See: https://github.com/Extra-Chill/homeboy/issues/436

use clap::{Arg, ArgAction, Args, Command, CommandFactory};

use crate::cli_surface::Cli;
use homeboy::core::component::{self, Component};

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

/// Mark explicit passthrough arguments so Homeboy-owned flag filtering preserves them.
pub(crate) fn mark_explicit_passthrough(args: Vec<String>) -> Vec<String> {
    let explicit_passthrough = matches!(args.get(1).map(String::as_str), Some("test" | "bench"));
    if !explicit_passthrough {
        return args;
    }

    let mut result = Vec::new();
    for arg in args.iter() {
        if arg == "--" {
            result.push(arg.clone());
            result.push(EXPLICIT_PASSTHROUGH_SENTINEL.to_string());
            continue;
        }
        result.push(arg.clone());
    }
    result
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
    mark_explicit_passthrough(args)
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
    use super::{normalize, EXPLICIT_PASSTHROUGH_SENTINEL};
    use crate::cli_surface::{Cli, Commands};
    use clap::Parser;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn version_show_shorthand_is_not_rewritten() {
        let input = argv(&["homeboy", "version", "my-comp"]);
        let expected = input.clone();
        assert_eq!(normalize(input), expected);
    }

    #[test]
    fn version_show_requires_canonical_subcommand() {
        let shorthand = normalize(argv(&["homeboy", "version", "my-comp"]));
        assert!(Cli::try_parse_from(shorthand).is_err());

        let canonical = normalize(argv(&["homeboy", "version", "show", "my-comp"]));
        assert!(Cli::try_parse_from(canonical).is_ok());
    }

    #[test]
    fn trace_compare_variant_scenario_flag_is_not_rewritten() {
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
        let expected = input.clone();
        assert_eq!(normalize(input), expected);
    }

    #[test]
    fn trace_compare_variant_scenario_flag_remains_canonical() {
        let args = normalize(argv(&[
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
        ]));

        assert!(Cli::try_parse_from(args).is_ok());
    }

    #[test]
    fn trace_secret_env_parses_repeated_split_and_equals_args() {
        let parsed = Cli::try_parse_from(normalize(argv(&[
            "homeboy",
            "trace",
            "compare",
            "woocommerce-gateway-stripe",
            "real-wallet",
            "--secret-env",
            "STRIPE_PUBLISHABLE_KEY",
            "--secret-env=STRIPE_SECRET_KEY",
        ])))
        .expect("trace secret-env args parse");

        let Commands::Trace(args) = parsed.command else {
            panic!("expected trace command");
        };
        assert_eq!(
            args.secret_env,
            vec![
                "STRIPE_PUBLISHABLE_KEY".to_string(),
                "STRIPE_SECRET_KEY".to_string(),
            ]
        );
    }

    #[test]
    fn unknown_flag_after_bench_is_not_auto_separated() {
        let input = argv(&["homeboy", "bench", "my-comp", "--unknown-flag", "value"]);
        let expected = input.clone();
        assert_eq!(normalize(input), expected);
    }

    #[test]
    fn bench_passthrough_requires_explicit_separator() {
        let implicit = normalize(argv(&[
            "homeboy",
            "bench",
            "my-comp",
            "--unknown-flag",
            "value",
        ]));
        assert!(Cli::try_parse_from(implicit).is_err());

        let explicit = normalize(argv(&[
            "homeboy",
            "bench",
            "my-comp",
            "--",
            "--unknown-flag",
            "value",
        ]));
        assert!(Cli::try_parse_from(explicit).is_ok());
    }

    #[test]
    fn legacy_cli_aliases_are_rejected() {
        let cases = [
            &["homeboy", "components", "list"][..],
            &["homeboy", "component", "edit", "example", "--json", "{}"][..],
            &["homeboy", "component", "merge", "example", "--json", "{}"][..],
            &[
                "homeboy",
                "extension",
                "install",
                "https://example.test/ext.git",
                "--revision",
                "main",
            ][..],
            &[
                "homeboy",
                "refactor",
                "propagate",
                "--struct",
                "FileFingerprint",
            ][..],
            &[
                "homeboy",
                "trace",
                "overlay-locks",
                "cleanup",
                "--stale",
                "--force-stale-lock-cleanup",
            ][..],
        ];

        for case in cases {
            assert!(
                Cli::try_parse_from(normalize(argv(case))).is_err(),
                "legacy alias should be rejected: {case:?}"
            );
        }
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
        assert_eq!(normalize(input), expected);
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
        assert_eq!(normalize(input), expected);
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
// LintSniffArgs: --errors-only + --sniffs + --exclude-sniffs
// ============================================================================

/// Sniff-selection flags flattened into the lint command.
///
/// The `[errors_only, sniffs, exclude_sniffs]` triplet used to be re-declared
/// field-by-field on `LintArgs` (CLI), `LintRunWorkflowArgs` (workflow), and
/// `LintSourceOptions` (refactor). Owning the group here — and mapping it to
/// the core [`homeboy::core::extension::lint::LintSniffFilters`] contract —
/// keeps the shape defined once instead of being repeated across layers (#5576).
#[derive(Args, Debug, Clone, Default)]
pub struct LintSniffArgs {
    /// Show only errors, suppress warnings
    #[arg(long)]
    pub errors_only: bool,

    /// Only check specific sniffs (comma-separated codes)
    #[arg(long)]
    pub sniffs: Option<String>,

    /// Exclude sniffs from checking (comma-separated codes)
    #[arg(long)]
    pub exclude_sniffs: Option<String>,
}

impl LintSniffArgs {
    /// Project the CLI flags onto the shared core sniff-filter contract.
    pub(crate) fn to_lint_sniff_filters(&self) -> homeboy::core::extension::lint::LintSniffFilters {
        homeboy::core::extension::lint::LintSniffFilters {
            errors_only: self.errors_only,
            sniffs: self.sniffs.clone(),
            exclude_sniffs: self.exclude_sniffs.clone(),
        }
    }
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
    /// String setting override. Repeatable.
    ///
    /// Format: `--setting key=value`. Use dotted keys such as
    /// `--setting bench_env.FOO=bar` to merge string fields into object
    /// settings. Use `--setting-json bench_env='{"FOO":"bar"}'` when an
    /// entire object, array, or typed scalar is needed.
    #[arg(long, value_name = "KEY=VALUE", value_parser = crate::commands::parse_key_val)]
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
