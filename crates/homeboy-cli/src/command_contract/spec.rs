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
pub(crate) const AGENT_TASK_STATUS_LAB_LABEL: &str =
    "agent-task run/status/logs/artifacts/review/list/active";
pub(crate) const AGENT_TASK_PROVIDERS_LAB_LABEL: &str = "agent-task providers";
pub(crate) const AGENT_TASK_FANOUT_COOK_BATCH_LAB_LABEL: &str = "agent-task fanout cook-batch";
pub(crate) const AGENT_TASK_AUTH_STATUS_LAB_LABEL: &str = "agent-task auth status";
// The lab-runnable command labels below are the ones `LabRunnerWorkload`
// classification matches on; they live in the homeboy-lab-contract crate and are
// re-exported here so existing `spec::*_LAB_LABEL` call sites are unchanged.
pub(crate) use homeboy_lab_contract::lab::labels::{
    BENCH_LAB_LABEL, FUZZ_DOCTOR_LAB_LABEL, FUZZ_LAB_LABEL, LINT_LAB_LABEL, REVIEW_LAB_LABEL,
    RIG_CHECK_LAB_LABEL, RIG_RUN_LAB_LABEL, TEST_LAB_LABEL, TRACE_LAB_LABEL, WORKLOAD_LAB_LABEL,
};

pub(crate) const RIG_SOURCE_MANAGEMENT_LAB_LABEL: &str = "rig source management";
pub(crate) const EXTENSION_DEV_RUN_LAB_LABEL: &str = "extension dev-run";
pub(crate) const EXTENSION_REFRESH_LAB_LABEL: &str = "extension refresh";
pub(crate) const EXTENSION_UPDATE_LAB_LABEL: &str = "extension update";
pub(crate) const WORKTREE_CLEANUP_LAB_LABEL: &str = "worktree cleanup";

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
        contract_labels: &[AGENT_TASK_RUN_LAB_LABEL],
        message_label: "agent-task retry --run",
        hint_label: "agent-task retry --run",
    },
    CommandLabSupportSummary {
        contract_labels: &[AGENT_TASK_STATUS_LAB_LABEL, AGENT_TASK_PROVIDERS_LAB_LABEL],
        message_label: "agent-task run/status/logs/artifacts/review/list/active/providers",
        hint_label: "agent-task run/status/logs/artifacts/review/list/active/providers",
    },
    CommandLabSupportSummary {
        contract_labels: &[AGENT_TASK_FANOUT_COOK_BATCH_LAB_LABEL],
        message_label: AGENT_TASK_FANOUT_COOK_BATCH_LAB_LABEL,
        hint_label: AGENT_TASK_FANOUT_COOK_BATCH_LAB_LABEL,
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

const WORKLOAD_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[WORKLOAD_LAB_LABEL],
    message_label: "workload run",
    hint_label: "workload run",
}];

const TRACE_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[TRACE_LAB_LABEL],
    message_label: TRACE_LAB_LABEL,
    hint_label: TRACE_LAB_LABEL,
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
const WORKLOAD_DANGEROUS_FLAGS: &[&str] = &["--allow-destructive"];
const CLEANUP_DANGEROUS_FLAGS: &[&str] = &["--apply"];
const REFACTOR_DANGEROUS_FLAGS: &[&str] = &["--write"];

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
const RIG_MANAGED_FILE_PATHS: &[&str] = &["app install", "app update", "app uninstall"];

const DEPS_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[paths_safety(
    DEPS_MUTATING_PATHS,
    mutating_safety(),
    "mutates dependency manifests, lockfiles, or installed dependency trees",
)];

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

const WORKLOAD_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        &["run", "replay", "minimize"],
        guarded_safety(WORKLOAD_DANGEROUS_FLAGS),
        "executes an extension-declared workload kind and persists run evidence; a kind declared safety_class destructive is refused unless --allow-destructive is passed with an explicit homeboy/isolation-proof/v1",
    ),
    paths_safety(
        &["list", "compare", "inspect"],
        guarded_safety(&[]),
        "reads declared workloads or persisted run evidence",
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
    paths_safety(
        &["reclaim"],
        operator_safety(None, &["--apply"]),
        "default output is a non-mutating reclaim plan naming claimed workspaces, released stale owner leases, and reclaimable bytes; pass --apply to remove reclaimable completed-loop workspaces (every removal is reaping-audited)",
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
        &["connect"],
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
        &["patch preserve", "patch restore"],
        mutating_safety(),
        "captures and cleans or restores exact worktree changes using operation-owned evidence",
    ),
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

const REFACTOR_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[paths_safety(
    &["rename"],
    guarded_mutating_safety(REFACTOR_DANGEROUS_FLAGS),
    "reports a plan by default; pass --write to rewrite source files",
)];

const AGENT_TASK_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        &["recover"],
        with_dry_run(mutating_safety(), "--preflight"),
        "performs the stopped Cook's one recovery step; with --authorize-external-proof and corrected gates it executes operator-supplied deterministic shell gates and records durable replacement proof; --preflight only reports the step",
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
        operator_safety(Some("--preview"), &["--run-plan"]),
        "admits a workflow instance that cooks every child when --run-plan is passed",
    ),
];

const RUNS_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[
    paths_safety(
        &["report failure-digest"],
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

const CLEANUP_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[paths_safety(
    &["artifacts"],
    guarded_mutating_safety(CLEANUP_DANGEROUS_FLAGS),
    "default output is a non-mutating cleanup plan; pass --apply to remove artifacts",
)];

const UPGRADE_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[paths_safety(
    &["status"],
    CommandSafetySpec::read_only(),
    "inspects a persisted upgrade operation without mutating the controller",
)];

const SELF_SUBCOMMAND_SAFETY: &[CommandPathSafetySpec] = &[paths_safety(
    &["cleanup-runtime-tmp"],
    operator_safety(None, &["--apply"]),
    "default output is a non-mutating cleanup plan; pass --apply to delete runtime temp entries",
)];

macro_rules! declare_command_specs {
    ($(($variant:ident, $handler:path, $value:expr),)*) => {
        pub const COMMAND_SPECS: &[CommandSpec] = &[$($value,)*];
    };
}

crate::builtin_json_command_descriptors!(declare_command_specs);

pub const COMMAND_DOC_REGISTRY: &[CommandDocSpec] = &[
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

pub(crate) fn runtime_extension_command_doc_slugs() -> impl Iterator<Item = &'static str> {
    COMMAND_DOC_REGISTRY
        .iter()
        .filter(|entry| entry.kind == CommandDocKind::RuntimeExtensionCommand)
        .map(|entry| entry.slug)
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
    fn recover_is_declared_mutating_shell_execution_with_preflight() {
        let agent_task = registered_command("agent-task").expect("agent-task command spec");
        let safety = agent_task
            .path_safety(&["recover"])
            .expect("recover safety");

        assert!(safety.safety.mutates);
        assert_eq!(safety.safety.dry_run_flag, Some("--preflight"));
        assert!(safety
            .output_notes
            .is_some_and(|notes| notes.contains("shell gates")));
    }
}
