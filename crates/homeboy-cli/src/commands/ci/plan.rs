//! Core CI command-plan orchestration.
//!
//! `homeboy-action` historically owned command inference, the quality-vs-ops
//! split, canonical ordering, refactor-only detection, the `HOMEBOY_SKIP_LINT`
//! decision, and per-command output filenames — all assembled by shell string
//! builders (`scripts/core/resolve-commands.sh`, `scripts/core/lib.sh`). Those
//! behaviors are reusable outside the GitHub Action, so this module promotes
//! them into core as pure, agnostic logic.
//!
//! Everything here is deterministic and side-effect free: it turns a raw,
//! comma-separated command request plus an event context into a structured
//! [`CiPlan`]. The action (or any other CI runner) consumes the plan instead of
//! re-deriving ordering and naming rules in shell.

use serde::Serialize;

/// Event context a CI run is reacting to. Mirrors the action's `SCOPE_CONTEXT`
/// (`pr | push | cron | manual`) but stays runner-agnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CiEventContext {
    Pr,
    Push,
    Cron,
    Manual,
}

impl CiEventContext {
    /// Parse a context label, defaulting unknown values to `Manual` to match
    /// the action's permissive `*) audit,lint,test` fallthrough.
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "pr" => CiEventContext::Pr,
            "push" => CiEventContext::Push,
            "cron" => CiEventContext::Cron,
            _ => CiEventContext::Manual,
        }
    }

    /// Commands inferred for this context when no explicit request is given.
    /// Operations commands (fleet/deploy) are never auto-inferred.
    fn default_commands(self) -> &'static [&'static str] {
        match self {
            CiEventContext::Cron => &["release"],
            // pr / push / manual all default to the quality gates.
            _ => &["audit", "lint", "test"],
        }
    }
}

/// Whether a command is a reusable quality gate or a remote operations command.
///
/// Operations commands (fleet/deploy) are passthrough — they target remote
/// servers via SSH, use their own argument structure, and never take the
/// component/workspace/scope flags the quality loop applies. They are also
/// never auto-inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandCategory {
    Quality,
    Operations,
}

/// Canonical rank for the quality gates. Lower runs first: audit → lint → test
/// → refactor. Anything unranked keeps request order after the ranked gates.
fn canonical_rank(base: &str) -> Option<u8> {
    match base {
        "audit" => Some(0),
        "lint" => Some(1),
        "test" => Some(2),
        "refactor" => Some(3),
        _ => None,
    }
}

/// The leading token of a command request (`refactor --from all` -> `refactor`).
fn base_command(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or("")
}

fn categorize(base: &str) -> CommandCategory {
    match base {
        "fleet" | "deploy" => CommandCategory::Operations,
        _ => CommandCategory::Quality,
    }
}

/// A single planned command with its derived, core-owned metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedCommand {
    /// The full command request as the runner should invoke it
    /// (e.g. `audit`, `refactor --from all`).
    pub command: String,
    /// Leading token used for routing decisions (e.g. `refactor`).
    pub base: String,
    pub category: CommandCategory,
    /// Stable, filesystem-safe stem for this command's structured output and
    /// log files (e.g. `refactor---from-all`). Owned by core so the action
    /// stops deriving filenames with `sed`.
    pub output_stem: String,
    /// When true, the runner should export `HOMEBOY_SKIP_LINT=1` for this
    /// command. Only `test` skips lint, and only when a standalone `lint`
    /// command already runs in the same plan (avoids double-linting).
    pub skip_lint: bool,
}

/// The full structured plan a CI runner executes. Replaces the action's
/// `RESOLVED_COMMANDS` / `OPERATIONS_COMMANDS` / `refactor-only` env exports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CiPlan {
    /// Whether the commands were inferred from context or taken from an
    /// explicit request.
    pub inferred: bool,
    pub context: CiEventContext,
    /// Quality gates in canonical order (audit → lint → test → refactor → …).
    pub quality: Vec<PlannedCommand>,
    /// Operations commands (fleet/deploy) in request order.
    pub operations: Vec<PlannedCommand>,
    /// True when every quality command is a `refactor` variant. The action uses
    /// this to skip the circular post-autofix rerun (the autofix IS the
    /// refactor, so rerunning it is pointless).
    pub refactor_only: bool,
    /// True when a standalone `lint` quality command is present.
    pub has_lint: bool,
}

impl CiPlan {
    /// Comma-separated quality command list, in canonical order.
    pub fn quality_commands(&self) -> String {
        join_commands(&self.quality)
    }

    /// Comma-separated operations command list, in request order.
    pub fn operations_commands(&self) -> String {
        join_commands(&self.operations)
    }
}

fn join_commands(commands: &[PlannedCommand]) -> String {
    commands
        .iter()
        .map(|c| c.command.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// Filesystem-safe output stem for a command request.
///
/// Collapses any run of non-`[alnum]._-` characters to a single `-`, trims
/// leading/trailing `-`, and falls back to a stable default when empty. Matches
/// the action's `command_output_stem` so existing artifact names stay stable.
pub fn output_stem(command: &str) -> String {
    let mut stem = String::with_capacity(command.len());
    let mut last_was_sep = false;
    for ch in command.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            stem.push(ch);
            last_was_sep = false;
        } else if !last_was_sep {
            stem.push('-');
            last_was_sep = true;
        }
    }
    let trimmed = stem.trim_matches('-');
    if trimmed.is_empty() {
        "homeboy-output".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Split a raw, comma-separated request into trimmed, non-empty command tokens.
fn split_request(request: &str) -> Vec<String> {
    request
        .split(',')
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .map(|c| c.to_string())
        .collect()
}

/// Build a [`CiPlan`] from a raw command request and event context.
///
/// When `request` is empty (or whitespace), commands are inferred from
/// `context`. Quality gates are sorted into canonical order; operations
/// commands keep request order. `skip_lint`, `refactor_only`, `has_lint`, and
/// per-command output stems are all derived here so the runner never re-derives
/// them.
pub fn plan(request: &str, context: CiEventContext) -> CiPlan {
    let (raw_commands, inferred) = if request.trim().is_empty() {
        (
            context
                .default_commands()
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>(),
            true,
        )
    } else {
        (split_request(request), false)
    };

    let mut quality: Vec<PlannedCommand> = Vec::new();
    let mut operations: Vec<PlannedCommand> = Vec::new();

    for command in raw_commands {
        let base = base_command(&command).to_string();
        let category = categorize(&base);
        let planned = PlannedCommand {
            output_stem: output_stem(&command),
            base,
            category,
            command,
            // Filled in below once we know the full quality set.
            skip_lint: false,
        };
        match category {
            CommandCategory::Quality => quality.push(planned),
            CommandCategory::Operations => operations.push(planned),
        }
    }

    // Canonical order: audit → lint → test → refactor → (others, stable).
    // `sort_by_key` is stable, so unranked commands keep their request order.
    quality.sort_by_key(|c| canonical_rank(&c.base).unwrap_or(u8::MAX));

    let has_lint = quality
        .iter()
        .any(|c| c.base == "lint" && c.command == "lint");
    // Only `test` skips lint, and only when a standalone lint also runs.
    for command in quality.iter_mut() {
        command.skip_lint = has_lint && command.base == "test" && command.command == "test";
    }

    let refactor_only = !quality.is_empty() && quality.iter().all(|c| c.base == "refactor");

    CiPlan {
        inferred,
        context,
        quality,
        operations,
        refactor_only,
        has_lint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_quality_gates_for_pr_push_manual() {
        for ctx in [
            CiEventContext::Pr,
            CiEventContext::Push,
            CiEventContext::Manual,
        ] {
            let p = plan("", ctx);
            assert!(p.inferred);
            assert_eq!(p.quality_commands(), "audit,lint,test");
            assert!(p.operations.is_empty());
        }
    }

    #[test]
    fn infers_release_for_cron() {
        let p = plan("", CiEventContext::Cron);
        assert!(p.inferred);
        assert_eq!(p.quality_commands(), "release");
    }

    #[test]
    fn unknown_context_defaults_to_manual_quality_gates() {
        assert_eq!(CiEventContext::parse("weird"), CiEventContext::Manual);
        let p = plan("", CiEventContext::parse("weird"));
        assert_eq!(p.quality_commands(), "audit,lint,test");
    }

    #[test]
    fn explicit_request_is_not_inferred() {
        let p = plan("test", CiEventContext::Pr);
        assert!(!p.inferred);
        assert_eq!(p.quality_commands(), "test");
    }

    #[test]
    fn enforces_canonical_order_regardless_of_request_order() {
        let p = plan("test, refactor, audit, lint", CiEventContext::Manual);
        assert_eq!(p.quality_commands(), "audit,lint,test,refactor");
    }

    #[test]
    fn splits_operations_commands_out_of_quality() {
        let p = plan("audit, deploy, fleet, lint", CiEventContext::Manual);
        assert_eq!(p.quality_commands(), "audit,lint");
        assert_eq!(p.operations_commands(), "deploy,fleet");
        assert!(p
            .operations
            .iter()
            .all(|c| c.category == CommandCategory::Operations));
    }

    #[test]
    fn skip_lint_only_set_on_test_when_lint_present() {
        let p = plan("audit, lint, test", CiEventContext::Manual);
        let test = p.quality.iter().find(|c| c.command == "test").unwrap();
        assert!(test.skip_lint);
        let audit = p.quality.iter().find(|c| c.command == "audit").unwrap();
        assert!(!audit.skip_lint);
    }

    #[test]
    fn no_skip_lint_when_lint_absent() {
        let p = plan("audit, test", CiEventContext::Manual);
        let test = p.quality.iter().find(|c| c.command == "test").unwrap();
        assert!(!test.skip_lint);
        assert!(!p.has_lint);
    }

    #[test]
    fn refactor_only_detects_pure_refactor_sets() {
        let p = plan("refactor --from all", CiEventContext::Manual);
        assert!(p.refactor_only);
        assert_eq!(p.quality[0].base, "refactor");
        assert_eq!(p.quality[0].command, "refactor --from all");

        let mixed = plan("audit, refactor", CiEventContext::Manual);
        assert!(!mixed.refactor_only);
    }

    #[test]
    fn refactor_only_false_when_no_quality_commands() {
        let p = plan("deploy", CiEventContext::Manual);
        assert!(!p.refactor_only);
        assert!(p.quality.is_empty());
    }

    #[test]
    fn output_stem_sanitizes_and_falls_back() {
        assert_eq!(output_stem("audit"), "audit");
        assert_eq!(output_stem("refactor --from all"), "refactor---from-all");
        assert_eq!(output_stem("   "), "homeboy-output");
        assert_eq!(output_stem("///"), "homeboy-output");
        assert_eq!(output_stem("-lint-"), "lint");
    }

    #[test]
    fn planned_command_carries_base_and_stem() {
        let p = plan("refactor --from all", CiEventContext::Manual);
        let cmd = &p.quality[0];
        assert_eq!(cmd.base, "refactor");
        assert_eq!(cmd.output_stem, "refactor---from-all");
        assert_eq!(cmd.category, CommandCategory::Quality);
    }

    #[test]
    fn empty_request_with_whitespace_infers() {
        let p = plan("   ", CiEventContext::Pr);
        assert!(p.inferred);
    }
}
