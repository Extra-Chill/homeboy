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
use homeboy::core::scope::{Scope, ScopeKind};

pub(crate) const EXPLICIT_PASSTHROUGH_SENTINEL: &str = "__homeboy_explicit_passthrough__";

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
            PassthroughCommand::Test => &["review", "test"],
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
    let explicit_passthrough = matches!(args.get(1).map(String::as_str), Some("bench"))
        || matches!(
            (
                args.get(1).map(String::as_str),
                args.get(2).map(String::as_str)
            ),
            (Some("review"), Some("test"))
        );
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

/// Detect runner exec options that occur after an implicit remote command tail.
/// An explicit `--` makes all remaining tokens remote-owned.
pub(crate) fn runner_exec_option_boundary_error(args: &[String]) -> Option<String> {
    let exec_index = top_level_runner_exec_index(args)?;
    let flags = known_cli_flags_for_path(&["runner", "exec"])?;
    let mut runner_seen = false;
    let mut command_seen = false;
    let mut index = exec_index;

    while let Some(arg) = args.get(index) {
        if arg == "--" {
            return None;
        }

        let flag = flags.iter().find(|flag| {
            arg == &flag.flag || (flag.takes_value && arg.strip_prefix(&flag.flag) == Some("="))
        });
        if let Some(flag) = flag {
            if command_seen {
                return Some(format!(
                    "runner exec option `{}` appears after the remote command. Use `homeboy runner exec [HOMEBOY_OPTIONS] <RUNNER> -- <COMMAND>...`; place `{}` before the runner or add `--` before a remote argument with that name.",
                    flag.flag, flag.flag
                ));
            }
            if flag.takes_value && arg == &flag.flag {
                index += 1;
            }
        } else if runner_seen {
            command_seen = true;
        } else {
            runner_seen = true;
        }
        index += 1;
    }

    None
}

fn top_level_runner_exec_index(args: &[String]) -> Option<usize> {
    let root = Cli::command();
    let flags = command_flag_specs(&root);
    let mut index = 1;

    while let Some(arg) = args.get(index) {
        if arg == "--" {
            return None;
        }
        if arg == "runner" && args.get(index + 1).is_some_and(|next| next == "exec") {
            return Some(index + 2);
        }

        let flag = flags.iter().find(|flag| {
            arg == &flag.flag || (flag.takes_value && arg.strip_prefix(&flag.flag) == Some("="))
        });
        let Some(flag) = flag else {
            return None;
        };
        index += usize::from(flag.takes_value && arg == &flag.flag) + 1;
    }

    None
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
            if let Some(aliases) = arg.get_all_aliases() {
                for alias in aliases {
                    flags.push(CliFlagSpec {
                        flag: format!("--{}", alias),
                        takes_value,
                    });
                }
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
    mark_explicit_passthrough(normalize_legacy_allow_local_fallback(
        normalize_review_audit_baseline(args),
    ))
}

/// Retain the one concrete placement alias being removed from the public
/// surface. The consolidated placement value keeps its semantics explicit.
fn normalize_legacy_allow_local_fallback(args: Vec<String>) -> Vec<String> {
    args.into_iter()
        .flat_map(|arg| {
            if arg == "--allow-local-fallback" {
                vec!["--placement".to_string(), "lab-or-local".to_string()]
            } else {
                vec![arg]
            }
        })
        .collect()
}

fn normalize_review_audit_baseline(mut args: Vec<String>) -> Vec<String> {
    if matches!(args.get(1).map(String::as_str), Some("review"))
        && matches!(args.get(2).map(String::as_str), Some("audit"))
        && matches!(args.get(3).map(String::as_str), Some("baseline"))
    {
        args.splice(2..4, ["audit-baseline".to_string()]);
    }
    args
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
    use super::{normalize, runner_exec_option_boundary_error, EXPLICIT_PASSTHROUGH_SENTINEL};
    use crate::cli_surface::{Cli, Commands};
    use clap::Parser;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn release_version_show_shorthand_is_not_rewritten() {
        let input = argv(&["homeboy", "release", "version", "my-comp"]);
        let expected = input.clone();
        assert_eq!(normalize(input), expected);
    }

    #[test]
    fn legacy_allow_local_fallback_normalizes_to_placement() {
        let args = normalize(argv(&[
            "homeboy",
            "bench",
            "example",
            "--allow-local-fallback",
        ]));
        let cli = Cli::try_parse_from(args).expect("legacy fallback flag should normalize");

        assert_eq!(cli.placement, crate::cli_surface::Placement::LabOrLocal);
    }

    #[test]
    fn release_version_show_requires_canonical_subcommand() {
        let shorthand = normalize(argv(&["homeboy", "release", "version", "my-comp"]));
        assert!(Cli::try_parse_from(shorthand).is_err());

        let canonical = normalize(argv(&["homeboy", "release", "version", "show", "my-comp"]));
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
    fn review_audit_baseline_uses_hidden_parse_target() {
        let args = normalize(argv(&[
            "homeboy", "review", "audit", "baseline", "refresh", "--path", ".",
        ]));

        assert_eq!(
            args,
            argv(&[
                "homeboy",
                "review",
                "audit-baseline",
                "refresh",
                "--path",
                ".",
            ])
        );
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
    fn runner_exec_rejects_known_options_after_an_implicit_command() {
        for flag in [
            "--cwd",
            "--raw",
            "--run-id",
            "--output",
            "--notification-transport",
        ] {
            let mut args = argv(&[
                "homeboy",
                "runner",
                "exec",
                "lab",
                "cp",
                "source",
                "destination",
            ]);
            args.push(flag.to_string());
            if !matches!(flag, "--raw") {
                args.push("value".to_string());
            }

            let error = runner_exec_option_boundary_error(&args)
                .expect("misplaced runner exec option should be diagnosed");
            assert!(error.contains(flag));
            assert!(error.contains("[HOMEBOY_OPTIONS] <RUNNER> -- <COMMAND>..."));
        }
    }

    #[test]
    fn runner_exec_allows_remote_flags_after_an_explicit_separator() {
        let args = argv(&[
            "homeboy",
            "runner",
            "exec",
            "lab",
            "--",
            "cp",
            "source",
            "destination",
            "--cwd",
            "remote",
            "--raw",
            "--run-id",
            "remote-run",
        ]);

        assert_eq!(runner_exec_option_boundary_error(&args), None);
    }

    #[test]
    fn runner_exec_allows_commands_without_flags() {
        let args = argv(&[
            "homeboy",
            "runner",
            "exec",
            "lab",
            "cp",
            "source",
            "destination",
        ]);

        assert_eq!(runner_exec_option_boundary_error(&args), None);
    }

    #[test]
    fn runner_exec_does_not_inspect_a_remote_ssh_command_tail() {
        let args = argv(&[
            "homeboy",
            "ssh",
            "lab-host",
            "--",
            "runner",
            "exec",
            "lab",
            "cp",
            "source",
            "destination",
            "--cwd",
            "/tmp",
        ]);

        assert_eq!(runner_exec_option_boundary_error(&args), None);
    }

    #[test]
    fn test_owned_flag_after_component_and_explicit_passthrough_stay_distinct() {
        let input = argv(&[
            "homeboy",
            "review",
            "test",
            "my-comp",
            "--changed-since",
            "origin/main",
            "--",
            "--filter=SmokeTest",
        ]);
        let expected = argv(&[
            "homeboy",
            "review",
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
// ScopeArgs: --project + --fleet + --component + --rig + --path + --workspace
// ============================================================================

/// The shared six-way entity selector.
///
/// [`Scope`] is already the single entity primitive commands resolve against
/// (`resolve_scope_components`, `resolve_scope_component_records`), but every
/// command that let an operator *choose* an entity re-declared the same
/// six-way switch by hand — `triage` declared it twice inside one command
/// (#10312). This group owns the spelling once so a command only has to
/// flatten it and call [`ScopeArgs::resolve`].
///
/// The selectors are mutually exclusive by construction: a scope is one
/// entity, not a set. Commands that genuinely operate on several components at
/// once (`refactor --component a --component b`) are a different shape and
/// keep their own args.
///
/// Deliberately long-flag-only. Short flags are already spoken for by several
/// of the commands this group is flattened into (`deploy -c`, `refactor -c`),
/// and adding a short here would silently change what those letters mean.
#[derive(Args, Debug, Clone, Default)]
pub struct ScopeArgs {
    /// Scope: project id.
    #[arg(long, value_name = "ID", conflicts_with_all = ["fleet", "component", "rig", "path", "workspace"])]
    pub project: Option<String>,

    /// Scope: fleet id.
    #[arg(long, value_name = "ID", conflicts_with_all = ["project", "component", "rig", "path", "workspace"])]
    pub fleet: Option<String>,

    /// Scope: registered component id.
    #[arg(long, value_name = "ID", conflicts_with_all = ["project", "fleet", "rig", "path", "workspace"])]
    pub component: Option<String>,

    /// Scope: local rig id.
    #[arg(long, value_name = "ID", conflicts_with_all = ["project", "fleet", "component", "path", "workspace"])]
    pub rig: Option<String>,

    /// Scope: checkout path, bypassing the registry.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["project", "fleet", "component", "rig", "workspace"])]
    pub path: Option<String>,

    /// Scope: every configured workspace repo.
    #[arg(long, conflicts_with_all = ["project", "fleet", "component", "rig", "path"])]
    pub workspace: bool,
}

impl ScopeArgs {
    /// The explicitly selected scope, or `None` when the operator supplied no
    /// selector at all. Commands whose unscoped default is CWD discovery (or
    /// anything else that is not the workspace) branch on this.
    pub fn selection(&self) -> Option<Scope> {
        if let Some(project) = &self.project {
            return Some(Scope::Project(project.clone()));
        }
        if let Some(fleet) = &self.fleet {
            return Some(Scope::Fleet(fleet.clone()));
        }
        if let Some(component) = &self.component {
            return Some(Scope::Component(component.clone()));
        }
        if let Some(rig) = &self.rig {
            return Some(Scope::Rig(rig.clone()));
        }
        if let Some(path) = &self.path {
            return Some(Scope::Path {
                path: path.clone(),
                component_id: None,
            });
        }
        if self.workspace {
            return Some(Scope::Workspace);
        }
        None
    }

    /// Resolve the selected scope, defaulting to the whole workspace.
    ///
    /// `--workspace` is therefore explicit *and* implicit: passing it is the
    /// same as passing nothing, which is what the commands using this group
    /// already documented.
    pub fn resolve(&self) -> Scope {
        self.selection().unwrap_or(Scope::Workspace)
    }

    /// True when no selector was supplied.
    pub fn is_unscoped(&self) -> bool {
        self.selection().is_none()
    }

    /// The selected scope kind, if any.
    pub fn kind(&self) -> Option<ScopeKind> {
        self.selection().map(|scope| scope.kind())
    }
}

#[cfg(test)]
mod scope_args_tests {
    use super::ScopeArgs;
    use clap::Parser;
    use homeboy::core::scope::{Scope, ScopeKind};

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        scope: ScopeArgs,
    }

    fn parse(args: &[&str]) -> ScopeArgs {
        TestCli::try_parse_from(args)
            .expect("scope selector should parse")
            .scope
    }

    #[test]
    fn each_selector_resolves_to_its_scope_variant() {
        assert_eq!(
            parse(&["scoped", "--project", "growth"]).resolve(),
            Scope::Project("growth".to_string())
        );
        assert_eq!(
            parse(&["scoped", "--fleet", "growth"]).resolve(),
            Scope::Fleet("growth".to_string())
        );
        assert_eq!(
            parse(&["scoped", "--component", "homeboy"]).resolve(),
            Scope::Component("homeboy".to_string())
        );
        assert_eq!(
            parse(&["scoped", "--rig", "studio"]).resolve(),
            Scope::Rig("studio".to_string())
        );
        assert_eq!(
            parse(&["scoped", "--path", "/src/homeboy"]).resolve(),
            Scope::Path {
                path: "/src/homeboy".to_string(),
                component_id: None,
            }
        );
        assert_eq!(
            parse(&["scoped", "--workspace"]).resolve(),
            Scope::Workspace
        );
    }

    #[test]
    fn inline_value_forms_parse_identically() {
        assert_eq!(
            parse(&["scoped", "--project=growth"]).resolve(),
            Scope::Project("growth".to_string())
        );
        assert_eq!(
            parse(&["scoped", "--path=/src/homeboy"]).resolve(),
            Scope::Path {
                path: "/src/homeboy".to_string(),
                component_id: None,
            }
        );
    }

    #[test]
    fn no_selector_is_unscoped_and_defaults_to_workspace() {
        let args = parse(&["scoped"]);
        assert!(args.is_unscoped());
        assert!(args.selection().is_none());
        assert!(args.kind().is_none());
        assert_eq!(args.resolve(), Scope::Workspace);
    }

    #[test]
    fn explicit_workspace_is_scoped() {
        let args = parse(&["scoped", "--workspace"]);
        assert!(!args.is_unscoped());
        assert_eq!(args.kind(), Some(ScopeKind::Workspace));
    }

    #[test]
    fn kind_matches_the_selected_variant() {
        assert_eq!(
            parse(&["scoped", "--project", "growth"]).kind(),
            Some(ScopeKind::Project)
        );
        assert_eq!(
            parse(&["scoped", "--fleet", "growth"]).kind(),
            Some(ScopeKind::Fleet)
        );
        assert_eq!(
            parse(&["scoped", "--component", "homeboy"]).kind(),
            Some(ScopeKind::Component)
        );
        assert_eq!(
            parse(&["scoped", "--rig", "studio"]).kind(),
            Some(ScopeKind::Rig)
        );
        assert_eq!(
            parse(&["scoped", "--path", "/src/homeboy"]).kind(),
            Some(ScopeKind::Path)
        );
    }

    #[test]
    fn selectors_are_mutually_exclusive() {
        let valued = ["--project", "--fleet", "--component", "--rig", "--path"];
        for (index, first) in valued.iter().enumerate() {
            for second in valued.iter().skip(index + 1) {
                let argv = vec!["scoped", first, "a", second, "b"];
                assert!(
                    TestCli::try_parse_from(&argv).is_err(),
                    "{first} and {second} should conflict"
                );
            }

            let with_workspace = vec!["scoped", first, "a", "--workspace"];
            assert!(
                TestCli::try_parse_from(&with_workspace).is_err(),
                "{first} and --workspace should conflict"
            );
        }
    }

    #[test]
    fn default_is_an_unscoped_group() {
        let args = ScopeArgs::default();
        assert!(args.is_unscoped());
        assert_eq!(args.resolve(), Scope::Workspace);
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
/// the core [`homeboy_extension::lint::LintSniffFilters`] contract —
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
    pub(crate) fn to_lint_sniff_filters(&self) -> homeboy_extension::lint::LintSniffFilters {
        homeboy_extension::lint::LintSniffFilters {
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

// ============================================================================
// DryRunArgs: --dry-run (execute by default)
// ============================================================================

#[derive(Args, Debug, Clone, Default)]
pub struct DryRunArgs {
    #[arg(long)]
    pub dry_run: bool,
}

// ============================================================================
// SettingArgs: --settings-json-file + --setting key=value + --setting-json key=<json>
// ============================================================================

use std::path::{Path, PathBuf};

/// Settings overrides flattened into every command that runs an extension
/// capability (test, bench, lint, build, validate).
///
/// Three inputs by design:
///
/// - `--settings-json-file <file>` / `--settings-profile <file>` (typed):
///   read a JSON object and apply each top-level key as a typed setting value.
///   These values sit below explicit CLI overrides.
///
/// - `--setting key=value` (string-coerced): the original "set this string
///   override" path. Values are always strings, mirroring how operators
///   typically configure settings interactively. Existing callers
///   unchanged.
///
/// - `--setting-json key=<json>` (typed): for object/array/typed-scalar
///   settings that `--setting`'s string-only coercion can't represent.
///   Required for any setting whose dispatcher consumer expects a JSON
///   object (for example an extension's `runtime_defines` and `bench_env`
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
    /// Load typed setting overrides from a JSON object file. Repeatable.
    ///
    /// Each top-level object key becomes a setting key. Values retain their
    /// JSON type. Explicit --setting and --setting-json flags override file
    /// values for the same key.
    #[arg(
        long = "settings-json-file",
        alias = "settings-profile",
        value_name = "FILE"
    )]
    pub settings_json_file: Vec<PathBuf>,

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
    ///   --setting-json runtime_defines='{"MARKDOWN_DB_MODE":"primary"}'
    ///   --setting-json my_flag=true
    #[arg(long = "setting-json", value_parser = crate::commands::parse_key_json)]
    pub setting_json: Vec<(String, serde_json::Value)>,
}

impl SettingArgs {
    pub fn settings_overrides(&self) -> homeboy::core::Result<Vec<(String, String)>> {
        Ok(self.setting.clone())
    }

    pub fn settings_profile_json_overrides(
        &self,
    ) -> homeboy::core::Result<Vec<(String, serde_json::Value)>> {
        let mut settings = Vec::new();

        for path in &self.settings_json_file {
            settings.extend(read_settings_json_file(path)?);
        }

        Ok(settings)
    }

    pub fn settings_json_overrides(
        &self,
    ) -> homeboy::core::Result<Vec<(String, serde_json::Value)>> {
        Ok(self.setting_json.clone())
    }

    pub fn has_overrides(&self) -> bool {
        !self.settings_json_file.is_empty()
            || !self.setting.is_empty()
            || !self.setting_json.is_empty()
    }
}

fn read_settings_json_file(path: &Path) -> homeboy::core::Result<Vec<(String, serde_json::Value)>> {
    let raw = std::fs::read_to_string(path).map_err(|error| {
        homeboy::core::Error::validation_invalid_argument(
            "settings-json-file",
            format!("failed to read {}: {error}", path.display()),
            Some(path.display().to_string()),
            None,
        )
    })?;

    let value: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
        homeboy::core::Error::validation_invalid_json(
            error,
            Some(format!("settings JSON file {}", path.display())),
            Some(raw.clone()),
        )
    })?;

    let serde_json::Value::Object(map) = value else {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "settings-json-file",
            format!("{} must contain a JSON object", path.display()),
            Some(path.display().to_string()),
            None,
        ));
    };

    Ok(map.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::SettingArgs;

    #[test]
    fn settings_json_file_loads_typed_profile_values() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"mode":"file","retries":2,"env":{"A":"1"},"flag":true}"#,
        )
        .expect("write profile");

        let args = SettingArgs {
            settings_json_file: vec![path],
            setting: vec![("mode".to_string(), "cli-string".to_string())],
            setting_json: vec![("mode".to_string(), serde_json::json!("cli-json"))],
        };

        assert_eq!(
            args.settings_overrides().expect("settings"),
            vec![("mode".to_string(), "cli-string".to_string())]
        );

        let json = args
            .settings_profile_json_overrides()
            .expect("json settings");
        assert_eq!(json.len(), 4);
        assert_eq!(
            json[0],
            ("env".to_string(), serde_json::json!({ "A": "1" }))
        );
        assert_eq!(json[1], ("flag".to_string(), serde_json::json!(true)));
        assert_eq!(json[2], ("mode".to_string(), serde_json::json!("file")));
        assert_eq!(json[3], ("retries".to_string(), serde_json::json!(2)));

        assert_eq!(
            args.settings_json_overrides()
                .expect("explicit json settings"),
            vec![("mode".to_string(), serde_json::json!("cli-json"))]
        );
    }

    #[test]
    fn settings_json_file_rejects_non_object_json() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"["not", "an", "object"]"#).expect("write profile");

        let args = SettingArgs {
            settings_json_file: vec![path],
            ..Default::default()
        };

        let error = args
            .settings_profile_json_overrides()
            .expect_err("non-object profile should fail");
        assert_eq!(
            error.code,
            homeboy::core::ErrorCode::ValidationInvalidArgument
        );
    }
}
