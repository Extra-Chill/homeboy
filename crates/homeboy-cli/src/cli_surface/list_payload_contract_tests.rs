//! Classifies every `* list` subcommand by its payload shape (#14876).
//!
//! Row-shaped registry lists return their rows under `data.entities` (legacy
//! command-specific keys are only deprecated mirrors), so one reader works
//! across all of them. Every other `list` is exempt for a recorded reason.
//!
//! Walking the real clap tree means a new `list` subcommand fails here until
//! someone decides which side it belongs on, instead of silently inventing a
//! new payload key.

use std::collections::BTreeSet;

use super::Cli;
use clap::CommandFactory;

/// Row-shaped registry lists. Each returns its rows under `data.entities`
/// (always present, `[]` when empty) and has a serialization test beside its
/// handler asserting that.
const ENTITY_LIST_COMMANDS: &[&str] = &[
    "component list",
    "extension list",
    "fleet list",
    "project list",
    "rig list",
    "runner list",
    "server list",
    "stack list",
    "tunnel service list",
];

/// `list` subcommands deliberately left on their own payload shape.
const EXEMPT_LIST_COMMANDS: &[(&str, &str)] = &[
    (
        "activity list",
        "report-shaped: counts and zero_executing_work alongside items; type shared with show",
    ),
    (
        "agent-task list",
        "schema-versioned discovery report; `runs` shared with active and latest",
    ),
    (
        "agent-task controller list",
        "schema-versioned; controller records already carry an `entities` map",
    ),
    (
        "agent-task prompts list",
        "schema-versioned prompt-store report with prompt_dir",
    ),
    (
        "bench list",
        "per-component workload discovery report (scenarios, profiles, rig package)",
    ),
    (
        "contract list",
        "contract catalog keyed by kind, not a registry of entities",
    ),
    (
        "daemon jobs list",
        "action-tagged daemon job projection; follow-up candidate",
    ),
    (
        "file list",
        "remote directory listing; FileOutput shared with read/write/delete",
    ),
    (
        "fuzz list",
        "per-component workload discovery report with diagnostics",
    ),
    ("logs list", "LogsOutput shared with show/clear/search"),
    (
        "project components list",
        "nested sub-resource under data.components, shared with set/attach",
    ),
    (
        "project pin list",
        "nested sub-resource under data.pin, shared with add/remove/update",
    ),
    (
        "refactor undo list",
        "command-tagged snapshot report; follow-up candidate",
    ),
    (
        "release readiness list",
        "retained operation records for one component; follow-up candidate",
    ),
    (
        "review ci list",
        "CI inventory with three row sets (profiles, jobs, discovered_jobs)",
    ),
    (
        "rig sources list",
        "three row sets (sources, orphaned_stacks, invalid)",
    ),
    (
        "runner broker list",
        "RunnerBrokerOutput shared with pair/revoke",
    ),
    (
        "runner job list",
        "live/retained job projection with counts; follow-up candidate",
    ),
    (
        "runner workspace list",
        "workspace roots alongside rows; lab-runner owned type",
    ),
    (
        "runs list",
        "report-shaped: counts, search provenance, and degradations alongside runs",
    ),
    (
        "schedule list",
        "data is a bare array; adding a key is a breaking change needing its own decision",
    ),
    (
        "ssh list",
        "own versioned schema (homeboy/ssh-list/v1) with operator summary and truncation",
    ),
    (
        "tunnel preview-ingress list",
        "nested action-tagged payload under data.preview_ingress",
    ),
    (
        "worktree list",
        "keyset-paged report (cursor, next_cursor, diagnostics); follow-up candidate",
    ),
    (
        "worktree quarantine list",
        "action-tagged quarantine records; follow-up candidate",
    ),
];

fn collect_list_paths(
    command: &clap::Command,
    prefix: &mut Vec<String>,
    out: &mut BTreeSet<String>,
) {
    for subcommand in command.get_subcommands() {
        prefix.push(subcommand.get_name().to_string());
        if subcommand.get_name() == "list" {
            out.insert(prefix.join(" "));
        }
        collect_list_paths(subcommand, prefix, out);
        prefix.pop();
    }
}

fn registered_list_paths() -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    collect_list_paths(&Cli::command(), &mut Vec::new(), &mut paths);
    paths
}

#[test]
fn every_list_subcommand_is_classified_by_payload_shape() {
    let classified: BTreeSet<String> = ENTITY_LIST_COMMANDS
        .iter()
        .copied()
        .chain(EXEMPT_LIST_COMMANDS.iter().map(|(path, _)| *path))
        .map(str::to_string)
        .collect();
    let registered = registered_list_paths();

    let unclassified: Vec<&String> = registered.difference(&classified).collect();
    assert!(
        unclassified.is_empty(),
        "unclassified `list` subcommands {unclassified:?}: return rows under `data.entities` \
         (homeboy::core::EntityRows) and add to ENTITY_LIST_COMMANDS, or add to \
         EXEMPT_LIST_COMMANDS with a reason (#14876)"
    );

    let stale: Vec<&String> = classified.difference(&registered).collect();
    assert!(
        stale.is_empty(),
        "classified `list` paths no longer in the command tree: {stale:?}"
    );
}

#[test]
fn list_classifications_are_disjoint_and_reasoned() {
    let entity: BTreeSet<&str> = ENTITY_LIST_COMMANDS.iter().copied().collect();
    assert_eq!(
        entity.len(),
        ENTITY_LIST_COMMANDS.len(),
        "duplicate entity entry"
    );

    let mut exempt = BTreeSet::new();
    for (path, reason) in EXEMPT_LIST_COMMANDS {
        assert!(exempt.insert(*path), "duplicate exempt entry {path}");
        assert!(!entity.contains(path), "{path} is both entity and exempt");
        assert!(
            !reason.trim().is_empty(),
            "{path} needs an exemption reason"
        );
    }
}
