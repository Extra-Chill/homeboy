//! Tiny argv inspection helpers shared by Lab offload submodules.
//!
//! These are intentionally minimal so that `secrets`, `agent_task_bridge`, and
//! `offload` can answer "is subcommand X present?" or "is this arg
//! placeholder-empty?" without each module redefining the same parser.

pub(super) fn subcommand_index(args: &[String], subcommand: &str) -> Option<usize> {
    args.iter().position(|arg| arg == subcommand)
}

pub(super) fn non_empty_arg(value: &str) -> Option<String> {
    (!value.trim().is_empty() && !value.starts_with('-')).then(|| value.to_string())
}
