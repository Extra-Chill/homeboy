//! Declarative built-in JSON command registrations.
//!
//! Literal module declarations and the public Clap enum stay outside this table:
//! rustfmt cannot discover modules emitted by `macro_rules!`, and Clap needs its
//! user-facing attributes at the enum variants. Registry and JSON-dispatch parity
//! tests fail closed when either shape drifts from these descriptors.

#[macro_export]
macro_rules! builtin_json_command_descriptors {
    ($consumer:ident) => {
        $consumer! {
            (Activity, $crate::commands::activity::run, CommandSpec { output_notes: "unified active/recent activity read model in the standard JSON envelope", lab_supported: false, lab_notes: "read-only local activity query; never offloaded because it inspects operator-local stores", ..command_spec("activity", CommandJsonFamily::Workspace) }),
            (AgentTask, $crate::commands::agent_task::run, CommandSpec { subcommand_safety: AGENT_TASK_SUBCOMMAND_SAFETY, ..command_spec_with_representative_argv(&["homeboy", "agent-task", "providers"], lab_command_spec_with_summary("agent-task", CommandJsonFamily::Workspace, "Lab runner routing covers portable, explicit-runner, and runner-resident agent-task workflows", AGENT_TASK_LAB_SUPPORT)) }),
            (Project, $crate::commands::project::run, CommandSpec { subcommand_safety: PROJECT_SUBCOMMAND_SAFETY, ..command_spec("project", CommandJsonFamily::Workspace) }),
            (Ssh, $crate::commands::ssh::run, command_spec("ssh", CommandJsonFamily::Ops)),
            (Server, $crate::commands::server::run, CommandSpec { subcommand_safety: SERVER_SUBCOMMAND_SAFETY, ..command_spec("server", CommandJsonFamily::Ops) }),
            (Bench, $crate::commands::bench::run, command_spec_with_representative_argv(&["homeboy", "bench"], lab_command_spec_with_summary("bench", CommandJsonFamily::Quality, "portable Lab offload is available for benchmark runs", BENCH_LAB_SUPPORT))),
            (Fuzz, $crate::commands::fuzz::run, CommandSpec { safety: guarded_safety(FUZZ_DANGEROUS_FLAGS), subcommand_safety: FUZZ_SUBCOMMAND_SAFETY, ..command_spec_with_representative_argv(&["homeboy", "fuzz"], lab_command_spec_with_summary("fuzz", CommandJsonFamily::Quality, "fuzz is measurement-only by default; --allow-destructive infers isolated mode and attaches an auditable homeboy/isolation-proof/v1 unless one is supplied", FUZZ_LAB_SUPPORT)) }),
            (Trace, $crate::commands::trace::run, CommandSpec { safety: mutating_safety(), ..command_spec_with_representative_argv(&["homeboy", "trace"], lab_command_spec_with_output_notes_and_summary("trace", CommandJsonFamily::Quality, "portable Lab offload is available for trace runs", "runs trace workflows and records observation artifacts unless using read-only subcommands", TRACE_LAB_SUPPORT)) }),
            (Db, $crate::commands::db::run, CommandSpec { subcommand_safety: DB_SUBCOMMAND_SAFETY, ..command_spec("db", CommandJsonFamily::Ops) }),
            (Deps, $crate::cli_surface::DepsArgs::run, CommandSpec { subcommand_safety: DEPS_SUBCOMMAND_SAFETY, ..command_spec("deps", CommandJsonFamily::Ops) }),
            (File, $crate::commands::file::run, CommandSpec { subcommand_safety: FILE_SUBCOMMAND_SAFETY, ..command_spec("file", CommandJsonFamily::Ops) }),
            (Fleet, $crate::commands::fleet::run, CommandSpec { subcommand_safety: FLEET_SUBCOMMAND_SAFETY, ..command_spec("fleet", CommandJsonFamily::Ops) }),
            (Logs, $crate::commands::logs::run, command_spec("logs", CommandJsonFamily::Ops)),
            (Deploy, $crate::commands::deploy::run, command_spec_with_safety("deploy", CommandJsonFamily::Ops, operator_safety(Some("--dry-run"), DEPLOY_DANGEROUS_FLAGS))),
            (Harvest, $crate::commands::harvest::run, command_spec_with_safety("harvest", CommandJsonFamily::Ops, operator_safety(Some("--dry-run"), &["--apply"]))),
            (Component, $crate::commands::component::run, CommandSpec { subcommand_safety: COMPONENT_SUBCOMMAND_SAFETY, ..command_spec("component", CommandJsonFamily::Workspace) }),
            (Config, $crate::commands::config::run, CommandSpec { subcommand_safety: CONFIG_SUBCOMMAND_SAFETY, ..command_spec("config", CommandJsonFamily::Workspace) }),
            (Contract, $crate::commands::contract::run, command_spec_with_output_notes("contract", CommandJsonFamily::Workspace, "lists, shows, exports constants, exports schemas, validates, normalizes, and emits Homeboy-owned contract metadata and command manifests through the central contract surface")),
            (Daemon, $crate::commands::daemon::run, command_spec("daemon", CommandJsonFamily::Ops)),
            (DeferredWorkload, $crate::commands::deferred_workload::run, command_spec("deferred-workload", CommandJsonFamily::Ops)),
            (Schedule, $crate::commands::schedule::run, command_spec("schedule", CommandJsonFamily::Ops)),
            (Extension, $crate::commands::extension::run, CommandSpec { subcommand_safety: EXTENSION_SUBCOMMAND_SAFETY, ..command_spec_with_representative_argv(&["homeboy", "extension", "refresh", "."], lab_command_spec_with_summary("extension", CommandJsonFamily::Workspace, "Lab runner routing covers runner extension refresh/update/dev-run workflows", EXTENSION_LAB_SUPPORT)) }),
            (Status, $crate::commands::status::run, command_spec("status", CommandJsonFamily::Ops)),
            (Cleanup, $crate::commands::json_output::cleanup_run_auto, CommandSpec { subcommand_safety: CLEANUP_SUBCOMMAND_SAFETY, ..command_spec_with_output_notes_and_safety("cleanup", CommandJsonFamily::Workspace, "cleanup subcommands report plans by default and require --apply for removals", CommandSafetySpec { mutates: true, operator: false, dry_run_flag: None, risk_exemption: None, dangerous_flags: CLEANUP_DANGEROUS_FLAGS }) }),
            (Git, $crate::commands::git::run, CommandSpec { subcommand_safety: GIT_SUBCOMMAND_SAFETY, ..command_spec("git", CommandJsonFamily::Ops) }),
            (Release, $crate::commands::release::run, command_spec_with_output_notes_and_safety("release", CommandJsonFamily::Workspace, "release execution mutates git tags/releases and may deploy; use --dry-run to plan and --apply for risky modes", operator_safety(Some("--dry-run"), RELEASE_DANGEROUS_FLAGS))),
            (Review, $crate::commands::review::run, CommandSpec { subcommand_safety: REVIEW_SUBCOMMAND_SAFETY, ..command_spec_with_representative_argv(&["homeboy", "review"], lab_command_spec_with_summary("review", CommandJsonFamily::Quality, "portable Lab offload is available for release-gate review runs", REVIEW_LAB_SUPPORT)) }),
            (Refactor, $crate::commands::refactor::run, CommandSpec { safety: CommandSafetySpec { mutates: true, operator: false, dry_run_flag: None, risk_exemption: None, dangerous_flags: REFACTOR_DANGEROUS_FLAGS }, subcommand_safety: REFACTOR_SUBCOMMAND_SAFETY, ..command_spec_with_representative_argv(&["homeboy", "refactor", "--all"], lab_command_spec_with_output_notes_and_summary("refactor", CommandJsonFamily::Workspace, "portable Lab offload is available for refactor source runs", "refactor subcommands can rewrite source files, inspect references, or restore undo snapshots; use planning/dry-run modes where available", REFACTOR_LAB_SUPPORT)) }),
            (Rig, $crate::commands::rig::run, CommandSpec { subcommand_safety: RIG_SUBCOMMAND_SAFETY, ..command_spec_with_representative_argv(&["homeboy", "rig", "check", "example-rig"], lab_command_spec_with_summary("rig", CommandJsonFamily::Workspace, "portable Lab offload is available for rig check workflows", RIG_LAB_SUPPORT)) }),
            (Runner, $crate::commands::runner::run, CommandSpec { subcommand_safety: RUNNER_SUBCOMMAND_SAFETY, ..command_spec("runner", CommandJsonFamily::Workspace) }),
            (Source, $crate::commands::source::run, command_spec_with_output_notes("source", CommandJsonFamily::Ops, "read-only sealed source-package admissibility check; creates no Homeboy resources")),
            (Runtime, $crate::commands::runtime::run, CommandSpec { subcommand_safety: RUNTIME_SUBCOMMAND_SAFETY, ..command_spec_with_representative_argv(&["homeboy", "runtime", "refresh", "example-runtime", "--source", "."], lab_command_spec_with_summary("runtime", CommandJsonFamily::Workspace, "Lab runner routing covers runtime package refresh workflows", RUNTIME_LAB_SUPPORT)) }),
            (Worktree, $crate::commands::worktree::run, CommandSpec { subcommand_safety: WORKTREE_SUBCOMMAND_SAFETY, ..command_spec_with_representative_argv(&["homeboy", "worktree", "cleanup"], lab_command_spec_with_summary("worktree", CommandJsonFamily::Workspace, "Lab runner routing covers runner-resident task worktree cleanup", WORKTREE_LAB_SUPPORT)) }),
            (Tunnel, $crate::commands::tunnel::run, CommandSpec { subcommand_safety: TUNNEL_SUBCOMMAND_SAFETY, ..command_spec_with_representative_argv(&["homeboy", "tunnel", "service", "start", "example-service", "--command", "npm start"], lab_command_spec_with_summary("tunnel", CommandJsonFamily::Workspace, "Lab runner routing covers tunnel preview and service workflows", TUNNEL_LAB_SUPPORT)) }),
            (Runs, $crate::commands::runs::run, CommandSpec { subcommand_safety: RUNS_SUBCOMMAND_SAFETY, ..command_spec_with_output_notes("runs", CommandJsonFamily::Workspace, "inspects persisted evidence, artifacts, typed report projections, artifact postprocessing, and finding reconciliation workflows") }),
            (SelfCmd, $crate::commands::self_cmd::run, CommandSpec { subcommand_safety: SELF_SUBCOMMAND_SAFETY, ..command_spec_with_output_notes("self", CommandJsonFamily::Ops, "inspects the active Homeboy runtime and renders built-in CLI documentation") }),
            (Stack, $crate::commands::stack::run, CommandSpec { subcommand_safety: STACK_SUBCOMMAND_SAFETY, ..command_spec("stack", CommandJsonFamily::Workspace) }),
            (Api, $crate::commands::api::run, CommandSpec { subcommand_safety: API_SUBCOMMAND_SAFETY, ..command_spec("api", CommandJsonFamily::Ops) }),
            (Upgrade, $crate::commands::upgrade::run, CommandSpec { subcommand_safety: UPGRADE_SUBCOMMAND_SAFETY, ..command_spec_with_output_notes_and_safety("upgrade", CommandJsonFamily::Ops, "upgrades the active Homeboy binary, extensions, runners, and services unless --check or skip flags are used", operator_safety(None, UPGRADE_DANGEROUS_FLAGS)) }),
        }
    };
}
