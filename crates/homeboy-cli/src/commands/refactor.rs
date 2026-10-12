use clap::{Args, Subcommand};
use homeboy::core::component::{self, TargetSpec};
use serde::Serialize;
use std::collections::HashSet;
use std::path::PathBuf;

use super::utils::args::WriteModeArgs;
use crate::commands::CmdResult;

mod rename;

use rename::{RenameContext, RenameScope, RenameSpec, RenameTargeting};

#[derive(Args)]
pub struct RefactorArgs {
    #[command(subcommand)]
    command: RefactorCommand,
}

#[derive(Subcommand)]
enum RefactorCommand {
    /// Rename a term across the codebase with case-variant awareness
    Rename {
        /// Term to rename from
        #[arg(long)]
        from: String,
        /// Term to rename to
        #[arg(long)]
        to: String,
        #[command(flatten)]
        target: RefactorTargetArgs,
        /// Scope: code, config, all (default: all)
        #[arg(long, default_value = "all")]
        scope: String,
        /// Exact string matching (no boundary detection, no case variants)
        #[arg(long)]
        literal: bool,
        /// Include only files matching this glob (repeatable)
        #[arg(long = "files", value_name = "GLOB")]
        files: Vec<String>,
        /// Exclude files matching this glob (repeatable)
        #[arg(long, value_name = "GLOB")]
        exclude: Vec<String>,
        /// Add an explicit variant mapping as FROM=TO (repeatable)
        #[arg(long = "variant", value_name = "FROM=TO")]
        variants: Vec<String>,
        /// Disable file/directory path renames (content edits only)
        #[arg(long)]
        no_file_renames: bool,
        /// Syntactic context filter: key (strings/property access), variable/var,
        /// parameter/param, all (default — match everything)
        #[arg(long, default_value = "all")]
        context: String,
        #[command(flatten)]
        write_mode: WriteModeArgs,
    },
}

#[derive(Args, Debug, Clone, Default)]
struct RefactorTargetArgs {
    /// Target a component by ID (repeatable)
    #[arg(short, long = "component", value_name = "ID", action = clap::ArgAction::Append)]
    component_ids: Vec<String>,

    /// Target multiple components with a comma-separated list
    #[arg(long, value_name = "ID[,ID...]", value_delimiter = ',')]
    components: Vec<String>,

    /// Override the source root for a single target
    #[arg(long)]
    path: Option<String>,
}

/// Rename inputs shared by every target of one invocation.
struct RenameRequest<'a> {
    from: &'a str,
    to: &'a str,
    scope: &'a str,
    literal: bool,
    include_globs: &'a [String],
    exclude_globs: &'a [String],
    variants: &'a [String],
    no_file_renames: bool,
    context: &'a str,
    write: bool,
}

pub fn run(args: RefactorArgs) -> CmdResult<RefactorOutput> {
    match args.command {
        RefactorCommand::Rename {
            from,
            to,
            target,
            scope,
            literal,
            files,
            exclude,
            variants,
            no_file_renames,
            context,
            write_mode,
        } => {
            let request = RenameRequest {
                from: &from,
                to: &to,
                scope: &scope,
                literal,
                include_globs: &files,
                exclude_globs: &exclude,
                variants: &variants,
                no_file_renames,
                context: &context,
                write: write_mode.write,
            };
            run_across_targets("rename", target.resolve_targets()?, |component_id, path| {
                run_rename_single(&request, component_id, path)
            })
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "command")]
pub enum RefactorOutput {
    #[serde(rename = "refactor.rename")]
    Rename {
        from: String,
        to: String,
        scope: String,
        dry_run: bool,
        variants: Vec<VariantSummary>,
        total_references: usize,
        total_files: usize,
        edits: Vec<EditSummary>,
        file_renames: Vec<RenameSummary>,
        warnings: Vec<WarningSummary>,
        applied: bool,
    },

    #[serde(rename = "refactor.bulk")]
    Bulk {
        action: String,
        results: Vec<RefactorBulkItem>,
        summary: RefactorBulkSummary,
    },
}

#[derive(Serialize)]
pub struct VariantSummary {
    pub from: String,
    pub to: String,
    pub label: String,
}

#[derive(Serialize)]
pub struct EditSummary {
    pub file: String,
    pub replacements: usize,
}

#[derive(Serialize)]
pub struct RenameSummary {
    pub from: String,
    pub to: String,
}

#[derive(Serialize)]
pub struct WarningSummary {
    pub kind: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub message: String,
}

#[derive(Serialize)]
pub struct RefactorBulkItem {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Box<RefactorOutput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct RefactorBulkSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
}

#[derive(Debug, Clone)]
struct RefactorTarget {
    component_id: Option<String>,
    path: Option<String>,
    label: String,
}

impl RefactorTargetArgs {
    /// Resolve targets: `--path`, then `--component`/`--components`, then the
    /// current directory when neither is given.
    fn resolve_targets(&self) -> homeboy::core::Result<Vec<RefactorTarget>> {
        let component_ids = collect_component_ids(&self.component_ids, &self.components);
        if self.path.is_some() && !component_ids.is_empty() {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "component",
                "--path cannot be combined with multiple component IDs",
                None,
                Some(vec![
                    "Use --path for one target only".to_string(),
                    "Use --component/--components for multi-component refactors".to_string(),
                ]),
            ));
        }

        if let Some(path) = &self.path {
            let target = component::resolve_target(TargetSpec::new(None, Some(path)))?;
            return Ok(vec![RefactorTarget {
                label: target.component_id.clone(),
                component_id: Some(target.component_id),
                path: Some(target.source_path.to_string_lossy().to_string()),
            }]);
        }

        if component_ids.is_empty() {
            return resolve_cwd_target();
        }

        Ok(component_ids
            .into_iter()
            .map(|id| RefactorTarget {
                label: id.clone(),
                component_id: Some(id),
                path: None,
            })
            .collect())
    }
}

fn resolve_cwd_target() -> homeboy::core::Result<Vec<RefactorTarget>> {
    let cwd = std::env::current_dir().map_err(|error| {
        homeboy::core::Error::internal_io(error.to_string(), Some("current directory".to_string()))
    })?;
    let label = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(".")
        .to_string();

    Ok(vec![RefactorTarget {
        component_id: None,
        path: Some(cwd.to_string_lossy().to_string()),
        label,
    }])
}

/// Resolve the source root for one target and require it to be a directory.
fn resolve_root(component_id: Option<&str>, path: Option<&str>) -> homeboy::core::Result<PathBuf> {
    let target = component::resolve_target(TargetSpec::new(component_id, path))?;
    if !target.source_path.is_dir() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "path",
            format!("Not a directory: {}", target.source_path.display()),
            None,
            None,
        ));
    }
    Ok(target.source_path)
}

fn collect_component_ids(primary: &[String], secondary: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    primary
        .iter()
        .chain(secondary.iter())
        .filter_map(|id| {
            let trimmed = id.trim();
            if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect()
}

fn run_across_targets<F>(
    action: &str,
    targets: Vec<RefactorTarget>,
    mut run_single: F,
) -> CmdResult<RefactorOutput>
where
    F: FnMut(Option<&str>, Option<&str>) -> CmdResult<RefactorOutput>,
{
    if targets.len() == 1 {
        let target = &targets[0];
        return run_single(target.component_id.as_deref(), target.path.as_deref());
    }

    let mut results = Vec::with_capacity(targets.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut any_zero_exit = false;

    for target in targets {
        match run_single(target.component_id.as_deref(), target.path.as_deref()) {
            Ok((output, exit_code)) => {
                if exit_code == 0 {
                    any_zero_exit = true;
                }
                succeeded += 1;
                results.push(RefactorBulkItem {
                    id: target.label,
                    result: Some(Box::new(output)),
                    error: None,
                });
            }
            Err(error) => {
                failed += 1;
                results.push(RefactorBulkItem {
                    id: target.label,
                    result: None,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    let exit_code = if failed > 0 || !any_zero_exit { 1 } else { 0 };

    Ok((
        RefactorOutput::Bulk {
            action: action.to_string(),
            results,
            summary: RefactorBulkSummary {
                total: succeeded + failed,
                succeeded,
                failed,
            },
        },
        exit_code,
    ))
}

fn run_rename_single(
    request: &RenameRequest<'_>,
    component_id: Option<&str>,
    path: Option<&str>,
) -> CmdResult<RefactorOutput> {
    let scope = RenameScope::from_str(request.scope)?;
    let rename_context = RenameContext::from_str(request.context)?;

    let root = resolve_root(component_id, path)?;

    let explicit_variants = parse_rename_variants(request.variants)?;
    let mut spec = if request.literal {
        RenameSpec::literal(request.from, request.to, scope.clone())
    } else {
        RenameSpec::new(request.from, request.to, scope.clone())
    }
    .with_explicit_variants(explicit_variants);
    spec.rename_context = rename_context;
    let targeting = RenameTargeting {
        include_globs: request.include_globs.to_vec(),
        exclude_globs: request.exclude_globs.to_vec(),
        rename_files: !request.no_file_renames,
    };
    let mut result = rename::generate_renames_with_targeting(&spec, &root, &targeting);

    // Print warnings to stderr before applying
    for warning in &result.warnings {
        let location = warning
            .line
            .map(|l| format!("{}:{}", warning.file, l))
            .unwrap_or_else(|| warning.file.clone());
        homeboy::log_status!("warning", "{}: {}", location, warning.message);
    }

    if request.write {
        if !result.warnings.is_empty() {
            homeboy::log_status!(
                "warning",
                "{} collision warning(s) detected — applying anyway",
                result.warnings.len()
            );
        }
        rename::apply_renames(&mut result, &root)?;
    }

    let scope_str = match scope {
        RenameScope::Code => "code",
        RenameScope::Config => "config",
        RenameScope::All => "all",
    };

    Ok((
        RefactorOutput::Rename {
            from: request.from.to_string(),
            to: request.to.to_string(),
            scope: scope_str.to_string(),
            dry_run: !request.write,
            variants: result
                .variants
                .iter()
                .map(|v| VariantSummary {
                    from: v.from.clone(),
                    to: v.to.clone(),
                    label: v.label.clone(),
                })
                .collect(),
            total_references: result.total_references,
            total_files: result.total_files,
            edits: result
                .edits
                .iter()
                .map(|e| EditSummary {
                    file: e.file.clone(),
                    replacements: e.replacements,
                })
                .collect(),
            file_renames: result
                .file_renames
                .iter()
                .map(|r| RenameSummary {
                    from: r.from.clone(),
                    to: r.to.clone(),
                })
                .collect(),
            warnings: result
                .warnings
                .iter()
                .map(|w| WarningSummary {
                    kind: w.kind.clone(),
                    file: w.file.clone(),
                    line: w.line,
                    message: w.message.clone(),
                })
                .collect(),
            applied: result.applied,
        },
        0,
    ))
}

fn parse_rename_variants(values: &[String]) -> homeboy::core::Result<Vec<(String, String)>> {
    values
        .iter()
        .map(|value| {
            let invalid = |message: String| {
                homeboy::core::Error::validation_invalid_argument(
                    "variant",
                    message,
                    Some(value.clone()),
                    None,
                )
            };
            let (from, to) = value.split_once('=').ok_or_else(|| {
                invalid(format!(
                    "Invalid rename variant '{}'. Expected FROM=TO",
                    value
                ))
            })?;
            let (from, to) = (from.trim(), to.trim());
            if from.is_empty() || to.is_empty() {
                return Err(invalid(format!(
                    "Invalid rename variant '{}'. FROM and TO must be non-empty",
                    value
                )));
            }
            Ok((from.to_string(), to.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn dry_run_request<'a>(from: &'a str, to: &'a str) -> RenameRequest<'a> {
        RenameRequest {
            from,
            to,
            scope: "code",
            literal: false,
            include_globs: &[],
            exclude_globs: &[],
            variants: &[],
            no_file_renames: false,
            context: "all",
            write: false,
        }
    }

    #[test]
    fn collect_component_ids_dedupes_and_trims() {
        let ids = collect_component_ids(
            &["alpha".to_string(), " beta ".to_string()],
            &["beta".to_string(), "gamma".to_string(), "".to_string()],
        );

        assert_eq!(ids, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn target_args_reject_path_with_multiple_components() {
        let args = RefactorTargetArgs {
            component_ids: vec!["alpha".to_string(), "beta".to_string()],
            components: vec![],
            path: Some("/tmp/example".to_string()),
        };

        let error = args.resolve_targets().unwrap_err();
        assert!(
            error.to_string().contains("--path cannot be combined"),
            "unexpected error: {}",
            error
        );
    }

    #[test]
    fn target_args_build_multi_component_targets() {
        let args = RefactorTargetArgs {
            component_ids: vec!["alpha".to_string()],
            components: vec!["beta".to_string(), "alpha".to_string()],
            path: None,
        };

        let targets = args.resolve_targets().unwrap();
        let labels: Vec<_> = targets.into_iter().map(|target| target.label).collect();
        assert_eq!(labels, vec!["alpha", "beta"]);
    }

    #[test]
    fn rename_variants_require_from_equals_to() {
        assert_eq!(
            parse_rename_variants(&["Foo = Bar".to_string()]).unwrap(),
            vec![("Foo".to_string(), "Bar".to_string())]
        );
        assert!(parse_rename_variants(&["FooBar".to_string()]).is_err());
        assert!(parse_rename_variants(&["Foo=".to_string()]).is_err());
    }

    #[test]
    fn rename_no_match_dry_run_is_successful_empty_result() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("src");
        fs::create_dir_all(&src).expect("src dir");
        fs::write(src.join("sample.rs"), "fn present_name() {}\n").expect("fixture file");

        let (output, exit_code) = run_rename_single(
            &dry_run_request("missing_name", "new_name"),
            None,
            Some(dir.path().to_str().expect("utf8 path")),
        )
        .expect("rename should return structured output");

        assert_eq!(exit_code, 0);
        let RefactorOutput::Rename {
            total_references,
            total_files,
            edits,
            file_renames,
            applied,
            ..
        } = output
        else {
            panic!("expected rename output");
        };
        assert_eq!(total_references, 0);
        assert_eq!(total_files, 0);
        assert!(edits.is_empty());
        assert!(file_renames.is_empty());
        assert!(!applied);
    }
}
