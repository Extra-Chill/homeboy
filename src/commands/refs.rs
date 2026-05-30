use clap::Args;
use homeboy::core::component::{self, TargetSpec};
use homeboy::core::refactor::{self, RenameContext, RenameScope, RenameSpec, RenameTargeting};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use super::CmdResult;

#[derive(Args, Debug, Clone)]
pub struct RefsArgs {
    /// Symbol or term to find.
    pub symbol: String,

    /// Target a component by ID (repeatable).
    #[arg(short, long = "component", value_name = "ID", action = clap::ArgAction::Append)]
    component_ids: Vec<String>,

    /// Target multiple components with a comma-separated list.
    #[arg(long, value_name = "ID[,ID...]", value_delimiter = ',')]
    components: Vec<String>,

    /// Override the source root for a single target.
    #[arg(long)]
    path: Option<String>,

    /// Scope: code, config, all.
    #[arg(long, default_value = "all")]
    scope: String,

    /// Exact string matching (no boundary detection, no case variants).
    #[arg(long)]
    literal: bool,

    /// Include only files matching this glob (repeatable).
    #[arg(long = "files", value_name = "GLOB")]
    files: Vec<String>,

    /// Exclude files matching this glob (repeatable).
    #[arg(long, value_name = "GLOB")]
    exclude: Vec<String>,

    /// Syntactic context filter: key, variable/var, parameter/param, all.
    #[arg(long, default_value = "all")]
    context: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "command")]
pub enum RefsOutput {
    #[serde(rename = "refs")]
    Single(RefsReport),
    #[serde(rename = "refs.bulk")]
    Bulk {
        symbol: String,
        targets: Vec<RefsBulkItem>,
        summary: RefsBulkSummary,
    },
}

#[derive(Debug, Serialize)]
pub struct RefsReport {
    pub symbol: String,
    pub target: String,
    pub root: String,
    pub scope: String,
    pub literal: bool,
    pub total_references: usize,
    pub actionable_references: usize,
    pub homeboy_owned_references: usize,
    pub total_files: usize,
    pub counts: RefsCounts,
    pub files: Vec<RefsFile>,
}

#[derive(Debug, Default, Serialize)]
pub struct RefsCounts {
    pub by_kind: BTreeMap<String, usize>,
    pub by_owner: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize)]
pub struct RefsFile {
    pub file: String,
    pub owner: ReferenceOwner,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_reason: Option<String>,
    pub references: usize,
    pub by_kind: BTreeMap<String, usize>,
    pub items: Vec<RefsItem>,
}

#[derive(Debug, Serialize)]
pub struct RefsItem {
    pub line: usize,
    #[serde(rename = "column")]
    pub reference_column: usize,
    #[serde(rename = "matched")]
    pub matched_text: String,
    pub variant: String,
    pub kind: ReferenceKind,
    pub context: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceKind {
    Code,
    Doc,
    String,
    Comment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceOwner {
    Actionable,
    HomeboyOwned,
}

#[derive(Debug, Serialize)]
pub struct RefsBulkItem {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<RefsReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RefsBulkSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub total_references: usize,
    pub actionable_references: usize,
    pub homeboy_owned_references: usize,
}

struct RefsTarget {
    label: String,
    component_id: Option<String>,
    path: Option<String>,
}

pub fn run(args: RefsArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<RefsOutput> {
    let targets = resolve_targets(&args)?;
    let scope = RenameScope::from_str(&args.scope)?;
    let rename_context = RenameContext::from_str(&args.context)?;

    if targets.len() == 1 {
        let report = run_single(&args, &targets[0], scope, rename_context)?;
        let exit_code = if report.total_references == 0 { 1 } else { 0 };
        return Ok((RefsOutput::Single(report), exit_code));
    }

    let mut results = Vec::with_capacity(targets.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut total_references = 0usize;
    let mut actionable_references = 0usize;
    let mut homeboy_owned_references = 0usize;

    for target in targets {
        match run_single(&args, &target, scope.clone(), rename_context.clone()) {
            Ok(report) => {
                total_references += report.total_references;
                actionable_references += report.actionable_references;
                homeboy_owned_references += report.homeboy_owned_references;
                succeeded += 1;
                results.push(RefsBulkItem {
                    id: target.label,
                    result: Some(report),
                    error: None,
                });
            }
            Err(error) => {
                failed += 1;
                results.push(RefsBulkItem {
                    id: target.label,
                    result: None,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    let exit_code = if failed > 0 || total_references == 0 {
        1
    } else {
        0
    };
    Ok((
        RefsOutput::Bulk {
            symbol: args.symbol,
            targets: results,
            summary: RefsBulkSummary {
                total: succeeded + failed,
                succeeded,
                failed,
                total_references,
                actionable_references,
                homeboy_owned_references,
            },
        },
        exit_code,
    ))
}

fn run_single(
    args: &RefsArgs,
    target: &RefsTarget,
    scope: RenameScope,
    rename_context: RenameContext,
) -> homeboy::core::Result<RefsReport> {
    let resolved = component::resolve_target(TargetSpec::new(
        target.component_id.as_deref(),
        target.path.as_deref(),
    ))?;
    let root = resolved.source_path;
    let mut spec = if args.literal {
        RenameSpec::literal(&args.symbol, &args.symbol, scope.clone())
    } else {
        RenameSpec::new(&args.symbol, &args.symbol, scope.clone())
    };
    spec.rename_context = rename_context;

    let targeting = RenameTargeting {
        include_globs: args.files.clone(),
        exclude_globs: args.exclude.clone(),
        rename_files: false,
    };
    let references = refactor::find_references_with_targeting(&spec, &root, &targeting);

    let mut files = BTreeMap::<String, Vec<RefsItem>>::new();
    let mut counts = RefsCounts::default();
    let mut actionable_references = 0usize;
    let mut homeboy_owned_references = 0usize;

    for reference in references {
        let kind = classify_reference(&reference.file, &reference.context, reference.column);
        let (owner, _reason) = classify_owner(&reference.file);
        increment(&mut counts.by_kind, kind_key(kind));
        increment(&mut counts.by_owner, owner_key(owner));
        match owner {
            ReferenceOwner::Actionable => actionable_references += 1,
            ReferenceOwner::HomeboyOwned => homeboy_owned_references += 1,
        }
        files
            .entry(reference.file.clone())
            .or_default()
            .push(RefsItem {
                line: reference.line,
                reference_column: reference.column,
                matched_text: reference.matched,
                variant: reference.variant,
                kind,
                context: reference.context,
            });
    }

    let mut file_reports = Vec::with_capacity(files.len());
    for (file, items) in files {
        let (owner, owner_reason) = classify_owner(&file);
        let mut by_kind = BTreeMap::new();
        for item in &items {
            increment(&mut by_kind, kind_key(item.kind));
        }
        file_reports.push(RefsFile {
            file,
            owner,
            owner_reason,
            references: items.len(),
            by_kind,
            items,
        });
    }

    let total_references = actionable_references + homeboy_owned_references;
    let total_files = file_reports.len();
    Ok(RefsReport {
        symbol: args.symbol.clone(),
        target: resolved.component_id,
        root: root.to_string_lossy().to_string(),
        scope: scope_key(scope).to_string(),
        literal: args.literal,
        total_references,
        actionable_references,
        homeboy_owned_references,
        total_files,
        counts,
        files: file_reports,
    })
}

fn resolve_targets(args: &RefsArgs) -> homeboy::core::Result<Vec<RefsTarget>> {
    let component_ids = collect_component_ids(&args.component_ids, &args.components);
    if args.path.is_some() && !component_ids.is_empty() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "component",
            "--path cannot be combined with component IDs",
            None,
            Some(vec!["Use --path for one target only".to_string()]),
        ));
    }

    if let Some(path) = &args.path {
        return Ok(vec![RefsTarget {
            label: path.clone(),
            component_id: None,
            path: Some(path.clone()),
        }]);
    }

    if component_ids.is_empty() {
        return Ok(vec![RefsTarget {
            label: "cwd".to_string(),
            component_id: None,
            path: None,
        }]);
    }

    Ok(component_ids
        .into_iter()
        .map(|id| RefsTarget {
            label: id.clone(),
            component_id: Some(id),
            path: None,
        })
        .collect())
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

fn classify_reference(file: &str, line: &str, column: usize) -> ReferenceKind {
    if is_doc_file(file) {
        return ReferenceKind::Doc;
    }

    if is_comment_line(line) || is_inside_line_comment(line, column.saturating_sub(1)) {
        return ReferenceKind::Comment;
    }

    if is_inside_string(line, column.saturating_sub(1)) {
        return ReferenceKind::String;
    }

    ReferenceKind::Code
}

fn is_doc_file(file: &str) -> bool {
    matches!(
        Path::new(file).extension().and_then(|ext| ext.to_str()),
        Some("md" | "mdx" | "txt" | "rst" | "adoc")
    )
}

fn is_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with("<!--")
}

fn is_inside_line_comment(line: &str, column: usize) -> bool {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if matches!(bytes[i], b'\'' | b'"' | b'`') {
            quote = if quote == Some(bytes[i]) {
                None
            } else if quote.is_none() {
                Some(bytes[i])
            } else {
                quote
            };
        }
        if quote.is_none() && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            return i < column;
        }
        i += 1;
    }
    false
}

fn is_inside_string(line: &str, column: usize) -> bool {
    let bytes = line.as_bytes();
    for quote in [b'\'', b'"', b'`'] {
        let mut in_string = false;
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == quote {
                in_string = !in_string;
            }
            if i == column {
                if in_string {
                    return true;
                }
                break;
            }
            i += 1;
        }
    }
    false
}

fn classify_owner(file: &str) -> (ReferenceOwner, Option<String>) {
    let normalized = file.replace('\\', "/");
    if normalized == "CHANGELOG.md" || normalized.ends_with("/CHANGELOG.md") {
        return (
            ReferenceOwner::HomeboyOwned,
            Some("CHANGELOG.md is generated from commits by Homeboy".to_string()),
        );
    }
    if normalized.starts_with("artifacts/") || normalized.contains("/.homeboy/") {
        return (
            ReferenceOwner::HomeboyOwned,
            Some("generated Homeboy artifact".to_string()),
        );
    }
    (ReferenceOwner::Actionable, None)
}

fn increment(map: &mut BTreeMap<String, usize>, key: &str) {
    *map.entry(key.to_string()).or_default() += 1;
}

fn kind_key(kind: ReferenceKind) -> &'static str {
    match kind {
        ReferenceKind::Code => "code",
        ReferenceKind::Doc => "doc",
        ReferenceKind::String => "string",
        ReferenceKind::Comment => "comment",
    }
}

fn owner_key(owner: ReferenceOwner) -> &'static str {
    match owner {
        ReferenceOwner::Actionable => "actionable",
        ReferenceOwner::HomeboyOwned => "homeboy_owned",
    }
}

fn scope_key(scope: RenameScope) -> &'static str {
    match scope {
        RenameScope::Code => "code",
        RenameScope::Config => "config",
        RenameScope::All => "all",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_reference_separates_code_doc_string_and_comment() {
        assert_eq!(
            classify_reference("src/lib.rs", "call_symbol();", 6),
            ReferenceKind::Code
        );
        assert_eq!(
            classify_reference("README.md", "call_symbol();", 1),
            ReferenceKind::Doc
        );
        assert_eq!(
            classify_reference("src/lib.rs", "let s = \"call_symbol\";", 10),
            ReferenceKind::String
        );
        assert_eq!(
            classify_reference("src/lib.rs", "// call_symbol", 4),
            ReferenceKind::Comment
        );
    }

    #[test]
    fn classify_owner_flags_homeboy_owned_changelog() {
        let (owner, reason) = classify_owner("docs/CHANGELOG.md");

        assert_eq!(owner, ReferenceOwner::HomeboyOwned);
        assert!(reason.expect("reason").contains("generated"));
    }

    #[test]
    fn collect_component_ids_deduplicates_comma_and_repeated_values() {
        let ids = collect_component_ids(
            &["alpha".to_string(), "beta".to_string()],
            &["alpha".to_string(), "gamma".to_string()],
        );

        assert_eq!(ids, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn run_refs_enumerates_references_without_mutating_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("src");
        std::fs::create_dir_all(&source).expect("src dir");
        let code_path = source.join("lib.rs");
        let doc_path = dir.path().join("CHANGELOG.md");
        std::fs::write(
            &code_path,
            "fn target_symbol() {}\nlet value = \"target_symbol\";\n// target_symbol\n",
        )
        .expect("code fixture");
        std::fs::write(&doc_path, "- target_symbol was changed\n").expect("doc fixture");
        let before_code = std::fs::read_to_string(&code_path).expect("before code");
        let before_doc = std::fs::read_to_string(&doc_path).expect("before doc");

        let args = RefsArgs {
            symbol: "target_symbol".to_string(),
            component_ids: Vec::new(),
            components: Vec::new(),
            path: Some(dir.path().to_string_lossy().to_string()),
            scope: "all".to_string(),
            literal: true,
            files: Vec::new(),
            exclude: Vec::new(),
            context: "all".to_string(),
        };
        let (output, exit_code) = run(args, &crate::commands::GlobalArgs {}).expect("refs run");

        assert_eq!(exit_code, 0);
        let RefsOutput::Single(report) = output else {
            panic!("expected single refs report");
        };
        assert_eq!(report.total_references, 4);
        assert_eq!(report.actionable_references, 3);
        assert_eq!(report.homeboy_owned_references, 1);
        assert_eq!(report.counts.by_kind.get("code"), Some(&1));
        assert_eq!(report.counts.by_kind.get("string"), Some(&1));
        assert_eq!(report.counts.by_kind.get("comment"), Some(&1));
        assert_eq!(report.counts.by_kind.get("doc"), Some(&1));
        assert_eq!(
            std::fs::read_to_string(&code_path).expect("after code"),
            before_code
        );
        assert_eq!(
            std::fs::read_to_string(&doc_path).expect("after doc"),
            before_doc
        );
    }
}
