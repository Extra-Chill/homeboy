//! Resolver hooks that let `core::runner` derive information from raw CLI argv
//! without depending on the full CLI parser (`cli_surface::Cli` / `commands`).
//!
//! The CLI layer owns argument parsing. Rather than have core call
//! `Cli::try_parse_from` directly (which would make core depend on `commands`
//! and block extracting the CLI into its own crate), the CLI layer registers
//! these resolvers at startup and core invokes them through the hook.

use crate::core::agent_task_dispatch_service::AgentTaskDispatchCommand;
use std::sync::{OnceLock, RwLock};

/// Resolve a dispatched command's argv to its hot-command label (e.g. `bench`,
/// `lint`), if the argv parses to a routable command. Registered by the CLI
/// layer via [`set_command_label_resolver`].
type CommandLabelResolver = fn(&[String]) -> Option<String>;

fn command_label_resolver() -> &'static RwLock<Option<CommandLabelResolver>> {
    static RESOLVER: OnceLock<RwLock<Option<CommandLabelResolver>>> = OnceLock::new();
    RESOLVER.get_or_init(|| RwLock::new(None))
}

/// Register the resolver that maps dispatched argv to a hot-command label.
/// Called once during startup by the CLI layer.
pub fn set_command_label_resolver(resolver: CommandLabelResolver) {
    let mut guard = command_label_resolver()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(resolver);
}

/// Resolve a hot-command label for the given argv via the registered resolver.
/// Returns `None` if no resolver is registered or the argv does not map to a
/// routable command.
pub fn resolve_command_label(argv: &[String]) -> Option<String> {
    let resolver = {
        let guard = command_label_resolver()
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard
    };
    resolver.and_then(|f| f(argv))
}

/// Resolve an agent-task `cook` dispatch command from raw argv.
///
/// - `Err` — argv failed to parse as a homeboy CLI command.
/// - `Ok(None)` — argv parsed but is not an `agent-task cook` command (callers
///   should leave the args unchanged).
/// - `Ok(Some(_))` — the dispatch command extracted from `agent-task cook`.
///
/// Registered by the CLI layer via [`set_agent_task_dispatch_resolver`].
type AgentTaskDispatchResolver =
    fn(&[String]) -> crate::core::Result<Option<AgentTaskDispatchCommand>>;

fn agent_task_dispatch_resolver() -> &'static RwLock<Option<AgentTaskDispatchResolver>> {
    static RESOLVER: OnceLock<RwLock<Option<AgentTaskDispatchResolver>>> = OnceLock::new();
    RESOLVER.get_or_init(|| RwLock::new(None))
}

/// Register the resolver that extracts an agent-task dispatch command from argv.
/// Called once during startup by the CLI layer.
pub fn set_agent_task_dispatch_resolver(resolver: AgentTaskDispatchResolver) {
    let mut guard = agent_task_dispatch_resolver()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(resolver);
}

/// Resolve an agent-task dispatch command from argv via the registered
/// resolver. Returns `Ok(None)` when no resolver is registered.
pub fn resolve_agent_task_dispatch(
    argv: &[String],
) -> crate::core::Result<Option<AgentTaskDispatchCommand>> {
    let resolver = {
        let guard = agent_task_dispatch_resolver()
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard
    };
    match resolver {
        Some(f) => f(argv),
        None => Ok(None),
    }
}
