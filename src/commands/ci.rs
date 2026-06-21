use clap::{Args, Subcommand};
use serde::Serialize;
use std::path::PathBuf;

use homeboy::core::ci_profile::{self, CiInventory, CiRunOutput, CiRunSelection};
use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::refactor::auto::transaction::{
    self, CiContext, TransactionOutcome, TransactionRequest, AUTOFIX_COMMIT_PREFIX,
};

use super::utils::args::{ExtensionOverrideArgs, PositionalComponentArgs};
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct CiArgs {
    #[command(subcommand)]
    pub command: CiCommand,
}

#[derive(Subcommand)]
pub enum CiCommand {
    /// List declared CI profiles and shallow discovered CI surfaces.
    List(CiListArgs),
    /// Run an extension-declared CI job or profile locally.
    Run(CiRunArgs),
    /// Run the end-to-end CI autofix transaction (branch prep, drift-only
    /// filtering, push-target resolution, commit, and push).
    ///
    /// This is the core-owned transaction the action calls instead of
    /// re-implementing branch/commit/push orchestration in shell. It assumes
    /// the working tree already contains the autofix changes to commit.
    Autofix(CiAutofixArgs),
}

#[derive(Args)]
pub struct CiAutofixArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    #[command(flatten)]
    pub extension_override: ExtensionOverrideArgs,

    /// Target repository to push to (`owner/repo`). Defaults to `origin`.
    #[arg(long)]
    pub target_repo: Option<String>,

    /// Repository backing the current `origin` remote (`owner/repo`).
    #[arg(long)]
    pub origin_repo: Option<String>,

    /// Branch to push to (PR head branch or autofix branch).
    #[arg(long)]
    pub target_branch: Option<String>,

    /// GitHub App / access token for the push (enables workflow re-runs and
    /// cross-repo pushes). Falls back to the `APP_TOKEN` env var.
    #[arg(long)]
    pub token: Option<String>,

    /// Git identity to commit as. Defaults to the CI bot identity.
    #[arg(long)]
    pub git_identity: Option<String>,

    /// Commit message for authored (non-drift) fixes. Defaults to a generic
    /// autofix subject.
    #[arg(long)]
    pub message: Option<String>,

    /// Classify and resolve the push target without committing or pushing.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Args)]
pub struct CiListArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    #[command(flatten)]
    pub extension_override: ExtensionOverrideArgs,
}

#[derive(Args)]
pub struct CiRunArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    #[command(flatten)]
    pub extension_override: ExtensionOverrideArgs,

    /// Run a single extension-declared CI job.
    #[arg(long, conflicts_with = "profile")]
    pub job: Option<String>,

    /// Run all jobs in an extension-declared CI profile.
    #[arg(long, conflicts_with = "job")]
    pub profile: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CiListOutput {
    pub command: &'static str,
    pub component_id: String,
    pub source_path: PathBuf,
    pub inventory: CiInventory,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum CiOutput {
    List(CiListOutput),
    Run(CiRunCommandOutput),
    Autofix(CiAutofixCommandOutput),
}

#[derive(Debug, Serialize)]
pub struct CiAutofixCommandOutput {
    pub command: &'static str,
    pub component_id: String,
    pub source_path: PathBuf,
    pub push_target: String,
    #[serde(flatten)]
    pub outcome: TransactionOutcome,
}

#[derive(Debug, Serialize)]
pub struct CiRunCommandOutput {
    pub command: &'static str,
    pub component_id: String,
    pub source_path: PathBuf,
    #[serde(flatten)]
    pub run: CiRunOutput,
}

pub fn run(args: CiArgs, global: &GlobalArgs) -> CmdResult<CiOutput> {
    match args.command {
        CiCommand::List(args) => run_list(args, global),
        CiCommand::Run(args) => run_ci(args, global),
        CiCommand::Autofix(args) => run_autofix(args, global),
    }
}

fn run_list(args: CiListArgs, _global: &GlobalArgs) -> CmdResult<CiOutput> {
    let ctx = execution_context::resolve(&ResolveOptions {
        component_id: args.comp.component.clone(),
        path_override: args.comp.path.clone(),
        capability: None,
        settings_overrides: Vec::new(),
        settings_json_overrides: Vec::new(),
        extension_overrides: args.extension_override.extensions.clone(),
    })?;
    let extension_ids = ctx
        .component
        .extensions
        .as_ref()
        .map(|extensions| {
            let mut ids: Vec<String> = extensions.keys().cloned().collect();
            ids.sort();
            ids
        })
        .unwrap_or_default();
    let extension_id = ci_profile::select_extension_id(&extension_ids)?;
    let inventory = ci_profile::list_for_extension(&ctx.source_path, &extension_id)?;

    Ok((
        CiOutput::List(CiListOutput {
            command: "ci.list",
            component_id: ctx.component_id,
            source_path: ctx.source_path,
            inventory,
        }),
        0,
    ))
}

fn run_ci(args: CiRunArgs, _global: &GlobalArgs) -> CmdResult<CiOutput> {
    let ctx = execution_context::resolve(&ResolveOptions {
        component_id: args.comp.component.clone(),
        path_override: args.comp.path.clone(),
        capability: None,
        settings_overrides: Vec::new(),
        settings_json_overrides: Vec::new(),
        extension_overrides: args.extension_override.extensions.clone(),
    })?;
    let extension_ids = ctx
        .component
        .extensions
        .as_ref()
        .map(|extensions| {
            let mut ids: Vec<String> = extensions.keys().cloned().collect();
            ids.sort();
            ids
        })
        .unwrap_or_default();
    let extension_id = ci_profile::select_extension_id(&extension_ids)?;
    let selection = ci_run_selection(&args)?;
    let run = ci_profile::run_for_extension(&ctx.source_path, &extension_id, selection)?;
    let exit_code = run.exit_code;

    Ok((
        CiOutput::Run(CiRunCommandOutput {
            command: "ci.run",
            component_id: ctx.component_id,
            source_path: ctx.source_path,
            run,
        }),
        exit_code,
    ))
}

fn run_autofix(args: CiAutofixArgs, _global: &GlobalArgs) -> CmdResult<CiOutput> {
    let ctx = execution_context::resolve(&ResolveOptions {
        component_id: args.comp.component.clone(),
        path_override: args.comp.path.clone(),
        capability: None,
        settings_overrides: Vec::new(),
        settings_json_overrides: Vec::new(),
        extension_overrides: args.extension_override.extensions.clone(),
    })?;

    let token = args
        .token
        .clone()
        .or_else(|| std::env::var("APP_TOKEN").ok().filter(|t| !t.is_empty()));
    let ci = CiContext {
        target_repo: args.target_repo.clone(),
        origin_repo: args.origin_repo.clone(),
        target_branch: args.target_branch.clone(),
        token,
    };
    let push_target = ci.resolve_push_target();

    let fix_commit_message = args
        .message
        .clone()
        .unwrap_or_else(|| AUTOFIX_COMMIT_PREFIX.to_string());

    let outcome = transaction::run_autofix_transaction(TransactionRequest {
        repo_path: &ctx.source_path,
        component: &ctx.component,
        ci,
        git_identity: args.git_identity.as_deref(),
        fix_commit_message,
        dry_run: args.dry_run,
    })?;

    let exit_code = if outcome.committed || args.dry_run || outcome.status == "no-changes" {
        0
    } else {
        1
    };

    Ok((
        CiOutput::Autofix(CiAutofixCommandOutput {
            command: "ci.autofix",
            component_id: ctx.component_id,
            source_path: ctx.source_path,
            push_target,
            outcome,
        }),
        exit_code,
    ))
}

fn ci_run_selection(args: &CiRunArgs) -> homeboy::core::Result<CiRunSelection> {
    match (&args.job, &args.profile) {
        (Some(job), None) => Ok(CiRunSelection::Job(job.clone())),
        (None, Some(profile)) => Ok(CiRunSelection::Profile(profile.clone())),
        _ => Err(homeboy::core::Error::validation_missing_argument(vec![
            "--job <ID> or --profile <ID>".to_string(),
        ])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_ci_list_path_and_extension() {
        let cli = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "ci",
            "list",
            "--path",
            "/tmp/repo",
            "--extension",
            "fixture-ci",
        ])
        .expect("parse cli");

        let crate::cli_surface::Commands::Ci(args) = cli.command else {
            panic!("expected ci command");
        };
        let CiCommand::List(args) = args.command else {
            panic!("expected ci list");
        };

        assert_eq!(args.comp.path.as_deref(), Some("/tmp/repo"));
        assert_eq!(args.extension_override.extensions, vec!["fixture-ci"]);
    }

    #[test]
    fn parses_ci_run_job_path_and_extension() {
        let cli = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "ci",
            "run",
            "--path",
            "/tmp/repo",
            "--extension",
            "fixture-ci",
            "--job",
            "lint",
        ])
        .expect("parse cli");

        let crate::cli_surface::Commands::Ci(args) = cli.command else {
            panic!("expected ci command");
        };
        let CiCommand::Run(args) = args.command else {
            panic!("expected ci run");
        };

        assert_eq!(args.comp.path.as_deref(), Some("/tmp/repo"));
        assert_eq!(args.extension_override.extensions, vec!["fixture-ci"]);
        assert_eq!(args.job.as_deref(), Some("lint"));
    }

    #[test]
    fn ci_run_requires_job_or_profile() {
        let args = CiRunArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: None,
            },
            extension_override: ExtensionOverrideArgs::default(),
            job: None,
            profile: None,
        };

        assert!(ci_run_selection(&args).is_err());
    }
}
