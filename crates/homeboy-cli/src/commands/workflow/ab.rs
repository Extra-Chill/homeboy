//! `homeboy workflow ab`: prove a refactor is byte-identical.
//!
//! A thin front end over the built-in `homeboy.ab` recipe. It resolves both
//! refs to commits on the controller, picks the runner through the normal
//! placement rules, and admits the recipe there: locally against the
//! repository, or on a Lab runner (`workflow run --on`) against one
//! Git or snapshot workspace per commit. `ab-step` is the recipe's step verb.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};
use homeboy::core::workflow::ab::{self, AbSide};
use homeboy::core::{Error, Result};
use serde_json::{json, Value};

use crate::cli_surface::Placement;
use crate::commands::CmdResult;

#[derive(Args)]
#[command(
    after_help = "Placement: auto and lab require a ready Lab runner and fail with its readiness reason otherwise. Only --placement local or lab-or-local may run on the controller."
)]
pub struct AbArgs {
    /// Component id or repository path
    target: String,
    /// The reference ref (branch, tag, or commit)
    #[arg(long, value_name = "REF")]
    base: String,
    /// The ref under test
    #[arg(long, value_name = "REF")]
    candidate: String,
    /// Preparation command run before --run, recorded separately; failure skips --run
    #[arg(long, value_name = "COMMAND")]
    prepare: Option<String>,
    /// Command run with `sh -c` in each checkout
    #[arg(long = "run", value_name = "COMMAND")]
    run: String,
    /// Output path, directory, or glob relative to the checkout (repeatable)
    #[arg(long = "compare", value_name = "PATH_OR_GLOB", required = true)]
    compare: Vec<String>,
    /// Instance name; distinct names re-run the same refs and command
    #[arg(long, value_name = "NAME")]
    instance: Option<String>,
    #[arg(skip)]
    pub(crate) wait: bool,
    #[arg(skip)]
    pub(crate) placement: Placement,
}

#[derive(Args)]
pub struct AbStepArgs {
    #[command(subcommand)]
    command: AbStepCommand,
}

#[derive(Subcommand)]
enum AbStepCommand {
    /// Run one side in an isolated checkout and hash its outputs
    Side {
        #[arg(long)]
        side: String,
        #[command(flatten)]
        repos: StepRepos,
        #[arg(long)]
        base_sha: String,
        #[arg(long)]
        candidate_sha: String,
        #[arg(long)]
        prepare: Option<String>,
        #[arg(long, default_value = "local")]
        base_materialization: String,
        #[arg(long, default_value = "local")]
        candidate_materialization: String,
        #[arg(long = "run")]
        run: String,
        /// A JSON array of paths/globs, or one path/glob
        #[arg(long)]
        compare: String,
    },
    /// Compare both side records and write the verdict
    Verdict {
        #[arg(long = "run")]
        run: String,
        /// The `side` step's items (JSON)
        #[arg(long)]
        sides: String,
    },
    /// Remove the instance's checkouts
    Cleanup {
        #[command(flatten)]
        repos: StepRepos,
    },
}

#[derive(Args)]
struct StepRepos {
    #[arg(long)]
    instance: String,
    #[arg(long)]
    base_repo: PathBuf,
    #[arg(long)]
    candidate_repo: PathBuf,
    #[arg(long, default_value = "false")]
    isolated: String,
}

impl StepRepos {
    fn isolated(&self) -> bool {
        self.isolated == "true"
    }
}

pub(super) fn run(args: AbArgs) -> CmdResult<Value> {
    let placement = args.placement;
    let repo = resolve_repo(&args.target)?;
    let base_sha = rev_parse(&repo, &args.base)?;
    let candidate_sha = rev_parse(&repo, &args.candidate)?;
    let runner = select_runner(placement)?;
    let definition = ab::definition();
    let compare = serde_json::to_string(&args.compare)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;

    let (base_repo, candidate_repo, base_materialization, candidate_materialization, isolated) =
        match &runner {
            None => (repo.clone(), repo.clone(), "local", "local", false),
            Some(runner) => {
                let (base, base_mode) = sync_commit(runner, &repo, &base_sha)?;
                let (candidate, candidate_mode) = sync_commit(runner, &repo, &candidate_sha)?;
                (
                    PathBuf::from(base),
                    PathBuf::from(candidate),
                    base_mode,
                    candidate_mode,
                    true,
                )
            }
        };
    let inputs = [
        ("base_repo", base_repo.display().to_string()),
        ("base_sha", base_sha.clone()),
        ("candidate_repo", candidate_repo.display().to_string()),
        ("candidate_sha", candidate_sha.clone()),
        ("run", args.run.clone()),
        ("prepare", args.prepare.clone().unwrap_or_default()),
        ("base_materialization", base_materialization.to_string()),
        (
            "candidate_materialization",
            candidate_materialization.to_string(),
        ),
        ("compare", compare),
        ("isolated", isolated.to_string()),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_string(), value))
    .collect::<Vec<_>>();

    let (mut view, code) = match &runner {
        None => {
            let provided = inputs
                .iter()
                .map(|(name, value)| (name.clone(), Value::String(value.clone())))
                .collect::<BTreeMap<_, _>>();
            super::admit(
                &definition,
                &provided,
                args.instance.as_deref(),
                args.wait,
                "workflow ab",
            )?
        }
        Some(runner) => {
            let run_args = super::RunArgs {
                workflow: ab::AB_WORKFLOW_ID.to_string(),
                inputs,
                instance: args.instance.clone(),
                resume_from: None,
                on: Some(runner.clone()),
                wait: args.wait,
            };
            let (mut view, code) = super::run_on_runner(runner, &definition, &run_args)?;
            if let Some(instance) = view["instance"].as_str() {
                view["cancel_command"] = json!(format!(
                    "homeboy runner exec {runner} -- homeboy workflow cancel {instance}"
                ));
            }
            (view, code)
        }
    };
    view["ab"] = json!({
        "repo": repo.display().to_string(),
        "base": { "ref": args.base, "sha": base_sha },
        "candidate": { "ref": args.candidate, "sha": candidate_sha },
        "placement": if runner.is_some() { "lab" } else { "local" },
        "runner": runner,
    });
    let verdict = view["steps"]["verdict"]["output"].clone();
    let code = match verdict["verdict"].as_str() {
        Some("identical") => code,
        Some(_) => 1,
        None => code,
    };
    if verdict.is_object() {
        view["verdict"] = verdict;
    }
    Ok((view, code))
}

pub(super) fn step(args: AbStepArgs) -> CmdResult<Value> {
    match args.command {
        AbStepCommand::Side {
            side,
            repos,
            base_sha,
            candidate_sha,
            run,
            prepare,
            base_materialization,
            candidate_materialization,
            compare,
        } => {
            let (repo, sha) = match side.as_str() {
                "base" => (&repos.base_repo, base_sha),
                "candidate" => (&repos.candidate_repo, candidate_sha),
                other => {
                    return Err(Error::validation_invalid_argument(
                        "side",
                        "side must be `base` or `candidate`",
                        Some(other.to_string()),
                        None,
                    ))
                }
            };
            let compare = if compare.trim_start().starts_with('[') {
                serde_json::from_str(&compare).map_err(|error| {
                    Error::validation_invalid_argument("compare", error.to_string(), None, None)
                })?
            } else {
                vec![compare]
            };
            let root = ab::checkout_root(&repos.instance);
            let run_id = std::env::var(homeboy::core::daemon::step_job::STEP_JOB_ID_ENV)
                .unwrap_or_else(|_| format!("{}-{side}", repos.instance));
            let record = ab::run_side(&AbSide {
                side: &side,
                repo,
                sha: &sha,
                isolated: repos.isolated(),
                prepare: prepare.as_deref(),
                materialization: if side == "base" {
                    &base_materialization
                } else {
                    &candidate_materialization
                },
                run: &run,
                compare: &compare,
                checkout_root: &root,
                run_id,
            })?;
            let code = i32::from(record["error"].as_str().is_some());
            Ok((record, code))
        }
        AbStepCommand::Verdict { run, sides } => {
            let sides: Value = serde_json::from_str(&sides).map_err(|error| {
                Error::validation_invalid_argument("sides", error.to_string(), None, None)
            })?;
            Ok((ab::verdict(&sides, &run)?, 0))
        }
        AbStepCommand::Cleanup { repos } => Ok((
            ab::cleanup(
                &[&repos.base_repo, &repos.candidate_repo],
                repos.isolated(),
                &ab::checkout_root(&repos.instance),
            ),
            0,
        )),
    }
}

/// The PR-pasteable summary of a finished `workflow ab`.
pub(crate) fn render_summary(payload: &Value) -> Option<String> {
    payload["verdict"]["summary"].as_str().map(|summary| {
        let mut out = summary.to_string();
        if let Some(instance) = payload["instance"].as_str() {
            out.push_str(&format!(
                "- evidence: `homeboy runs show {instance}` (verdict step output)\n"
            ));
        }
        out
    })
}

/// A component id resolves to its checkout; anything else is a path.
fn resolve_repo(target: &str) -> Result<PathBuf> {
    let path = Path::new(target);
    if path.is_dir() {
        return homeboy::core::paths::absolute_local_path(path.to_path_buf());
    }
    homeboy::core::component::resolve_target(homeboy::core::component::TargetSpec::new(
        Some(target),
        None,
    ))
    .map(|target| target.source_path)
}

fn rev_parse(repo: &Path, reference: &str) -> Result<String> {
    git(
        repo,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    )
}

/// Only explicit local placement permits controller execution.
fn select_runner(placement: Placement) -> Result<Option<String>> {
    if placement == Placement::Local {
        return Ok(None);
    }
    let readiness = homeboy::runner::lab_runner_readiness()?;
    runner_from_readiness(placement, readiness)
}

fn runner_from_readiness(
    placement: Placement,
    readiness: homeboy::runner::LabRunnerReadiness,
) -> Result<Option<String>> {
    let runner = readiness.selected_runner_id;
    if matches!(placement, Placement::Auto | Placement::Lab) && runner.is_none() {
        return Err(Error::validation_invalid_argument(
            "placement",
            format!("workflow ab requires a ready Lab runner: {}: {}; use --placement local or --placement lab-or-local to permit controller execution", readiness.state.as_str(), readiness.reasons.join(", ")),
            None,
            None,
        ));
    }
    Ok(runner)
}

/// Fetch origin-reachable commits on the runner; upload a Git snapshot for
/// unpublished commits. Return the workspace path and materialization route.
fn sync_commit(runner: &str, repo: &Path, sha: &str) -> Result<(String, &'static str)> {
    let on_origin = !git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            &format!("--contains={sha}"),
            "refs/remotes/origin/",
        ],
    )?
    .is_empty();
    let staging = tempfile::Builder::new()
        .prefix("homeboy-ab-")
        .tempdir()
        .map_err(|error| Error::internal_io(error.to_string(), None))?;
    let checkout = staging.path().join(&sha[..12.min(sha.len())]);
    let checkout_arg = checkout.display().to_string();
    git(
        repo,
        &["worktree", "add", "--detach", "--quiet", &checkout_arg, sha],
    )?;
    let synced = homeboy::runner::runners::sync_workspace(
        runner,
        homeboy::runner::runners::RunnerWorkspaceSyncOptions {
            path: checkout_arg.clone(),
            mode: if on_origin {
                homeboy::runner::runners::RunnerWorkspaceSyncMode::Git
            } else {
                homeboy::runner::runners::RunnerWorkspaceSyncMode::SnapshotGit
            },
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: if on_origin {
                vec![sha.to_string()]
            } else {
                Vec::new()
            },
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
            validation_dependency_ids: None,
            run_isolation_token: None,
        },
    );
    let _ = git(repo, &["worktree", "remove", "--force", &checkout_arg]);
    Ok((
        synced?.0.remote_path,
        if on_origin {
            "origin_fetch"
        } else {
            "snapshot"
        },
    ))
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|error| Error::internal_io(error.to_string(), Some("run git".into())))?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "ref",
            format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            None,
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use clap::Parser;

    #[test]
    fn workflow_ab_auto_refuses_unready_lab_before_admission() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let dir = tempfile::tempdir().unwrap();
            git(dir.path(), &["init", "-q"]).unwrap();
            git(
                dir.path(),
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    "base",
                ],
            )
            .unwrap();
            for placement in [Placement::Auto, Placement::Lab] {
                let error = run(AbArgs {
                    target: dir.path().display().to_string(),
                    base: "HEAD".into(),
                    candidate: "HEAD".into(),
                    prepare: None,
                    run: "touch should-not-run".into(),
                    compare: vec!["out".into()],
                    instance: None,
                    wait: true,
                    placement,
                })
                .unwrap_err();
                assert!(error.to_string().contains("absent"), "{error}");
                assert!(!dir.path().join("should-not-run").exists());
            }
            let error = runner_from_readiness(
                Placement::Auto,
                homeboy::runner::LabRunnerReadiness {
                    state: homeboy::runner::LabRunnerReadinessState::CapacityBlocked,
                    selected_runner_id: None,
                    available_runner_ids: vec![],
                    reasons: vec!["pressure".into()],
                    remediation_commands: vec![],
                },
            )
            .unwrap_err();
            assert!(error.to_string().contains("capacity_blocked: pressure"));
        });
    }

    #[test]
    fn workflow_ab_sync_fetches_origin_sha_and_snapshots_unpushed_sha() {
        homeboy::core::test_support::with_isolated_home(|_| {
            let dir = tempfile::tempdir().unwrap();
            let origin = dir.path().join("origin");
            let repo = dir.path().join("repo");
            std::fs::create_dir(&origin).unwrap();
            git(&origin, &["init", "-q"]).unwrap();
            git(
                &origin,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    "base",
                ],
            )
            .unwrap();
            git(dir.path(), &["clone", "origin", "repo"]).unwrap();
            let base = rev_parse(&repo, "HEAD").unwrap();
            git(
                &repo,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "--allow-empty",
                    "-qm",
                    "unpushed",
                ],
            )
            .unwrap();
            let candidate = rev_parse(&repo, "HEAD").unwrap();
            homeboy::runner::create(&json!({"id":"ab-fetch", "kind":"local", "workspace_root":dir.path().join("runner")}).to_string(), false).unwrap();
            for (sha, expected) in [(&base, "origin_fetch"), (&candidate, "snapshot")] {
                let (path, mode) = sync_commit("ab-fetch", &repo, sha).unwrap();
                assert_eq!(mode, expected);
                assert_eq!(rev_parse(Path::new(&path), "HEAD").unwrap(), *sha);
            }
        });
    }

    #[test]
    fn workflow_ab_parses_repeatable_compare_and_step_verbs() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "--placement",
            "local",
            "workflow",
            "ab",
            ".",
            "--base",
            "main",
            "--candidate",
            "HEAD",
            "--run",
            "make out",
            "--compare",
            "out",
            "--compare",
            "dist/*.html",
        ])
        .expect("parse workflow ab");
        let Commands::Workflow(args) = cli.command else {
            panic!("workflow");
        };
        let super::super::WorkflowCommand::Ab(ab) = args.command else {
            panic!("ab");
        };
        assert_eq!(ab.compare, ["out", "dist/*.html"]);
        assert!(Cli::try_parse_from([
            "homeboy",
            "workflow",
            "ab",
            ".",
            "--base",
            "a",
            "--candidate",
            "b",
            "--run",
            "x",
        ])
        .is_err());

        // The recipe's step argv is a real command line.
        let definition = ab::definition();
        for step in &definition.steps {
            let mut argv = vec!["homeboy".to_string()];
            argv.extend(step.homeboy.clone().expect("homeboy step"));
            Cli::try_parse_from(&argv).unwrap_or_else(|error| panic!("{}: {error}", step.id));
        }
        super::super::validate(&definition).expect("recipe validates");
    }

    #[test]
    fn workflow_ab_side_verbs_prove_parity_over_a_temp_repo() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let git_in = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .current_dir(repo)
                .output()
                .unwrap();
            assert!(output.status.success(), "git {args:?}");
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        };
        git_in(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("gen.sh"), "echo one > out.txt\n").unwrap();
        git_in(&["add", "."]);
        git_in(&["commit", "-qm", "base"]);
        std::fs::write(repo.join("gen.sh"), "echo two > out.txt\n").unwrap();
        git_in(&["commit", "-qam", "candidate"]);
        let base = rev_parse(repo, "HEAD~1").unwrap();
        let candidate = rev_parse(repo, "main").unwrap();
        let instance = format!("wf-ab-test-{}", std::process::id());
        let repo_arg = repo.display().to_string();
        let parse_step = |extra: &[&str]| {
            let mut argv = vec!["homeboy", "workflow", "ab-step"];
            argv.extend_from_slice(extra);
            let cli = Cli::try_parse_from(argv).expect("parse ab-step");
            let Commands::Workflow(args) = cli.command else {
                panic!("workflow");
            };
            let super::super::WorkflowCommand::AbStep(step_args) = args.command else {
                panic!("ab-step");
            };
            step(step_args).expect("ab-step").0
        };
        let sides = ["base", "candidate"]
            .iter()
            .map(|side| {
                json!({ "item": side, "output": parse_step(&[
                    "side", "--side", side, "--instance", &instance,
                    "--base-repo", &repo_arg, "--base-sha", &base,
                    "--candidate-repo", &repo_arg, "--candidate-sha", &candidate,
                    "--run", "sh gen.sh", "--compare", "[\"out.txt\"]",
                ]) })
            })
            .collect::<Vec<_>>();
        let sides = serde_json::to_string(&sides).unwrap();
        let verdict = parse_step(&["verdict", "--run", "sh gen.sh", "--sides", &sides]);
        parse_step(&[
            "cleanup",
            "--instance",
            &instance,
            "--base-repo",
            &repo_arg,
            "--candidate-repo",
            &repo_arg,
        ]);
        assert_eq!(verdict["verdict"], "different");
        assert_eq!(verdict["differing"][0]["path"], "out.txt");
        assert_eq!(verdict["base"]["sha"], base);
        assert_eq!(verdict["candidate"]["sha"], candidate);
        assert!(!ab::checkout_root(&instance).exists());
        let summary = render_summary(&json!({ "verdict": verdict, "instance": instance })).unwrap();
        assert!(summary.contains("A/B parity: different") && summary.contains("`out.txt`"));
    }
}
