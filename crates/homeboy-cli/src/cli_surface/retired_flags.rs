//! Registry of removed/renamed public CLI flags kept as hidden no-ops.
//!
//! Deleting a public flag outright breaks every script, prompt, and runbook
//! written against the release that shipped it, with no diagnostic pointing
//! at the replacement (#14964). `--detach-after-handoff` was removed this way
//! when detaching became the default: none of the releases between 0.383.7
//! and its removal mentioned it, and #14914's own repro command stopped
//! parsing.
//!
//! To retire a public flag going forward:
//! 1. Add a hidden compatibility field for the old long name to the struct
//!    that used to own it (global on `Cli`, or local to the owning command's
//!    args struct), defaulting to a no-op action that does not change
//!    behavior on its own.
//! 2. Wire a deprecation warning for it — see
//!    `cli_runtime::warn_if_deprecated_flags_used` for the global case.
//! 3. Add an entry here with sample argv that reaches a real command the flag
//!    used to be valid on.
//! 4. If the field is `global = true`, also update
//!    `global_flag_surface_tests::root_global_flag_surface_is_pinned` — that
//!    test enumerates every global flag, hidden or not, and is the guard that
//!    makes silently deleting one instead of retiring it a visible diff.
//!
//! [`retired_flags_still_parse_as_hidden_no_ops`] is the generic guard this
//! inventory backs: every entry must still parse without error, for as long
//! as it stays listed here.

/// One retired public flag kept parseable for compatibility.
pub(crate) struct RetiredFlag {
    /// The removed/renamed long flag spelling, without the leading `--`.
    pub(crate) long: &'static str,
    /// The clap `id` assigned to the hidden compatibility field that now
    /// owns this spelling, e.g. `#[arg(long = "...", id = "...")]`. Used to
    /// read the flag back off raw `ArgMatches` for the deprecation warning in
    /// `cli_runtime::warn_if_deprecated_flags_used`.
    pub(crate) id: &'static str,
    /// Argv that reaches a real command the flag used to be valid on, without
    /// the program name or the retired flag itself. Only read by the
    /// still-parses contract test below; a non-test build never constructs
    /// the argv it describes.
    #[allow(dead_code)]
    pub(crate) sample_argv: &'static [&'static str],
}

pub(crate) const RETIRED_FLAGS: &[RetiredFlag] = &[RetiredFlag {
    long: "detach-after-handoff",
    id: "deprecated_detach_after_handoff",
    // A real, previewable agent-task cook invocation: --preview inspects
    // inferred inputs without side effects, so this stays a pure parse check.
    sample_argv: &[
        "agent-task",
        "cook",
        "--repo",
        "homeboy",
        "--task-url",
        "https://example.com/issues/14964",
        "--prompt",
        "retired flag contract test",
        "--preview",
    ],
}];

#[cfg(test)]
mod tests {
    use super::RETIRED_FLAGS;
    use crate::cli_surface::Cli;
    use clap::{CommandFactory, Parser};

    /// #14964's contract test: a flag listed here must still parse, hidden
    /// and inert, on the command it used to be valid on. This is what keeps
    /// removing a flag from the registry (rather than from the entire CLI) a
    /// deliberate, reviewable act instead of a silent break.
    #[test]
    fn retired_flags_still_parse_as_hidden_no_ops() {
        for flag in RETIRED_FLAGS {
            let long_flag = format!("--{}", flag.long);
            let mut argv: Vec<&str> = vec!["homeboy", &long_flag];
            argv.extend_from_slice(flag.sample_argv);

            let result = Cli::try_parse_from(&argv);
            assert!(
                result.is_ok(),
                "retired flag --{} should still parse as a hidden no-op: {:?}",
                flag.long,
                result.err()
            );
        }
    }

    /// The retired spelling must not resurface in `--help`/`--help-full`: a
    /// flag is retired precisely so it stops being advertised, not just
    /// renamed back into visibility.
    #[test]
    fn retired_flags_stay_hidden_from_the_built_command_tree() {
        for flag in RETIRED_FLAGS {
            let hidden = Cli::command()
                .get_arguments()
                .find(|arg| arg.get_long() == Some(flag.long))
                .is_some_and(|arg| arg.is_hide_set());
            assert!(
                hidden,
                "retired flag --{} must be declared with hide = true",
                flag.long
            );
        }
    }
}
