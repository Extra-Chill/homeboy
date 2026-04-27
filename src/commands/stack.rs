//! `homeboy stack` — combined-fixes branches as a first-class primitive.
//!
//! Manages stack specs at `~/.config/homeboy/stacks/{id}.json` plus the
//! spec-less `inspect` introspection layer (formerly `homeboy git stack`).

use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::stack::{
    self, ApplyOutput, GitRef, InspectOptions, InspectOutput, PushOutput, RebaseOutput,
    StackPrEntry, StackSpec, StatusOutput, SyncOutput,
};

use super::CmdResult;

#[derive(Args)]
pub struct StackArgs {
    #[command(subcommand)]
    command: StackCommand,
}

#[derive(Subcommand)]
enum StackCommand {
    /// List all installed stack specs.
    List,
    /// Show a stack spec.
    Show {
        /// Stack ID.
        stack_id: String,
    },
    /// Create a new stack spec.
    Create {
        /// Stack ID (used as filename: `~/.config/homeboy/stacks/<id>.json`).
        stack_id: String,
        /// Component identifier (informational; future: rig binding key).
        #[arg(long)]
        component: String,
        /// Local checkout path. Supports `~` and `${env.VAR}` expansion.
        #[arg(long)]
        component_path: String,
        /// Upstream ref to rebuild from, as `<remote>/<branch>`
        /// (e.g. `origin/trunk`).
        #[arg(long)]
        base: String,
        /// Target combined-fixes branch as `<remote>/<branch>`
        /// (e.g. `fork/dev/combined-fixes`).
        #[arg(long)]
        target: String,
        /// Optional human-readable description.
        #[arg(long, default_value_t = String::new())]
        description: String,
    },
    /// Append a PR entry to the stack's `prs` array.
    AddPr {
        /// Stack ID.
        stack_id: String,
        /// `<owner>/<repo>` coordinate (e.g. `Automattic/studio`).
        repo: String,
        /// PR number.
        number: u64,
        /// Optional human-readable note.
        #[arg(long)]
        note: Option<String>,
    },
    /// Remove a PR entry from the stack's `prs` array.
    RemovePr {
        /// Stack ID.
        stack_id: String,
        /// PR number to remove. If multiple entries match (different repos),
        /// pass `--repo` to disambiguate.
        number: u64,
        /// Restrict removal to this `<owner>/<repo>` (when the same PR
        /// number appears in multiple repos in the stack).
        #[arg(long)]
        repo: Option<String>,
    },
    /// Materialize a stack: cherry-pick `base + prs` onto `target`.
    ///
    /// Stops on the first cherry-pick conflict and prints a manual-resolve
    /// message. (Phase 2 will add `--continue`.)
    Apply {
        /// Stack ID.
        stack_id: String,
    },
    /// Rebuild the target branch from fresh base + current spec PRs.
    ///
    /// Unlike `sync`, `rebase` never edits the stack spec: merged PRs stay
    /// listed until an explicit `sync` or `remove-pr`.
    Rebase {
        /// Stack ID.
        stack_id: String,
    },
    /// Read-only status report — upstream PR state + local target state.
    Status {
        /// Stack ID.
        stack_id: String,
    },
    /// Rebase the target branch onto fresh base AND auto-drop merged PRs
    /// from the spec.
    ///
    /// `sync` is the holistic upkeep verb for a combined-fixes branch:
    /// PRs that have been merged upstream (and whose content is in base)
    /// are removed from the spec; everything else is cherry-picked onto
    /// a freshly-rebuilt target. On the first cherry-pick conflict, the
    /// in-progress pick is aborted and a manual-resolve message printed.
    Sync {
        /// Stack ID.
        stack_id: String,
        /// Print what WOULD drop and pick without mutating spec or
        /// target branch.
        #[arg(long)]
        dry_run: bool,
    },
    /// Push the materialized target branch to its configured remote.
    Push {
        /// Stack ID.
        stack_id: String,
    },
    /// Spec-less inspection of the current branch as a stack of commits.
    /// Replaces the previous `homeboy git stack` command (re-homed into
    /// the stack domain).
    Inspect {
        /// Component ID. When omitted, auto-detected from CWD.
        component_id: Option<String>,
        /// Base ref to compare against. Defaults to `@{upstream}` of the
        /// current branch.
        #[arg(long, value_name = "REF")]
        base: Option<String>,
        /// Skip the GitHub PR lookup pass.
        #[arg(long)]
        no_pr: bool,
        /// Scope PR lookups to a specific GitHub repo (`owner/name`).
        #[arg(long, value_name = "OWNER/NAME")]
        repo: Option<String>,
        /// Workspace path to operate on directly.
        #[arg(long, value_name = "PATH")]
        path: Option<String>,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum StackCommandOutput {
    List(StackListOutput),
    Show(StackShowOutput),
    Create(StackMutationOutput),
    AddPr(StackMutationOutput),
    RemovePr(StackMutationOutput),
    Apply(StackApplyOutput),
    Rebase(StackRebaseOutput),
    Status(StackStatusOutput),
    Sync(StackSyncOutput),
    Push(StackPushOutput),
    Inspect(StackInspectOutput),
}

#[derive(Serialize)]
pub struct StackListOutput {
    pub command: &'static str,
    pub stacks: Vec<StackSummary>,
}

#[derive(Serialize)]
pub struct StackSummary {
    pub id: String,
    pub description: String,
    pub component: String,
    pub component_path: String,
    pub base: String,
    pub target: String,
    pub pr_count: usize,
}

#[derive(Serialize)]
pub struct StackShowOutput {
    pub command: &'static str,
    pub stack: StackSpec,
}

#[derive(Serialize)]
pub struct StackMutationOutput {
    pub command: &'static str,
    pub stack: StackSpec,
}

#[derive(Serialize)]
pub struct StackApplyOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: ApplyOutput,
}

#[derive(Serialize)]
pub struct StackRebaseOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: RebaseOutput,
}

#[derive(Serialize)]
pub struct StackStatusOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: StatusOutput,
}

#[derive(Serialize)]
pub struct StackSyncOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: SyncOutput,
}

#[derive(Serialize)]
pub struct StackPushOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: PushOutput,
}

#[derive(Serialize)]
pub struct StackInspectOutput {
    pub command: &'static str,
    #[serde(flatten)]
    pub report: InspectOutput,
}

pub fn run(args: StackArgs, _global: &super::GlobalArgs) -> CmdResult<StackCommandOutput> {
    match args.command {
        StackCommand::List => list(),
        StackCommand::Show { stack_id } => show(&stack_id),
        StackCommand::Create {
            stack_id,
            component,
            component_path,
            base,
            target,
            description,
        } => create(
            &stack_id,
            &component,
            &component_path,
            &base,
            &target,
            &description,
        ),
        StackCommand::AddPr {
            stack_id,
            repo,
            number,
            note,
        } => add_pr(&stack_id, &repo, number, note),
        StackCommand::RemovePr {
            stack_id,
            number,
            repo,
        } => remove_pr(&stack_id, number, repo.as_deref()),
        StackCommand::Apply { stack_id } => apply(&stack_id),
        StackCommand::Rebase { stack_id } => rebase(&stack_id),
        StackCommand::Status { stack_id } => status(&stack_id),
        StackCommand::Sync { stack_id, dry_run } => sync(&stack_id, dry_run),
        StackCommand::Push { stack_id } => push(&stack_id),
        StackCommand::Inspect {
            component_id,
            base,
            no_pr,
            repo,
            path,
        } => inspect(component_id.as_deref(), base, no_pr, repo, path.as_deref()),
    }
}

fn list() -> CmdResult<StackCommandOutput> {
    let stacks = stack::list()?;
    let summaries = stacks
        .into_iter()
        .map(|s| StackSummary {
            id: s.id,
            description: s.description,
            component: s.component,
            component_path: s.component_path,
            base: s.base.display(),
            target: s.target.display(),
            pr_count: s.prs.len(),
        })
        .collect();
    Ok((
        StackCommandOutput::List(StackListOutput {
            command: "stack.list",
            stacks: summaries,
        }),
        0,
    ))
}

fn show(stack_id: &str) -> CmdResult<StackCommandOutput> {
    let stack = stack::load(stack_id)?;
    Ok((
        StackCommandOutput::Show(StackShowOutput {
            command: "stack.show",
            stack,
        }),
        0,
    ))
}

fn create(
    stack_id: &str,
    component: &str,
    component_path: &str,
    base: &str,
    target: &str,
    description: &str,
) -> CmdResult<StackCommandOutput> {
    // Refuse to silently overwrite an existing stack — `add-pr`/`remove-pr`
    // are the right verbs for editing a live spec.
    if stack::exists(stack_id)? {
        return Err(homeboy::Error::validation_invalid_argument(
            "stack_id",
            format!("Stack '{}' already exists", stack_id),
            None,
            Some(vec![format!(
                "Edit it instead: homeboy stack add-pr {} <repo> <number>",
                stack_id
            )]),
        ));
    }

    let base_ref: GitRef = stack::parse_git_ref(base, "base")?;
    let target_ref: GitRef = stack::parse_git_ref(target, "target")?;

    let spec = StackSpec {
        id: stack_id.to_string(),
        description: description.to_string(),
        component: component.to_string(),
        component_path: component_path.to_string(),
        base: base_ref,
        target: target_ref,
        prs: Vec::new(),
    };
    stack::save(&spec)?;
    Ok((
        StackCommandOutput::Create(StackMutationOutput {
            command: "stack.create",
            stack: spec,
        }),
        0,
    ))
}

fn add_pr(
    stack_id: &str,
    repo: &str,
    number: u64,
    note: Option<String>,
) -> CmdResult<StackCommandOutput> {
    let mut spec = stack::load(stack_id)?;

    // Refuse to silently double-add the same PR — keeps the spec tidy and
    // makes "did I already add this?" obvious.
    if spec
        .prs
        .iter()
        .any(|p| p.repo == repo && p.number == number)
    {
        return Err(homeboy::Error::validation_invalid_argument(
            "number",
            format!("PR {}#{} is already in stack '{}'", repo, number, stack_id),
            None,
            Some(vec![format!(
                "Remove first: homeboy stack remove-pr {} {} --repo {}",
                stack_id, number, repo
            )]),
        ));
    }

    spec.prs.push(StackPrEntry {
        repo: repo.to_string(),
        number,
        note,
    });
    stack::save(&spec)?;
    Ok((
        StackCommandOutput::AddPr(StackMutationOutput {
            command: "stack.add_pr",
            stack: spec,
        }),
        0,
    ))
}

fn remove_pr(stack_id: &str, number: u64, repo: Option<&str>) -> CmdResult<StackCommandOutput> {
    let mut spec = stack::load(stack_id)?;

    let matches: Vec<usize> = spec
        .prs
        .iter()
        .enumerate()
        .filter(|(_, p)| p.number == number && repo.map(|r| r == p.repo).unwrap_or(true))
        .map(|(i, _)| i)
        .collect();

    if matches.is_empty() {
        return Err(homeboy::Error::validation_invalid_argument(
            "number",
            format!("No PR {} in stack '{}'", number, stack_id),
            None,
            Some(vec![
                "Check current state: homeboy stack show <id>".to_string()
            ]),
        ));
    }
    if matches.len() > 1 {
        let repos: Vec<String> = matches.iter().map(|i| spec.prs[*i].repo.clone()).collect();
        return Err(homeboy::Error::validation_invalid_argument(
            "number",
            format!(
                "PR {} matches multiple entries in stack '{}' (repos: {}). Pass --repo to disambiguate.",
                number,
                stack_id,
                repos.join(", ")
            ),
            None,
            Some(vec![format!(
                "homeboy stack remove-pr {} {} --repo <owner/name>",
                stack_id, number
            )]),
        ));
    }

    spec.prs.remove(matches[0]);
    stack::save(&spec)?;
    Ok((
        StackCommandOutput::RemovePr(StackMutationOutput {
            command: "stack.remove_pr",
            stack: spec,
        }),
        0,
    ))
}

fn apply(stack_id: &str) -> CmdResult<StackCommandOutput> {
    let spec = stack::load(stack_id)?;
    let report = stack::apply(&spec)?;
    let exit_code = if report.success { 0 } else { 1 };
    Ok((
        StackCommandOutput::Apply(StackApplyOutput {
            command: "stack.apply",
            report,
        }),
        exit_code,
    ))
}

fn rebase(stack_id: &str) -> CmdResult<StackCommandOutput> {
    let spec = stack::load(stack_id)?;
    let report = stack::rebase(&spec)?;
    let exit_code = if report.success { 0 } else { 1 };
    Ok((
        StackCommandOutput::Rebase(StackRebaseOutput {
            command: "stack.rebase",
            report,
        }),
        exit_code,
    ))
}

fn status(stack_id: &str) -> CmdResult<StackCommandOutput> {
    let spec = stack::load(stack_id)?;
    let report = stack::status(&spec)?;
    Ok((
        StackCommandOutput::Status(StackStatusOutput {
            command: "stack.status",
            report,
        }),
        0,
    ))
}

fn sync(stack_id: &str, dry_run: bool) -> CmdResult<StackCommandOutput> {
    let mut spec = stack::load(stack_id)?;
    let report = stack::sync(&mut spec, dry_run)?;
    let exit_code = if report.success { 0 } else { 1 };
    Ok((
        StackCommandOutput::Sync(StackSyncOutput {
            command: "stack.sync",
            report,
        }),
        exit_code,
    ))
}

fn push(stack_id: &str) -> CmdResult<StackCommandOutput> {
    let spec = stack::load(stack_id)?;
    let report = stack::push(&spec)?;
    Ok((
        StackCommandOutput::Push(StackPushOutput {
            command: "stack.push",
            report,
        }),
        0,
    ))
}

fn inspect(
    component_id: Option<&str>,
    base: Option<String>,
    no_pr: bool,
    repo: Option<String>,
    path: Option<&str>,
) -> CmdResult<StackCommandOutput> {
    let report =
        stack::inspect::inspect_at(component_id, InspectOptions { base, no_pr, repo }, path)?;
    let exit_code = if report.success { 0 } else { 1 };
    Ok((
        StackCommandOutput::Inspect(StackInspectOutput {
            command: "stack.inspect",
            report,
        }),
        exit_code,
    ))
}
