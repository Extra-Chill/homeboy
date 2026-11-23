//! `homeboy bench compare`: base-vs-head corpus A/B for a component.
//!
//! The CLI resolves the component, revisions, corpus, runner, and artifact
//! location (merging an optional spec with flags), then hands a fully resolved
//! request to `homeboy_core::bench_compare`, which owns materialization,
//! lifecycle, diffing, and the artifact.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use homeboy_core::bench_compare::{
    self, BenchCompareFailOn, BenchCompareOutput, BenchCompareRequest, BenchCompareSpec,
    WORKTREE_HEAD,
};

#[derive(Args, Clone, Debug, Default)]
pub struct BenchCompareArgs {
    /// Component to compare. Defaults to the component discovered from
    /// `--path` or the current directory.
    #[arg(long, value_name = "COMPONENT")]
    pub component: Option<String>,

    /// Component checkout path (overrides the registered local path).
    #[arg(long, value_name = "PATH")]
    pub path: Option<String>,

    /// Base revision (commit, branch, or tag).
    #[arg(long, value_name = "REV")]
    pub base: Option<String>,

    /// Head revision. `WORKTREE` (the default) compares the checkout as it is
    /// on disk, including uncommitted changes.
    #[arg(long, value_name = "REV")]
    pub head: Option<String>,

    /// Corpus (captures) both sides import. `$NAME` reads the path from the
    /// environment variable `NAME`.
    #[arg(long, value_name = "PATH")]
    pub corpus: Option<String>,

    /// Runner command overriding the component's `scripts.bench_compare` or
    /// the extension-declared `bench-compare` workload kind. Repeat for
    /// multiple sequential commands.
    #[arg(long = "workload-command", value_name = "COMMAND")]
    pub workload_command: Vec<String>,

    /// Regression policy: `new-blockers` (default) fails on new evaluator
    /// blockers or routes missing on head; `regressions` fails on any
    /// regressed route; `never` only reports. Runner failures and processes
    /// surviving teardown always fail.
    #[arg(long = "fail-on", value_name = "POLICY", value_parser = BenchCompareFailOn::VALUES.to_vec())]
    pub fail_on: Option<String>,

    /// Wall-clock budget per side, in seconds (default 3600).
    #[arg(long = "timeout-seconds", value_name = "SECONDS")]
    pub timeout_seconds: Option<u64>,

    /// JSON spec file declaring any of the fields above. Flags override it.
    #[arg(long, value_name = "FILE")]
    pub spec: Option<PathBuf>,

    /// Directory for the durable artifact. Defaults to
    /// `<homeboy data>/bench-compare/<compare-id>`.
    #[arg(long = "output-dir", value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
}

impl BenchCompareArgs {
    fn load_spec(&self) -> homeboy::core::Result<BenchCompareSpec> {
        let Some(path) = &self.spec else {
            return Ok(BenchCompareSpec::default());
        };
        let json = std::fs::read_to_string(path).map_err(|error| {
            homeboy::core::Error::validation_invalid_argument(
                "--spec",
                format!("cannot read bench compare spec {}: {error}", path.display()),
                Some(path.display().to_string()),
                None,
            )
        })?;
        BenchCompareSpec::parse(&json)
    }

    /// Flags override spec fields.
    pub fn effective_spec(&self) -> homeboy::core::Result<BenchCompareSpec> {
        let mut spec = self.load_spec()?;
        let pick = |flag: &Option<String>, field: &mut Option<String>| {
            if flag.is_some() {
                *field = flag.clone();
            }
        };
        pick(&self.component, &mut spec.component);
        pick(&self.path, &mut spec.path);
        pick(&self.base, &mut spec.base);
        pick(&self.head, &mut spec.head);
        pick(&self.corpus, &mut spec.corpus);
        if !self.workload_command.is_empty() {
            spec.runner = self.workload_command.clone();
        }
        if let Some(fail_on) = &self.fail_on {
            spec.fail_on = Some(fail_on.parse().map_err(|message: String| {
                homeboy::core::Error::validation_invalid_argument("--fail-on", message, None, None)
            })?);
        }
        if self.timeout_seconds.is_some() {
            spec.timeout_seconds = self.timeout_seconds;
        }
        Ok(spec)
    }
}

pub fn run_compare(args: &BenchCompareArgs) -> homeboy::core::Result<(BenchCompareOutput, i32)> {
    let spec = args.effective_spec()?;

    let path_override = spec
        .path
        .clone()
        .or_else(|| spec.component.is_none().then(|| ".".to_string()));
    let component = homeboy::core::component::resolve_effective(
        spec.component.as_deref(),
        path_override.as_deref(),
        None,
    )?;
    let component_path = PathBuf::from(&component.local_path);
    let repo_root = homeboy::core::git::repo_root(&component_path).ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "component",
            format!(
                "component '{}' at {} is not inside a git checkout; bench compare materializes revisions with git",
                component.id,
                component_path.display()
            ),
            Some(component.id.clone()),
            None,
        )
    })?;
    let component_relative = relative_component_path(&repo_root, &component_path);

    let base = spec.base.clone().ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "--base",
            "bench compare needs a base revision",
            None,
            Some(vec![
                "Pass --base <sha|branch|tag> or set `base` in the spec.".to_string(),
            ]),
        )
    })?;
    let head = spec
        .head
        .clone()
        .unwrap_or_else(|| WORKTREE_HEAD.to_string());
    let corpus = resolve_corpus(spec.corpus.as_deref())?;

    let runner = resolve_runner(&component, &spec.runner)?;

    let compare_id = format!(
        "bench-compare-{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let artifact_dir = match &args.output_dir {
        Some(dir) => absolute(dir)?,
        None => homeboy::core::paths::homeboy_data_store(bench_compare::ARTIFACT_STORE)?
            .join(&compare_id),
    };

    // Cancel (Ctrl-C / SIGTERM) tears down the running side instead of
    // orphaning its servers. Handler installation is best-effort: a process
    // that already owns SIGINT keeps its handler and still reaps via drop.
    let stop = Arc::new(AtomicBool::new(false));
    let _ = homeboy::core::process::install_terminate_handler(stop.clone());
    let _ = homeboy::core::process::install_shutdown_handler(stop.clone(), "bench compare");

    let output = bench_compare::run_bench_compare(&BenchCompareRequest {
        compare_id,
        component_id: component.id.clone(),
        repo_root,
        component_relative,
        base,
        head,
        corpus,
        runner,
        fail_on: spec.fail_on.unwrap_or_default(),
        side_timeout: Duration::from_secs(
            spec.timeout_seconds
                .unwrap_or(bench_compare::DEFAULT_SIDE_TIMEOUT_SECONDS),
        ),
        artifact_dir,
        stop: &stop,
    })?;
    let exit = if output.policy.passed { 0 } else { 1 };
    Ok((output, exit))
}

/// Resolve the per-side corpus workload: explicit `--workload-command`/spec commands,
/// then the component's `scripts.bench_compare`, then the `bench-compare`
/// workload kind declared by a linked extension. Core never hardcodes how a
/// corpus is imported or evaluated.
fn resolve_runner(
    component: &homeboy::core::component::Component,
    explicit: &[String],
) -> homeboy::core::Result<Vec<String>> {
    if !explicit.is_empty() {
        return Ok(explicit.to_vec());
    }
    let declared = component
        .scripts
        .as_ref()
        .map(|scripts| scripts.bench_compare.clone())
        .unwrap_or_default();
    if !declared.is_empty() {
        return Ok(declared);
    }
    let has_extensions = component
        .extensions
        .as_ref()
        .is_some_and(|extensions| !extensions.is_empty());
    let extension_detail = if has_extensions {
        match homeboy::core::extension::resolve::resolve_workload_execution_context(
            component,
            bench_compare::WORKLOAD_KIND,
        ) {
            Ok((context, _)) if !context.script_path.trim().is_empty() => {
                let script = context.extension_path.join(&context.script_path);
                return Ok(vec![format!(
                    "bash {}",
                    homeboy_engine_primitives::shell::quote_path(&script.to_string_lossy())
                )]);
            }
            Ok((context, _)) => Some(format!(
                "extension '{}' declares workloads.{} without a script",
                context.extension_id,
                bench_compare::WORKLOAD_KIND
            )),
            Err(error) => Some(error.message),
        }
    } else {
        None
    };
    let mut hints = vec![
        "Declare `scripts.bench_compare` in the component's homeboy.json, e.g. \"scripts\": {\"bench_compare\": [\"./bench/compare.sh\"]}.".to_string(),
        format!(
            "Or declare `workloads.{}` with a `script` in a linked extension's manifest.",
            bench_compare::WORKLOAD_KIND
        ),
        format!(
            "Or pass --workload-command <command>. The runner reads ${} and writes a `{}` document to ${}.",
            bench_compare::env::CORPUS,
            bench_compare::SIDE_RESULTS_SCHEMA,
            bench_compare::env::RESULTS
        ),
    ];
    if let Some(detail) = extension_detail {
        hints.insert(0, detail);
    }
    Err(homeboy::core::Error::validation_invalid_argument(
        "runner",
        format!(
            "component '{}' declares no bench compare runner",
            component.id
        ),
        Some(component.id.clone()),
        Some(hints),
    ))
}

fn relative_component_path(repo_root: &Path, component_path: &Path) -> PathBuf {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical(component_path)
        .strip_prefix(canonical(repo_root))
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn absolute(path: &Path) -> homeboy::core::Result<PathBuf> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| homeboy::core::Error::internal_io(error.to_string(), None))
}

pub(crate) fn resolve_corpus(value: Option<&str>) -> homeboy::core::Result<PathBuf> {
    let value = value.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "--corpus",
            "bench compare needs a corpus",
            None,
            Some(vec![
                "Pass --corpus <path>, or `$NAME` to read the path from an environment variable."
                    .to_string(),
            ]),
        )
    })?;
    let raw = match value.strip_prefix('$') {
        Some(name) => std::env::var(name).map_err(|_| {
            homeboy::core::Error::validation_invalid_argument(
                "--corpus",
                format!("corpus environment variable `{name}` is not set"),
                Some(value.to_string()),
                None,
            )
        })?,
        None => value.to_string(),
    };
    let path = absolute(Path::new(&raw))?;
    if !path.exists() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "--corpus",
            format!("corpus {} does not exist", path.display()),
            Some(raw),
            Some(vec![
                "Pass an existing corpus directory, or `$NAME` naming an environment variable that holds one.".to_string(),
            ]),
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_override_spec_fields() {
        let dir = tempfile::tempdir().unwrap();
        let spec = dir.path().join("spec.json");
        std::fs::write(
            &spec,
            r#"{"component":"blocks-engine","base":"main","corpus":"/c","fail_on":"never"}"#,
        )
        .unwrap();
        let args = BenchCompareArgs {
            spec: Some(spec),
            base: Some("abc123".to_string()),
            fail_on: Some("regressions".to_string()),
            ..Default::default()
        };
        let spec = args.effective_spec().unwrap();
        assert_eq!(spec.component.as_deref(), Some("blocks-engine"));
        assert_eq!(spec.base.as_deref(), Some("abc123"));
        assert_eq!(spec.corpus.as_deref(), Some("/c"));
        assert_eq!(spec.fail_on, Some(BenchCompareFailOn::Regressions));
    }

    #[test]
    fn corpus_env_reference_must_be_set() {
        assert!(resolve_corpus(Some("$HOMEBOY_BENCH_COMPARE_TEST_UNSET_CORPUS")).is_err());
        assert!(resolve_corpus(None).is_err());
    }
}
