//! Shared top-level command metadata.
//!
//! This is the first narrow `CommandSpec` slice: top-level metadata that is
//! consumed by output routing, safety/docs manifest derivation, and command
//! lookup without changing parsed CLI behavior.

use super::output::{
    CommandDispatchFamily, CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSpec {
    pub name: &'static str,
    pub json_family: CommandJsonFamily,
    pub docs_slug: Option<&'static str>,
    pub safety: CommandSafetySpec,
    pub output_notes: &'static str,
    pub lab_supported: bool,
    pub lab_notes: &'static str,
    pub lab_support_summary: &'static [CommandLabSupportSummary],
}

pub type CommandRegistryEntry = CommandSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandLabSupportSummary {
    pub contract_labels: &'static [&'static str],
    pub message_label: &'static str,
    pub hint_label: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandSafetySpec {
    pub mutates: bool,
    pub operator: bool,
    pub dry_run_flag: Option<&'static str>,
    pub risk_exemption: Option<&'static str>,
    pub dangerous_flags: &'static [&'static str],
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
pub(crate) const LINT_LAB_LABEL: &str = "lint";
pub(crate) const TEST_LAB_LABEL: &str = "test";
pub(crate) const AUDIT_LAB_LABEL: &str = "audit";
pub(crate) const REVIEW_LAB_LABEL: &str = "review";
pub(crate) const BENCH_LAB_LABEL: &str = "bench";
pub(crate) const FUZZ_LAB_LABEL: &str = "fuzz";
pub(crate) const TRACE_LAB_LABEL: &str = "trace";
pub(crate) const REFACTOR_LAB_LABEL: &str = "refactor";
pub(crate) const RIG_CHECK_LAB_LABEL: &str = "rig check";
pub(crate) const RUNTIME_REFRESH_LAB_LABEL: &str = "runtime refresh";
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

    pub fn dispatch_family(&self) -> CommandDispatchFamily {
        self.json_family.into()
    }
}

const fn command_spec(name: &'static str, json_family: CommandJsonFamily) -> CommandSpec {
    CommandSpec {
        name,
        json_family,
        docs_slug: Some(name),
        safety: CommandSafetySpec::read_only(),
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

const fn manifest_command_spec() -> CommandSpec {
    CommandSpec {
        output_notes:
            "recursive command safety, docs, output, and Lab metadata in the standard JSON envelope",
        ..command_spec("manifest", CommandJsonFamily::Workspace)
    }
}

const AGENT_TASK_LAB_SUPPORT: &[CommandLabSupportSummary] = &[
    CommandLabSupportSummary {
        contract_labels: &[AGENT_TASK_RUN_LAB_LABEL],
        message_label: "agent-task cook/run-plan",
        hint_label: "agent-task cook/run-plan",
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

const LINT_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[LINT_LAB_LABEL],
    message_label: LINT_LAB_LABEL,
    hint_label: LINT_LAB_LABEL,
}];

const TEST_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[TEST_LAB_LABEL],
    message_label: TEST_LAB_LABEL,
    hint_label: TEST_LAB_LABEL,
}];

const AUDIT_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[AUDIT_LAB_LABEL],
    message_label: AUDIT_LAB_LABEL,
    hint_label: AUDIT_LAB_LABEL,
}];

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
    contract_labels: &[FUZZ_LAB_LABEL],
    message_label: FUZZ_LAB_LABEL,
    hint_label: "fuzz run",
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

const RIG_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[RIG_CHECK_LAB_LABEL],
    message_label: RIG_CHECK_LAB_LABEL,
    hint_label: RIG_CHECK_LAB_LABEL,
}];

const RUNTIME_LAB_SUPPORT: &[CommandLabSupportSummary] = &[CommandLabSupportSummary {
    contract_labels: &[RUNTIME_REFRESH_LAB_LABEL],
    message_label: RUNTIME_REFRESH_LAB_LABEL,
    hint_label: RUNTIME_REFRESH_LAB_LABEL,
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
const LINT_DANGEROUS_FLAGS: &[&str] = &["--fix", "--force"];
const FUZZ_DANGEROUS_FLAGS: &[&str] = &["--allow-destructive"];
const CLEANUP_DANGEROUS_FLAGS: &[&str] = &["--apply"];
const TRIAGE_DANGEROUS_FLAGS: &[&str] = &["--auto-merge"];
const REFACTOR_DANGEROUS_FLAGS: &[&str] = &["--write", "--commit"];

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

pub const COMMAND_SPECS: &[CommandSpec] = &[
    lab_command_spec_with_summary(
        "agent-task",
        CommandJsonFamily::Workspace,
        "Lab runner routing covers portable, explicit-runner, and runner-resident agent-task workflows",
        AGENT_TASK_LAB_SUPPORT,
    ),
    command_spec("project", CommandJsonFamily::Workspace),
    command_spec("ssh", CommandJsonFamily::Ops),
    command_spec("server", CommandJsonFamily::Ops),
    lab_command_spec_with_summary(
        "test",
        CommandJsonFamily::Quality,
        "portable Lab offload is available for test runs",
        TEST_LAB_SUPPORT,
    ),
    lab_command_spec_with_summary(
        "bench",
        CommandJsonFamily::Quality,
        "portable Lab offload is available for benchmark runs",
        BENCH_LAB_SUPPORT,
    ),
    CommandSpec {
        safety: guarded_safety(FUZZ_DANGEROUS_FLAGS),
        ..lab_command_spec_with_summary(
            "fuzz",
            CommandJsonFamily::Quality,
            "fuzz is measurement-only by default; --allow-destructive requires explicit disposable homeboy/isolation-proof/v1 input",
            FUZZ_LAB_SUPPORT,
        )
    },
    CommandSpec {
        safety: mutating_safety(),
        ..lab_command_spec_with_output_notes_and_summary(
            "trace",
            CommandJsonFamily::Quality,
            "portable Lab offload is available for trace runs",
            "runs trace workflows and records observation artifacts unless using read-only subcommands",
            TRACE_LAB_SUPPORT,
        )
    },
    command_spec("observe", CommandJsonFamily::Quality),
    CommandSpec {
        safety: guarded_safety(LINT_DANGEROUS_FLAGS),
        ..lab_command_spec_with_output_notes_and_summary(
            "lint",
            CommandJsonFamily::Quality,
            "portable Lab offload is available for changed-scope lint runs",
            "runs lint workflows; pass --fix to apply auto-fixable findings in place",
            LINT_LAB_SUPPORT,
        )
    },
    command_spec("db", CommandJsonFamily::Ops),
    command_spec("deps", CommandJsonFamily::Ops),
    command_spec("ci", CommandJsonFamily::Ops),
    command_spec("file", CommandJsonFamily::Ops),
    command_spec("fleet", CommandJsonFamily::Ops),
    command_spec("logs", CommandJsonFamily::Ops),
    command_spec_with_safety(
        "triage",
        CommandJsonFamily::Ops,
        operator_safety(None, TRIAGE_DANGEROUS_FLAGS),
    ),
    command_spec_with_safety(
        "deploy",
        CommandJsonFamily::Ops,
        operator_safety(Some("--dry-run"), DEPLOY_DANGEROUS_FLAGS),
    ),
    command_spec("component", CommandJsonFamily::Workspace),
    command_spec("config", CommandJsonFamily::Workspace),
    command_spec_with_output_notes(
        "contract",
        CommandJsonFamily::Workspace,
        "exports stable machine-consumable contract JSON files for downstream non-Rust consumers",
    ),
    command_spec("daemon", CommandJsonFamily::Ops),
    command_spec("extension", CommandJsonFamily::Workspace),
    command_spec("status", CommandJsonFamily::Ops),
    command_spec("docs", CommandJsonFamily::Workspace),
    manifest_command_spec(),
    command_spec("changelog", CommandJsonFamily::Workspace),
    command_spec_with_output_notes_and_safety(
        "cleanup",
        CommandJsonFamily::Workspace,
        "cleanup subcommands report plans by default and require --apply for removals",
        CommandSafetySpec {
            mutates: true,
            operator: false,
            dry_run_flag: None,
            risk_exemption: None,
            dangerous_flags: CLEANUP_DANGEROUS_FLAGS,
        },
    ),
    command_spec("git", CommandJsonFamily::Ops),
    command_spec("issues", CommandJsonFamily::Ops),
    command_spec("version", CommandJsonFamily::Workspace),
    command_spec("build", CommandJsonFamily::Workspace),
    command_spec("changes", CommandJsonFamily::Workspace),
    command_spec_with_output_notes_and_safety(
        "release",
        CommandJsonFamily::Workspace,
        "release execution mutates git tags/releases and may deploy; use --dry-run to plan and --apply for risky modes",
        operator_safety(Some("--dry-run"), RELEASE_DANGEROUS_FLAGS),
    ),
    command_spec("report", CommandJsonFamily::Workspace),
    lab_command_spec_with_summary(
        "review",
        CommandJsonFamily::Quality,
        "portable Lab offload is available for release-gate review runs",
        REVIEW_LAB_SUPPORT,
    ),
    lab_command_spec_with_summary(
        "audit",
        CommandJsonFamily::Quality,
        "portable Lab offload is available for audit source runs",
        AUDIT_LAB_SUPPORT,
    ),
    command_spec("audit-baseline", CommandJsonFamily::Quality),
    CommandSpec {
        safety: CommandSafetySpec {
            mutates: true,
            operator: false,
            dry_run_flag: None,
            risk_exemption: None,
            dangerous_flags: REFACTOR_DANGEROUS_FLAGS,
        },
        ..lab_command_spec_with_output_notes_and_summary(
            "refactor",
            CommandJsonFamily::Workspace,
            "portable Lab offload is available for refactor source runs",
            "refactor subcommands can rewrite source files; use planning/dry-run modes where available",
            REFACTOR_LAB_SUPPORT,
        )
    },
    command_spec("refs", CommandJsonFamily::Workspace),
    lab_command_spec_with_summary(
        "rig",
        CommandJsonFamily::Workspace,
        "portable Lab offload is available for rig check workflows",
        RIG_LAB_SUPPORT,
    ),
    command_spec("runner", CommandJsonFamily::Workspace),
    lab_command_spec_with_summary(
        "runtime",
        CommandJsonFamily::Workspace,
        "Lab runner routing covers runtime package refresh workflows",
        RUNTIME_LAB_SUPPORT,
    ),
    command_spec("worktree", CommandJsonFamily::Workspace),
    lab_command_spec_with_summary(
        "tunnel",
        CommandJsonFamily::Workspace,
        "Lab runner routing covers tunnel preview and service workflows",
        TUNNEL_LAB_SUPPORT,
    ),
    command_spec("runs", CommandJsonFamily::Workspace),
    command_spec("self", CommandJsonFamily::Ops),
    command_spec("stack", CommandJsonFamily::Workspace),
    command_spec_with_output_notes_and_safety(
        "undo",
        CommandJsonFamily::Workspace,
        "restores files from the latest or selected undo snapshot",
        mutating_safety(),
    ),
    command_spec("auth", CommandJsonFamily::Ops),
    command_spec("api", CommandJsonFamily::Ops),
    command_spec("http", CommandJsonFamily::Ops),
    command_spec_with_output_notes_and_safety(
        "upgrade",
        CommandJsonFamily::Ops,
        "upgrades the active Homeboy binary, extensions, runners, and services unless --check or skip flags are used",
        operator_safety(None, UPGRADE_DANGEROUS_FLAGS),
    ),
];

pub const COMMAND_REGISTRY: &[CommandRegistryEntry] = COMMAND_SPECS;

pub fn registered_command(name: &str) -> Option<&'static CommandSpec> {
    COMMAND_SPECS.iter().find(|entry| entry.name == name)
}

pub fn registered_command_json_family(name: &str) -> Option<CommandJsonFamily> {
    registered_command(name).map(|entry| entry.json_family)
}

pub fn registered_command_dispatch_family(name: &str) -> Option<CommandDispatchFamily> {
    registered_command(name).map(CommandSpec::dispatch_family)
}
