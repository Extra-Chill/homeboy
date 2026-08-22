//! Shared top-level command metadata.
//!
//! This is the first narrow `CommandSpec` slice: top-level metadata that is
//! consumed by output routing, safety/docs manifest derivation, and command
//! lookup without changing parsed CLI behavior.

use super::output::{CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: &'static str,
    pub json_family: CommandJsonFamily,
    pub docs_slug: Option<&'static str>,
    pub representative_argv: Option<&'static [&'static str]>,
    pub safety: CommandSafetySpec,
    pub subcommand_safety: &'static [CommandPathSafetySpec],
    pub output_notes: &'static str,
    pub lab_supported: bool,
    pub lab_notes: &'static str,
    pub lab_support_summary: &'static [CommandLabSupportSummary],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandLabSupportSummary {
    pub contract_labels: &'static [&'static str],
    pub message_label: &'static str,
    pub hint_label: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandDocSpec {
    pub slug: &'static str,
    pub kind: CommandDocKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandDocKind {
    RuntimeExtensionCommand,
    Support,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSafetySpec {
    pub mutates: bool,
    pub operator: bool,
    pub dry_run_flag: Option<&'static str>,
    pub risk_exemption: Option<&'static str>,
    pub dangerous_flags: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandPathSafetySpec {
    /// Space-separated subcommand paths *below* the owning command, matched
    /// against the clap-derived command surface. The empty string addresses the
    /// owning command itself.
    pub paths: &'static [&'static str],
    pub safety: CommandSafetySpec,
    pub output_notes: Option<&'static str>,
    pub lab_notes: Option<&'static str>,
}

impl CommandSafetySpec {
    pub const fn read_only() -> Self {
        Self {
            mutates: false,
            operator: false,
            dry_run_flag: None,
            risk_exemption: None,
            dangerous_flags: &[],
        }
    }
}

pub const DEFAULT_LAB_UNSUPPORTED_NOTES: &str =
    "not declared as Lab-routable in the command registry";
pub(crate) const AGENT_TASK_RUN_LAB_LABEL: &str = "agent-task cook/run-plan/retry --run";
pub(crate) const AGENT_TASK_PROMOTE_LAB_LABEL: &str = "agent-task promote";
pub(crate) const AGENT_TASK_CONTROLLER_FROM_SPEC_LAB_LABEL: &str =
    "agent-task controller from-spec --resume/run-from-spec/materialize";
pub(crate) const AGENT_TASK_CONTROLLER_RESUME_LAB_LABEL: &str = "agent-task controller resume";
pub(crate) const AGENT_TASK_STATUS_LAB_LABEL: &str =
    "agent-task run/run-next/status/logs/artifacts/review/list/active/latest";
pub(crate) const AGENT_TASK_PROVIDERS_LAB_LABEL: &str = "agent-task providers";
pub(crate) const AGENT_TASK_FANOUT_COOK_BATCH_LAB_LABEL: &str = "agent-task fanout cook-batch";
pub(crate) const AGENT_TASK_FANOUT_RUN_PLAN_LAB_LABEL: &str = "agent-task fanout run-plan";
pub(crate) const AGENT_TASK_FANOUT_SUBMIT_BATCH_LAB_LABEL: &str = "agent-task fanout submit-batch";
pub(crate) const AGENT_TASK_FANOUT_STATUS_LAB_LABEL: &str = "agent-task fanout status/artifacts";
pub(crate) const AGENT_TASK_AUTH_STATUS_LAB_LABEL: &str = "agent-task auth status";
// The lab-runnable command labels below are the ones `LabRunnerWorkload`
// classification matches on; they live in the homeboy-lab-contract crate and are
// re-exported here so existing `spec::*_LAB_LABEL` call sites are unchanged.
pub(crate) use homeboy_lab_contract::lab::labels::{
    AUDIT_LAB_LABEL, BENCH_LAB_LABEL, FUZZ_DOCTOR_LAB_LABEL, FUZZ_LAB_LABEL, LINT_LAB_LABEL,
    REFACTOR_LAB_LABEL, REVIEW_LAB_LABEL, RIG_CHECK_LAB_LABEL, RIG_RUN_LAB_LABEL,
    RUNTIME_REFRESH_LAB_LABEL, TEST_LAB_LABEL, TRACE_LAB_LABEL,
};

pub(crate) const RIG_SOURCE_MANAGEMENT_LAB_LABEL: &str = "rig source management";
pub(crate) const EXTENSION_DEV_RUN_LAB_LABEL: &str = "extension dev-run";
pub(crate) const EXTENSION_REFRESH_LAB_LABEL: &str = "extension refresh";
pub(crate) const EXTENSION_UPDATE_LAB_LABEL: &str = "extension update";
pub(crate) const WORKTREE_CLEANUP_LAB_LABEL: &str = "worktree cleanup";
pub(crate) const TUNNEL_PREVIEW_CONSUMER_RUN_LAB_LABEL: &str = "tunnel preview-consumer run";
pub(crate) const TUNNEL_SERVICE_EXPOSE_LAB_LABEL: &str = "tunnel service expose";
pub(crate) const TUNNEL_SERVICE_START_LAB_LABEL: &str = "tunnel service start";

impl CommandSpec {
    pub fn docs_path(&self) -> Option<String> {
        self.docs_slug
            .map(|slug| format!("docs/commands/{slug}.md"))
    }

    pub const fn output_descriptor(
        &self,
        output_file_mode: CommandOutputFileMode,
    ) -> CommandOutputDescriptor {
        CommandOutputDescriptor::json_envelope(self.json_family, output_file_mode)
    }

    pub(crate) fn path_safety(&self, path: &[&str]) -> Option<&'static CommandPathSafetySpec> {
        self.subcommand_safety.iter().find(|entry| {
            entry
                .paths
                .iter()
                .any(|entry_path| path_matches(entry_path, path))
        })
    }
}

fn path_matches(spec: &str, path: &[&str]) -> bool {
    spec.split_whitespace().eq(path.iter().copied())
}

const fn command_spec(name: &'static str, json_family: CommandJsonFamily) -> CommandSpec {
    CommandSpec {
        name,
        json_family,
        docs_slug: Some(name),
        representative_argv: None,
        safety: CommandSafetySpec::read_only(),
        subcommand_safety: &[],
        output_notes: "standard CLI output contract",
        lab_supported: false,
        lab_notes: DEFAULT_LAB_UNSUPPORTED_NOTES,
        lab_support_summary: &[],
    }
}

const fn command_spec_with_safety(
    name: &'static str,
    json_family: CommandJsonFamily,
    safety: CommandSafetySpec,
) -> CommandSpec {
    CommandSpec {
        safety,
        ..command_spec(name, json_family)
    }
}

const fn command_spec_with_output_notes_and_safety(
    name: &'static str,
    json_family: CommandJsonFamily,
    output_notes: &'static str,
    safety: CommandSafetySpec,
) -> CommandSpec {
    CommandSpec {
        safety,
        ..command_spec_with_output_notes(name, json_family, output_notes)
    }
}

const fn command_spec_with_output_notes(
    name: &'static str,
    json_family: CommandJsonFamily,
    output_notes: &'static str,
) -> CommandSpec {
    CommandSpec {
        output_notes,
        ..command_spec(name, json_family)
    }
}

const fn lab_command_spec_with_output_notes(
    name: &'static str,
    json_family: CommandJsonFamily,
    lab_notes: &'static str,
    output_notes: &'static str,
) -> CommandSpec {
    CommandSpec {
        output_notes,
        ..lab_command_spec(name, json_family, lab_notes)
    }
}

const fn lab_command_spec(
    name: &'static str,
    json_family: CommandJsonFamily,
    lab_notes: &'static str,
) -> CommandSpec {
    CommandSpec {
        lab_supported: true,
        lab_notes,
        ..command_spec(name, json_family)
    }
}

const fn lab_command_spec_with_summary(
    name: &'static str,
    json_family: CommandJsonFamily,
    lab_notes: &'static str,
    lab_support_summary: &'static [CommandLabSupportSummary],
) -> CommandSpec {
    CommandSpec {
        lab_support_summary,
        ..lab_command_spec(name, json_family, lab_notes)
    }
}

const fn lab_command_spec_with_output_notes_and_summary(
    name: &'static str,
    json_family: CommandJsonFamily,
    lab_notes: &'static str,
    output_notes: &'static str,
    lab_support_summary: &'static [CommandLabSupportSummary],
) -> CommandSpec {
    CommandSpec {
        lab_support_summary,
        ..lab_command_spec_with_output_notes(name, json_family, lab_notes, output_notes)
    }
}

const fn command_spec_with_representative_argv(
    representative_argv: &'static [&'static str],
    spec: CommandSpec,
) -> CommandSpec {
    CommandSpec {
        representative_argv: Some(representative_argv),
        ..spec
    }
}

const AGENT_TASK_LAB_SUPPORT: &[CommandLabSupportSummary] = &[
    CommandLabSupportSummary {
        contract_labels: &[AGENT_TASK_RUN_LAB_LABEL],
        message_label: "agent-task cook/run-plan",
        hint_label: "agent-task cook/run-plan",
    },
    CommandLabSupportSummary {
        contract_labels: &[AGENT_TASK_PROMOTE_LAB_LABEL],
        message_label: AGENT_TASK_PROMOTE_LAB_LABEL,
        hint_label: AGENT_TASK_PROMOTE_LAB_LABEL,
    },
    CommandLabSupportSummary {
        contract_labels: &[
            AGENT_TASK_CONTROLLER_FROM_SPEC_LAB_LABEL,
            AGENT_TASK_CONTROLLER_RESUME_LAB_LABEL,
        ],
        message_label: "agent-task controller from-spec --resume/run-from-spec/materialize/resume",
        hint_label: "agent-task controller from-spec --resume/run-from-spec/materialize/resume",
    },
    CommandLabSupportSummary {
        contract_labels: &[AGENT_TASK_RUN_LAB_LABEL],
        message_label: "agent-task retry --run",
        hint_label: "agent-task retry --run",
    },
    CommandLabSupportSummary {
        contract_labels: &[AGENT_TASK_STATUS_LAB_LABEL, AGENT_TASK_PROVIDERS_LAB_LABEL],
        message_label:
            "agent-task run/run-next/status/logs/artifacts/review/list/active/latest/providers",
        hint_label:
            "agent-task run/run-next/status/logs/artifacts/review/list/active/latest/providers",
    },
    CommandLabSupportSummary {
        contract_labels: &[
            AGENT_TASK_FANOUT_COOK_BATCH_LAB_LABEL,
            AGENT_TASK_FANOUT_RUN_PLAN_LAB_LABEL,
            AGENT_TASK_FANOUT_SUBMIT_BATCH_LAB_LABEL,
            AGENT_TASK_FANOUT_STATUS_LAB_LABEL,
        ],
        message_label: "agent-task fanout cook-batch/run-plan/submit-batch/status/artifacts",
        hint_label: "agent-task fanout cook-batch/run-plan/submit-batch/status/artifacts",
    },
    CommandLabSupportSummary {
        contract_labels: &[AGENT_TASK_AUTH_STATUS_LAB_LABEL],
        message_label: AGENT_TASK_AUTH_STATUS_LAB_LABEL,
        hint_label: AGENT_TASK_AUTH_STATUS_LAB_LABEL,
    },
];

const REVIEW_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[REVIEW_LAB_LABEL],
    message_label: REVIEW_LAB_LABEL,
    hint_label: REVIEW_LAB_LABEL,
}];

const BENCH_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[BENCH_LAB_LABEL],
    message_label: BENCH_LAB_LABEL,
    hint_label: "bench run",
}];

const FUZZ_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[FUZZ_LAB_LABEL, FUZZ_DOCTOR_LAB_LABEL],
    message_label: "fuzz run/doctor",
    hint_label: "fuzz run/doctor",
}];

const TRACE_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[TRACE_LAB_LABEL],
    message_label: TRACE_LAB_LABEL,
    hint_label: TRACE_LAB_LABEL,
}];

const REFACTOR_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[REFACTOR_LAB_LABEL],
    message_label: "refactor source runs",
    hint_label: "refactor source runs",
}];

const RIG_LAB_SUPPORT: &[CommandLabSupportSummary] = &[
    CommandLabSupportSummary {
        contract_labels: &[RIG_CHECK_LAB_LABEL],
        message_label: RIG_CHECK_LAB_LABEL,
        hint_label: RIG_CHECK_LAB_LABEL,
    },
    CommandLabSupportSummary {
        contract_labels: &[RIG_RUN_LAB_LABEL],
        message_label: RIG_RUN_LAB_LABEL,
        hint_label: RIG_RUN_LAB_LABEL,
    },
];

const RUNTIME_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[RUNTIME_REFRESH_LAB_LABEL],
    message_label: RUNTIME_REFRESH_LAB_LABEL,
    hint_label: RUNTIME_REFRESH_LAB_LABEL,
}];

const EXTENSION_LAB_SUPPORT: &[CommandLabSupportSummary] = &[
    CommandLabSupportSummary {
        contract_labels: &[EXTENSION_DEV_RUN_LAB_LABEL],
        message_label: EXTENSION_DEV_RUN_LAB_LABEL,
        hint_label: EXTENSION_DEV_RUN_LAB_LABEL,
    },
    CommandLabSupportSummary {
        contract_labels: &[EXTENSION_REFRESH_LAB_LABEL],
        message_label: EXTENSION_REFRESH_LAB_LABEL,
        hint_label: EXTENSION_REFRESH_LAB_LABEL,
    },
    CommandLabSupportSummary {
        contract_labels: &[EXTENSION_UPDATE_LAB_LABEL],
        message_label: EXTENSION_UPDATE_LAB_LABEL,
        hint_label: EXTENSION_UPDATE_LAB_LABEL,
    },
];

const WORKTREE_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[WORKTREE_CLEANUP_LAB_LABEL],
    message_label: WORKTREE_CLEANUP_LAB_LABEL,
    hint_label: "worktree cleanup --runner <runner-id>",
}];

const TUNNEL_LAB_SUPPORT: &[CommandLabSupportSummary] = &[
    CommandLabSupportSummary {
        contract_labels: &[TUNNEL_PREVIEW_CONSUMER_RUN_LAB_LABEL],
        message_label: TUNNEL_PREVIEW_CONSUMER_RUN_LAB_LABEL,
        hint_label: TUNNEL_PREVIEW_CONSUMER_RUN_LAB_LABEL,
    },
    CommandLabSupportSummary {
        contract_labels: &[TUNNEL_SERVICE_EXPOSE_LAB_LABEL],
        message_label: TUNNEL_SERVICE_EXPOSE_LAB_LABEL,
        hint_label: TUNNEL_SERVICE_EXPOSE_LAB_LABEL,
    },
    CommandLabSupportSummary {
        contract_labels: &[TUNNEL_SERVICE_START_LAB_LABEL],
        message_label: TUNNEL_SERVICE_START_LAB_LABEL,
        hint_label: TUNNEL_SERVICE_START_LAB_LABEL,
    },
];

const DEPLOY_DANGEROUS_FLAGS: &[&str] = &["--head", "--force"];
const RELEASE_DANGEROUS_FLAGS: &[&str] = &[
    "--apply",
    "--deploy",
    "--recover",
    "--retag",
    "--head",
    "--skip-checks",
    "--force-lower-bump",
];
const UPGRADE_DANGEROUS_FLAGS: &[&str] = &["--force", "--upgrade-runner"];
const FUZZ_DANGEROUS_FLAGS: &[&str] = &["--allow-destructive"];
const CLEANUP_DANGEROUS_FLAGS: &[&str] = &["--apply"];
const TRIAGE_DANGEROUS_FLAGS: &[&str] = &["--auto-merge"];
const REFACTOR_DANGEROUS_FLAGS: &[&str] = &["--write", "--commit"];
const FILE_APPLY_DANGEROUS_FLAGS: &[&str] = &["--apply"];
const FLEET_EXEC_DANGEROUS_FLAGS: &[&str] = &["--apply"];
const API_MUTATION_DANGEROUS_FLAGS: &[&str] = &["--apply"];
const API_HTTP_REQUEST_DANGEROUS_FLAGS: &[&str] =
    &["--apply", "METHOD!=GET", "METHOD!=HEAD", "METHOD!=OPTIONS"];

const fn mutating_safety() -> CommandSafetySpec {
    CommandSafetySpec {
        mutates: true,
        operator: false,
        dry_run_flag: None,
        risk_exemption: None,
        dangerous_flags: &[],
    }
}

const fn operator_safety(
    dry_run_flag: Option<&'static str>,
    dangerous_flags: &'static [&'static str],
) -> CommandSafetySpec {
    CommandSafetySpec {
        mutates: true,
        operator: true,
        dry_run_flag,
        risk_exemption: None,
        dangerous_flags,
    }
}

const fn guarded_safety(dangerous_flags: &'static [&'static str]) -> CommandSafetySpec {
    CommandSafetySpec {
        mutates: false,
        operator: false,
        dry_run_flag: None,
        risk_exemption: None,
        dangerous_flags,
    }
}

const fn guarded_mutating_safety(dangerous_flags: &'static [&'static str]) -> CommandSafetySpec {
    CommandSafetySpec {
        mutates: true,
        operator: false,
        dry_run_flag: None,
        risk_exemption: None,
        dangerous_flags,
    }
}

const fn with_dry_run(safety: CommandSafetySpec, dry_run_flag: &'static str) -> CommandSafetySpec {
    CommandSafetySpec {
        dry_run_flag: Some(dry_run_flag),
        ..safety
    }
}

const fn with_risk_exemption(
    safety: CommandSafetySpec,
    risk_exemption: &'static str,
) -> CommandSafetySpec {
    CommandSafetySpec {
        risk_exemption: Some(risk_exemption),
        ..safety
    }
}

/// Operator-gated but non-mutating: an explicit operator entry point that only
/// renders a plan.
const fn operator_read_only() -> CommandSafetySpec {
    CommandSafetySpec {
        operator: true,
        ..CommandSafetySpec::read_only()
    }
}

const fn paths_safety(
    paths: &'static [&'static str],
    safety: CommandSafetySpec,
    output_notes: &'static str,
) -> CommandPathSafetySpec {
    CommandPathSafetySpec {
        paths,
        safety,
        output_notes: Some(output_notes),
        lab_notes: None,
    }
}

const fn paths_safety_without_notes(
    paths: &'static [&'static str],
    safety: CommandSafetySpec,
) -> CommandPathSafetySpec {
    CommandPathSafetySpec {
        paths,
        safety,
        output_notes: None,
        lab_notes: None,
    }
}

const DEPS_MUTATING_PATHS: &[&str] = &["install", "update", "stack apply"];
const FILE_APPLY_PATHS: &[&str] = &["write", "delete", "mkdir", "rename"];
const FILE_TRANSFER_PATHS: &[&str] = &["copy", "sync"];
const FLEET_CONFIG_PATHS: &[&str] = &["create", "set", "delete", "add", "remove"];
const API_MUTATION_PATHS: &[&str] = &["post", "put", "patch", "delete"];
const API_AUTH_PATHS: &[&str] = &[
    "auth login",
    "auth set",
    "auth remove",
    "auth logout",
    "auth profile set-basic",
    "auth profile set-bearer",
    "auth profile remove",
];
const SERVER_OPERATOR_PATHS: &[&str] = &[
    "create",
    "set",
    "delete",
    "connect",
    "disconnect",
    "key generate",
    "key import",
    "key use",
    "key unset",
];
const CONFIG_MUTATING_PATHS: &[&str] = &["set", "remove", "reset"];
const PROJECT_MUTATING_PATHS: &[&str] = &[
    "create",
    "set",
    "remove",
    "rename",
    "delete",
    "init",
    "components set",
    "components attach-path",
    "components attach-paths",
    "components remove",
    "components clear",
    "pin add",
    "pin remove",
    "pin rename",
    "pin update",
];
const COMPONENT_MUTATING_PATHS: &[&str] = &["create", "set", "delete", "rename", "setup"];
const COMPONENT_GUARDED_PATHS: &[&str] = &["reconcile", "artifacts"];
const RIG_STATIC_LINT_PATHS: &[&str] = &["lint", "package lint", "materialize"];
const RIG_RUNTIME_MUTATING_PATHS: &[&str] = &["down", "repair", "install", "update"];
const RIG_MANAGED_FILE_PATHS: &[&str] = &["sync", "app install", "app update", "app uninstall"];

const DEPS_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[paths_safety(
    DEPS_MUTATING_PATHS,
    mutating_safety(),
    "mutates dependency manifests, lockfiles, or installed dependency trees",
)];

const FILE_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        FILE_APPLY_PATHS,
        operator_safety(None, FILE_APPLY_DANGEROUS_FLAGS),
        "default output is a non-mutating plan; pass --apply to mutate",
    ),
    paths_safety(
        &["edit"],
        operator_safety(Some("--dry-run"), &[]),
        "mutates file content unless --dry-run is passed",
    ),
    paths_safety_without_notes(FILE_TRANSFER_PATHS, mutating_safety()),
];

const FLEET_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    CommandPathSafetySpec {
        paths: &["exec"],
        safety: operator_safety(Some("--check"), FLEET_EXEC_DANGEROUS_FLAGS),
        output_notes: Some(
            "default output is blocked for remote execution; pass --check to plan or --apply to execute",
        ),
        lab_notes: Some(
            "local-only: depends on local fleet/project/server configuration before SSH fan-out",
        ),
    },
    paths_safety(FLEET_CONFIG_PATHS, mutating_safety(), "mutates local fleet configuration"),
];

const API_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        API_MUTATION_PATHS,
        operator_safety(None, API_MUTATION_DANGEROUS_FLAGS),
        "mutating API requests require --apply",
    ),
    paths_safety(
        &["http request"],
        operator_safety(None, API_HTTP_REQUEST_DANGEROUS_FLAGS),
        "mutating HTTP methods require --apply; GET, HEAD, and OPTIONS are allowed without it",
    ),
    paths_safety(
        API_AUTH_PATHS,
        operator_safety(None, &[]),
        "mutates keychain-backed authentication state",
    ),
];

const SERVER_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[paths_safety_without_notes(
    SERVER_OPERATOR_PATHS,
    operator_safety(None, &[]),
)];
const CONFIG_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[paths_safety_without_notes(
    CONFIG_MUTATING_PATHS,
    mutating_safety(),
)];
const PROJECT_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[paths_safety_without_notes(
    PROJECT_MUTATING_PATHS,
    mutating_safety(),
)];
const COMPONENT_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety_without_notes(COMPONENT_MUTATING_PATHS, mutating_safety()),
    paths_safety(
        COMPONENT_GUARDED_PATHS,
        guarded_mutating_safety(&["--apply"]),
        "default output is non-mutating; pass --apply to repair or remove artifacts",
    ),
];
const RIG_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        RIG_STATIC_LINT_PATHS,
        CommandSafetySpec::read_only(),
        "reads rig package files and emits the standard JSON lint report without evaluating the live environment",
    ),
    paths_safety(
        &["release-lock"],
        operator_safety(None, &["--force"]),
        "releases a local rig active-run lease; --force can reclaim a live holder's guardrail",
    ),
    paths_safety(
        &["up"],
        operator_safety(Some("--dry-run"), &[]),
        "mutates local rig runtime state unless --dry-run is passed with --runner to emit a runner exec plan",
    ),
    paths_safety(
        RIG_RUNTIME_MUTATING_PATHS,
        operator_safety(None, &[]),
        "mutates local rig runtime state or installed rig packages",
    ),
    paths_safety(
        RIG_MANAGED_FILE_PATHS,
        operator_safety(Some("--dry-run"), &[]),
        "mutates rig-managed files unless --dry-run is passed",
    ),
    paths_safety(
        &["sources remove", "sources refresh"],
        mutating_safety(),
        "mutates installed rig source metadata",
    ),
];

const FUZZ_CASE_REPLAY_PATHS: &[&str] = &["replay", "minimize"];
/// The empty path addresses bare `homeboy fuzz`, which shares the default
/// planning/execution contract with its measurement subcommands.
const FUZZ_MEASUREMENT_PATHS: &[&str] = &["", "run", "plan", "run-campaign"];

const FUZZ_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        FUZZ_CASE_REPLAY_PATHS,
        mutating_safety(),
        "replays or minimizes a persisted fuzz case against local code and may write run artifacts",
    ),
    paths_safety(
        FUZZ_MEASUREMENT_PATHS,
        guarded_safety(FUZZ_DANGEROUS_FLAGS),
        "read-only fuzz planning/execution contract by default; --allow-destructive infers isolated mode and attaches an auditable homeboy/isolation-proof/v1 unless one is supplied",
    ),
];

const WORKTREE_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        &["queue-create"],
        with_dry_run(mutating_safety(), "--dry-run"),
        "default output creates task worktrees one-at-a-time; pass --dry-run to plan without creating",
    ),
    paths_safety(
        &["create"],
        mutating_safety(),
        "creates a task worktree from a registered component checkout",
    ),
    paths_safety(
        &["remove"],
        guarded_mutating_safety(&["--force"]),
        "removes a task worktree after safety checks",
    ),
    paths_safety(
        &["cleanup"],
        operator_safety(
            Some("--dry-run"),
            &["--apply", "--force", "--cleanup-artifacts"],
        ),
        "default output is a non-mutating task-worktree cleanup plan; pass --apply to remove eligible worktrees, and --cleanup-artifacts to include rebuildable Homeboy artifacts",
    ),
    paths_safety(
        &["inventory"],
        operator_safety(None, &["--apply"]),
        "bounded cursor-paginated task-worktree and adopted-workspace inventory; --apply reconciles only leased terminal snapshots and reports typed refusals for incomplete local or offloaded authority",
    ),
];

const TUNNEL_SERVICE_DECLARATION_PATHS: &[&str] =
    &["service expose", "service set", "service remove"];
const TUNNEL_PREVIEW_RUNTIME_PATHS: &[&str] = &[
    "preview-client start",
    "preview-consumer run",
    "preview-ingress serve",
    "artifact-origin serve",
];

const TUNNEL_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        TUNNEL_SERVICE_DECLARATION_PATHS,
        operator_safety(None, &[]),
        "mutates private service tunnel declarations",
    ),
    paths_safety(
        &["service start", "service stop"],
        operator_safety(None, &[]),
        "mutates private service tunnel runtime state",
    ),
    paths_safety(
        TUNNEL_PREVIEW_RUNTIME_PATHS,
        operator_safety(None, &[]),
        "starts or supervises tunnel preview runtime state",
    ),
    paths_safety(
        &["preview-ingress route", "preview-ingress unroute"],
        operator_safety(None, &[]),
        "mutates preview ingress route state",
    ),
    paths_safety(
        &["preview-ingress install"],
        operator_read_only(),
        "renders a non-destructive operator install plan",
    ),
];

const STACK_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        &["create", "add-pr", "remove-pr"],
        mutating_safety(),
        "mutates persisted stack specification metadata",
    ),
    paths_safety(
        &["apply", "rebase"],
        with_risk_exemption(
            operator_safety(None, &[]),
            "stack command name is the explicit branch mutation action; status/sync --dry-run are the planning paths",
        ),
        "mutates the configured stack target branch",
    ),
    paths_safety(
        &["sync"],
        operator_safety(Some("--dry-run"), &[]),
        "mutates the configured stack target branch and may update the stack spec unless --dry-run is passed",
    ),
    paths_safety(
        &["push"],
        with_risk_exemption(
            operator_safety(None, &[]),
            "push is the explicit remote publication action; no dry-run contract exists yet",
        ),
        "pushes the configured stack target branch to its remote",
    ),
];

const RUNNER_CONFIG_PATHS: &[&str] = &[
    "add",
    "enable",
    "set",
    "trust",
    "pair",
    "remove",
    "disconnect",
    "refresh-homeboy",
];

const RUNNER_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        RUNNER_CONFIG_PATHS,
        operator_safety(None, &[]),
        "mutates runner configuration, trust policy, or runner lifecycle state",
    ),
    paths_safety(
        &["connect", "work"],
        with_risk_exemption(
            operator_safety(None, &[]),
            "runner lifecycle command name is the explicit operator action; no dry-run contract exists yet",
        ),
        "mutates runner lifecycle state",
    ),
    paths_safety(
        &["doctor"],
        operator_safety(None, &["--repair"]),
        "diagnoses runners by default; --repair mutates runner lifecycle state",
    ),
    paths_safety(
        &["exec"],
        operator_safety(Some("--dry-run"), &[]),
        "executes commands on a runner unless --dry-run is passed",
    ),
    paths_safety(
        &["lifecycle"],
        CommandSafetySpec::read_only(),
        "non-mutating runner workspace lifecycle/finalization readiness report suitable for RunOutcomeEnvelope embedding",
    ),
    paths_safety(
        &["workspace sync"],
        operator_safety(None, &["--allow-dirty-lab-workspace"]),
        "materializes a local worktree into runner workspace state",
    ),
    paths_safety(
        &["workspace update"],
        operator_safety(None, &[]),
        "advances a prepared runner workspace from its snapshot lease",
    ),
    paths_safety(
        &["workspace pull"],
        operator_safety(Some("--dry-run"), &[]),
        "copies selected files from runner workspace state to a local destination",
    ),
    paths_safety(
        &["workspace apply"],
        operator_safety(None, &["--force"]),
        "applies a Lab-generated workspace patch to a local worktree",
    ),
    paths_safety(
        &["workspace prune"],
        operator_safety(None, &["--apply"]),
        "default output is a non-mutating orphan cleanup plan with candidate/remaining bytes; pass --apply to delete exact runner workspace paths and --passes to drain bounded pages",
    ),
];

const GIT_ISSUE_WRITE_PATHS: &[&str] =
    &["issue create", "issue comment", "issue close", "issue edit"];
const GIT_PR_WRITE_PATHS: &[&str] = &[
    "pr create",
    "pr edit",
    "pr comment",
    "pr refresh",
    "pr policy open",
];

const GIT_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        GIT_ISSUE_WRITE_PATHS,
        with_risk_exemption(
            operator_safety(None, &[]),
            "the issue subcommand is the explicit GitHub write action; no dry-run contract exists yet",
        ),
        "mutates GitHub issue state through the configured repository",
    ),
    paths_safety(
        GIT_PR_WRITE_PATHS,
        with_risk_exemption(
            operator_safety(None, &[]),
            "the PR subcommand is the explicit GitHub write action; no dry-run contract exists yet",
        ),
        "mutates GitHub pull request state or branch state",
    ),
    paths_safety(
        &["pr fleet", "pr land"],
        operator_safety(Some("--dry-run"), &["--apply", "--delete-branch"]),
        "reports by default or with --dry-run; apply/merge flags mutate PR state",
    ),
];

const REFACTOR_SOURCE_REWRITE_PATHS: &[&str] = &[
    "rename",
    "add",
    "move",
    "propagate",
    "transform",
    "decompose",
];

const REFACTOR_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        REFACTOR_SOURCE_REWRITE_PATHS,
        guarded_mutating_safety(&["--write"]),
        "reports a plan by default; pass --write to rewrite source files",
    ),
    paths_safety(
        &["undo delete"],
        mutating_safety(),
        "deletes an undo snapshot without restoring it",
    ),
];

const DB_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[paths_safety(
    &["delete-row", "drop-table"],
    operator_safety(None, &[]),
    "default output is a non-mutating plan; pass --apply to mutate",
)];

/// `review audit-baseline` is the real clap path; `review audit baseline` only
/// exists as a pre-parse argv rewrite in `commands::utils::args`, so declaring
/// safety against that spelling silently classifies the command as read-only.
const REVIEW_AUDIT_BASELINE_PATHS: &[&str] = &[
    "audit-baseline refresh",
    "audit-baseline merge",
    "audit-baseline prune",
];

const AGENT_TASK_CONTROLLER_PATHS: &[&str] = &[
    "controller init",
    "controller from-spec",
    "controller run-from-spec",
    "controller materialize",
    "controller events",
    "controller apply-event",
    "controller run-next",
    "controller run",
    "controller resume",
    "controller mark-human-ready",
];

const AGENT_TASK_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        &["verify-replacement"],
        mutating_safety(),
        "executes operator-supplied deterministic shell gates and records durable replacement proof",
    ),
    paths_safety(
        &["promote"],
        with_dry_run(mutating_safety(), "--dry-run"),
        "applies a selected patch artifact into a managed worktree unless --dry-run is passed",
    ),
    paths_safety(
        &["active"],
        with_dry_run(guarded_mutating_safety(&["--apply"]), "--dry-run"),
        "reads active runs by default; --reconcile previews the full fleet mutation set and --apply authorizes its cancellation",
    ),
    paths_safety(
        &["reconcile"],
        with_dry_run(guarded_mutating_safety(&["--apply"]), "--dry-run"),
        "previews reconciliation for one durable run; --apply authorizes a scoped lifecycle mutation after provider-state inspection",
    ),
    paths_safety(
        AGENT_TASK_CONTROLLER_PATHS,
        mutating_safety(),
        "mutates durable agent-task loop controller state",
    ),
    paths_safety(
        &["auth remove"],
        operator_safety(None, &[]),
        "removes one agent-task provider secret source mapping",
    ),
    paths_safety(
        &["prompts remove"],
        mutating_safety(),
        "removes one stored agent-task prompt",
    ),
    paths_safety(
        &["fanout cook-batch"],
        operator_safety(Some("--dry-run"), &["--run-plan"]),
        "creates/reuses task worktrees and can run the generated fanout unless --dry-run is passed",
    ),
];

const RUNS_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        &[
            "report failure-digest",
            "report performance-digest",
            "report bench-coverage",
            "report browser-evidence-compare",
            "report matrix-artifacts",
            "report compare",
        ],
        CommandSafetySpec::read_only(),
        "renders read-only evidence projections from supplied or persisted artifacts",
    ),
    paths_safety(
        &["reconcile"],
        with_dry_run(mutating_safety(), "--dry-run"),
        "marks orphaned running records stale unless --dry-run is passed",
    ),
    paths_safety(
        &["import"],
        mutating_safety(),
        "imports observation bundle or GitHub Actions artifacts into the local run store",
    ),
    paths_safety(
        &["loop-sync"],
        with_dry_run(mutating_safety(), "--dry-run"),
        "syncs copied loop archives into observation runs/artifacts unless --dry-run is passed",
    ),
    paths_safety(
        &["artifact cleanup-downloads", "artifact cleanup-persisted"],
        guarded_mutating_safety(&["--apply"]),
        "default output is a non-mutating cleanup plan; pass --apply to delete artifacts",
    ),
    paths_safety(
        &["resources"],
        guarded_mutating_safety(&["--apply"]),
        "default output is non-mutating; pass --cleanup-plan to plan lifecycle resource cleanup or --apply with --cleanup-root to delete bounded apply-intended candidates",
    ),
    paths_safety(
        &["artifact attach"],
        mutating_safety(),
        "copies an existing runner-side file into the persisted local artifact store and records it against a run",
    ),
    paths_safety(
        &["findings reconcile", "findings reconcile-run"],
        operator_safety(Some("--dry-run"), &["--apply"]),
        "default output is a non-mutating issue reconciliation plan; pass --apply to mutate tracker state",
    ),
];

const EXTENSION_MUTATING_PATHS: &[&str] = &[
    "setup",
    "refresh",
    "relink",
    "dev-run",
    "install-for-component",
    "converge",
    "set",
];
const EXTENSION_MUTATION_NOTES: &str =
    "mutates installed extension files or extension manifest metadata";
const EXTENSION_PASSTHROUGH_DANGEROUS_FLAGS: &[&str] =
    &["extension runtime command", "passthrough args"];

const EXTENSION_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        EXTENSION_MUTATING_PATHS,
        mutating_safety(),
        EXTENSION_MUTATION_NOTES,
    ),
    paths_safety(
        &["install"],
        guarded_mutating_safety(&["--replace"]),
        EXTENSION_MUTATION_NOTES,
    ),
    paths_safety(
        &["update"],
        guarded_mutating_safety(&["--force"]),
        EXTENSION_MUTATION_NOTES,
    ),
    paths_safety(
        &["uninstall"],
        guarded_mutating_safety(&["uninstall"]),
        EXTENSION_MUTATION_NOTES,
    ),
    paths_safety(
        &["run", "exec"],
        operator_safety(None, EXTENSION_PASSTHROUGH_DANGEROUS_FLAGS),
        "executes extension-owned runtime commands with forwarded arguments that may mutate the target system",
    ),
    paths_safety(
        &["action"],
        operator_safety(None, &["extension action"]),
        "executes extension-owned actions that may mutate the target system",
    ),
];

const RUNTIME_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[paths_safety(
    &["refresh"],
    mutating_safety(),
    "mutates installed runtime package files",
)];

const CLEANUP_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[paths_safety(
    &["artifacts"],
    guarded_mutating_safety(CLEANUP_DANGEROUS_FLAGS),
    "default output is a non-mutating cleanup plan; pass --apply to remove artifacts",
)];

const SELF_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        &["docs map"],
        guarded_mutating_safety(&["--write"]),
        "default JSON output is non-mutating; pass --write to write markdown docs to disk",
    ),
    paths_safety(
        &["cleanup-runtime-tmp"],
        operator_safety(None, &["--apply"]),
        "default output is a non-mutating cleanup plan; pass --apply to delete runtime temp entries",
    ),
];

const REVIEW_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        &["ci autofix"],
        operator_safety(None, &[]),
        "commits and pushes prepared CI autofix changes",
    ),
    paths_safety(
        REVIEW_AUDIT_BASELINE_PATHS,
        mutating_safety(),
        "mutates persisted audit baseline data in component configuration",
    ),
];

macro_rules! declare_command_specs {
    ($(($variant:ident, $handler:path, $value:expr),)*) => {
        pub const COMMAND_SPECS: &[CommandSpec] = &[$($value,)*];
    };
}

crate::builtin_json_command_descriptors!(declare_command_specs);

pub const COMMAND_DOC_REGISTRY: &[CommandDocSpec] = &[
    CommandDocSpec {
        slug: "audit-rules",
        kind: CommandDocKind::Support,
    },
    CommandDocSpec {
        slug: "cargo",
        kind: CommandDocKind::RuntimeExtensionCommand,
    },
    CommandDocSpec {
        slug: "commands-index",
        kind: CommandDocKind::Support,
    },
    CommandDocSpec {
        slug: "rig-spec",
        kind: CommandDocKind::Support,
    },
];

pub(crate) fn registered_command(name: &str) -> Option<&'static CommandSpec> {
    COMMAND_SPECS.iter().find(|entry| entry.name == name)
}

pub(crate) fn registered_command_json_family(name: &str) -> Option<CommandJsonFamily> {
    registered_command(name).map(|entry| entry.json_family)
}

pub(crate) fn runtime_extension_command_doc_slugs() -> impl Iterator<Item = &'static str> {
    COMMAND_DOC_REGISTRY
        .iter()
        .filter(|entry| entry.kind == CommandDocKind::RuntimeExtensionCommand)
        .map(|entry| entry.slug)
}

pub(crate) fn support_command_doc_slugs() -> impl Iterator<Item = &'static str> {
    COMMAND_DOC_REGISTRY
        .iter()
        .filter(|entry| entry.kind == CommandDocKind::Support)
        .map(|entry| entry.slug)
}

pub(crate) fn non_core_command_doc_slugs() -> impl Iterator<Item = &'static str> {
    COMMAND_DOC_REGISTRY.iter().map(|entry| entry.slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_converge_is_declared_mutating() {
        let extension = registered_command("extension").expect("extension command spec");
        let safety = extension
            .path_safety(&["converge"])
            .expect("extension converge safety");

        assert!(safety.safety.mutates);
        assert!(!safety.safety.operator);
    }

    #[test]
    fn verify_replacement_is_declared_mutating_shell_execution() {
        let agent_task = registered_command("agent-task").expect("agent-task command spec");
        let safety = agent_task
            .path_safety(&["verify-replacement"])
            .expect("verify-replacement safety");

        assert!(safety.safety.mutates);
        assert!(safety
            .output_notes
            .is_some_and(|notes| notes.contains("shell gates")));
    }
}
