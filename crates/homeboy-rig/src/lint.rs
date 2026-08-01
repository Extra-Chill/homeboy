//! Generic rig package lint checks used by `homeboy rig check` and `homeboy rig lint`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use super::install::read_source_metadata;
use super::pipeline::{PipelineOutcome, PipelineStepOutcome};
use super::spec::RigSpec;
use homeboy_core::error::{Error, Result};

/// Structural, dependency, and generated directories the generic rig package lint never descends into.
const STRUCTURAL_IGNORED_DIRECTORIES: &[&str] = &[
    ".claude",
    ".datamachine",
    ".git",
    ".homeboy",
    ".next",
    ".opencode",
    ".sampleplugin",
    ".venv",
    "bower_components",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "target",
    "vendor",
    "venv",
];

pub fn run_package_lint(rig: &RigSpec) -> Result<PipelineOutcome> {
    let Some(root) = package_lint_root(rig) else {
        return Ok(empty_outcome());
    };

    run_package_lint_at(&root)
}

pub fn run_package_lint_at(root: &Path) -> Result<PipelineOutcome> {
    run_package_lint_at_with_ignores(root, &[])
}

pub fn run_package_lint_at_with_ignores(
    root: &Path,
    package_ignores: &[String],
) -> Result<PipelineOutcome> {
    if !root.is_dir() {
        return Err(Error::validation_invalid_argument(
            "source",
            "Rig package lint target must be a directory",
            Some(root.to_string_lossy().to_string()),
            None,
        ));
    }

    let mut files = Vec::new();
    let mut declared_ignores = package_ignores.to_vec();
    declared_ignores.extend(package_manifest_ignores(root)?);
    let ignore_policy = IgnorePolicy::new(&declared_ignores);
    collect_files(root, &mut files, &ignore_policy)?;

    let materialized = collect_materialized_rigs(root, &files)?;
    let conflict_failures = conflict_marker_failures(root, &files)?;
    let json_failures = json_parse_failures(root, &files)?;
    let template_failures = materialized.materialization_failures.clone();
    let unknown_field_failures = unknown_top_level_field_failures(&materialized.rigs);
    let component_env_failures = component_path_env_agreement_failures(&materialized.rigs);
    let contract_failures = contract_validation_failures(root)?;
    let portability_failures = portability_failures(root, &files, &materialized.rigs)?;
    let reference_failures = workload_reference_failures(root, &files, &materialized.rigs)?;
    let steps = vec![
        aggregate_step(
            "rig-package-lint",
            "rig package has no unresolved conflict markers",
            conflict_failures,
        ),
        aggregate_step(
            "rig-package-lint",
            "rig package JSON specs parse",
            json_failures,
        ),
        aggregate_step(
            "rig-package-lint",
            "rig package template specs materialize",
            template_failures,
        ),
        aggregate_step(
            "rig-package-lint",
            "rig package specs satisfy the Homeboy rig contract",
            [
                unknown_field_failures,
                component_env_failures,
                contract_failures,
            ]
            .concat(),
        ),
        aggregate_step(
            "rig-package-lint",
            "rig package paths are portable",
            portability_failures,
        ),
        aggregate_step(
            "rig-package-lint",
            "rig package workload profiles reference declared workloads",
            reference_failures,
        ),
    ];

    Ok(PipelineOutcome {
        name: "check".to_string(),
        passed: steps.iter().filter(|step| step.status == "pass").count(),
        failed: steps.iter().filter(|step| step.status == "fail").count(),
        steps,
    })
}

const KNOWN_RIG_TOP_LEVEL_FIELDS: &[&str] = &[
    "app_launcher",
    "bench",
    "bench_profiles",
    "bench_workloads",
    "components",
    "description",
    "extends",
    "fuzz",
    "fuzz_profiles",
    "fuzz_workloads",
    "id",
    "lifecycle",
    "package_dependencies",
    "pipeline",
    "requirements",
    "resources",
    "services",
    "shared_templates",
    "shared_paths",
    "symlinks",
    "trace",
    "trace_experiments",
    "trace_guardrails",
    "trace_phase_templates",
    "trace_profiles",
    "trace_variants",
    "trace_workload_defaults",
    "trace_workloads",
];

/// One rig, resolved through its `extends` chain.
///
/// Rig-shaped lint checks run against [`MaterializedRig::value`] rather than the
/// bytes on disk. The `extends` refactor moved most rig content into
/// `*.base.json` files that a raw `rig.json` filename filter never sees, so any
/// check that reads the authored file only inspects whatever the leaf spec
/// happened to override.
struct MaterializedRig {
    /// Path to the authoring `rig.json`, used for failure attribution.
    path: PathBuf,
    /// `rig.json` path relative to the lint root.
    rel: String,
    /// Package root the rig belongs to, used to resolve `${package.root}`.
    package_root: PathBuf,
    /// The rig spec with its `extends` chain merged in.
    value: serde_json::Value,
}

struct MaterializedRigs {
    rigs: Vec<MaterializedRig>,
    materialization_failures: Vec<String>,
}

impl MaterializedRig {
    fn id(&self) -> Option<&str> {
        self.value
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())
    }
}

/// The lint unit is "each rig, materialized" — not "each JSON file".
///
/// A `*.base.json` consumed only through `extends` may legitimately be
/// incomplete on its own (no `id`, no `components`), so linting base files
/// standalone would report contract failures that are not defects. Materializing
/// each `rig.json` instead covers every base file through the rigs that actually
/// consume it, in the shape the runtime will see.
fn collect_materialized_rigs(root: &Path, files: &[PathBuf]) -> Result<MaterializedRigs> {
    let mut rigs = Vec::new();
    let mut materialization_failures = Vec::new();
    for file in files
        .iter()
        .filter(|path| path.file_name() == Some(OsStr::new("rig.json")))
    {
        let content = fs::read_to_string(file).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", file.display())))
        })?;
        // Unparseable specs are reported by the JSON parse aggregate; there is
        // nothing to materialize and nothing rig-shaped to check.
        let Ok(raw) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        let declares_extends = content.contains("\"extends\"");

        let materialized = template_source_root(root, file)
            .and_then(|source_root| super::install::materialize_rig_spec(file, &source_root));
        let value = match materialized {
            Ok(value) => value,
            Err(error) => {
                // Only rigs that actually declare `extends` can fail template
                // materialization in a way the author can act on; everything else
                // falls back to the authored spec so the rig-shaped checks below
                // still run.
                if declares_extends {
                    materialization_failures.push(format!(
                        "{} template materialization failed: {}",
                        display_relative(root, file),
                        error.message
                    ));
                }
                raw
            }
        };

        rigs.push(MaterializedRig {
            rel: display_relative(root, file),
            package_root: package_root_for_rig(root, file),
            path: file.clone(),
            value,
        });
    }
    Ok(MaterializedRigs {
        rigs,
        materialization_failures,
    })
}

fn unknown_top_level_field_failures(rigs: &[MaterializedRig]) -> Vec<String> {
    let mut failures = Vec::new();
    for rig in rigs {
        let Some(object) = rig.value.as_object() else {
            continue;
        };
        for key in object.keys() {
            if KNOWN_RIG_TOP_LEVEL_FIELDS.contains(&key.as_str()) || key.starts_with("x-") {
                continue;
            }
            failures.push(format!(
                "{}: unknown top-level rig field `{}` (use `x-*` for extension-owned metadata)",
                rig.rel, key
            ));
        }
    }
    failures
}

const COMPONENT_PATH_ENV_PREFIX: &str = "HOMEBOY_RIG_COMPONENT_PATH__";
const COMPONENT_CHECKOUT_ROOT_ENV_PREFIX: &str = "HOMEBOY_RIG_COMPONENT_CHECKOUT_ROOT__";

/// A rig's component override env vars carry the rig id in their own name, so
/// renaming a rig silently unbinds every `${env.HOMEBOY_RIG_COMPONENT_PATH__*}`
/// reference the spec makes. `${env.*}` resolves an unset name to the empty
/// string, so nothing fails — the component just points at nothing.
///
/// In chubes4/homeboy-rigs, `studio-agent-claude-trunk/rig.json` still
/// references `..._STUDIO_AGENT_CLAUDE_EVALTIMING__STUDIO` from before a rename.
/// `portability_failures` only looks for `/Users/` and `~/Developer/` literals,
/// so it sees a perfectly portable path that resolves to nothing.
///
/// Require every such reference to agree with the rig it lives in, and to name
/// a component that rig declares. The canonical names come from `expand.rs` so
/// the sanitization rule stays in exactly one place: passing an empty component
/// id yields the rig-scoped prefix every name for this rig must start with.
fn component_path_env_agreement_failures(rigs: &[MaterializedRig]) -> Vec<String> {
    let mut failures = Vec::new();
    for rig in rigs {
        // A rig with no usable id is already reported by the contract aggregate;
        // there is no canonical name to compare against.
        let Some(rig_id) = rig.id() else {
            continue;
        };
        let declared: Vec<String> = rig
            .value
            .get("components")
            .and_then(|components| components.as_object())
            .map(|components| components.keys().cloned().collect())
            .unwrap_or_default();

        let path_prefix = crate::expand::rig_component_path_override_env_name(rig_id, "");
        let checkout_prefix =
            crate::expand::rig_component_checkout_root_override_env_name(rig_id, "");

        for (pointer, name) in component_override_env_references(&rig.value) {
            let (kind, prefix, expected) = if name.starts_with(COMPONENT_PATH_ENV_PREFIX) {
                (
                    "component path",
                    &path_prefix,
                    declared
                        .iter()
                        .map(|component| {
                            crate::expand::rig_component_path_override_env_name(rig_id, component)
                        })
                        .collect::<Vec<_>>(),
                )
            } else {
                (
                    "component checkout root",
                    &checkout_prefix,
                    declared
                        .iter()
                        .map(|component| {
                            crate::expand::rig_component_checkout_root_override_env_name(
                                rig_id, component,
                            )
                        })
                        .collect::<Vec<_>>(),
                )
            };

            if !name.starts_with(prefix.as_str()) {
                failures.push(format!(
                    "{}: {} references ${{env.{name}}}, but this rig's {kind} override env vars must be named `{prefix}<COMPONENT>` for rig id `{rig_id}`; an unset name expands to the empty string",
                    rig.rel,
                    display_pointer(&pointer)
                ));
                continue;
            }
            if !expected.iter().any(|candidate| candidate == &name) {
                let component = name.trim_start_matches(prefix.as_str());
                failures.push(format!(
                    "{}: {} references ${{env.{name}}}, but rig `{rig_id}` declares no component matching `{component}` (declared: {})",
                    rig.rel,
                    display_pointer(&pointer),
                    if declared.is_empty() {
                        "none".to_string()
                    } else {
                        declared.join(", ")
                    }
                ));
            }
        }
    }
    failures
}

/// Every `${env.HOMEBOY_RIG_COMPONENT_{PATH,CHECKOUT_ROOT}__*}` reference in a
/// rig spec, paired with a JSON pointer to the string that contains it.
fn component_override_env_references(value: &serde_json::Value) -> Vec<(String, String)> {
    let mut references = Vec::new();
    collect_component_override_env_references(value, "", &mut references);
    references
}

fn collect_component_override_env_references(
    value: &serde_json::Value,
    pointer: &str,
    references: &mut Vec<(String, String)>,
) {
    match value {
        serde_json::Value::String(text) => {
            for name in env_reference_names(text) {
                if name.starts_with(COMPONENT_PATH_ENV_PREFIX)
                    || name.starts_with(COMPONENT_CHECKOUT_ROOT_ENV_PREFIX)
                {
                    references.push((pointer.to_string(), name));
                }
            }
        }
        serde_json::Value::Array(entries) => {
            for (index, entry) in entries.iter().enumerate() {
                collect_component_override_env_references(
                    entry,
                    &format!("{pointer}/{index}"),
                    references,
                );
            }
        }
        serde_json::Value::Object(entries) => {
            for (key, entry) in entries {
                collect_component_override_env_references(
                    entry,
                    &format!("{pointer}/{}", escape_pointer(key)),
                    references,
                );
            }
        }
        _ => {}
    }
}

/// Extract the `<NAME>` of every `${env.<NAME>}` interpolation in `text`.
fn env_reference_names(text: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut remaining = text;
    while let Some(start) = remaining.find("${env.") {
        let after = &remaining[start + "${env.".len()..];
        let Some(end) = after.find('}') else {
            break;
        };
        let name = after[..end].trim();
        if !name.is_empty() {
            names.push(name.to_string());
        }
        remaining = &after[end + 1..];
    }
    names
}

fn contract_validation_failures(root: &Path) -> Result<Vec<String>> {
    let rigs = match super::discover_rigs(root) {
        Ok(rigs) => rigs,
        Err(error) => return Ok(vec![error.message]),
    };

    let mut failures = Vec::new();
    for rig in rigs {
        if let Err(error) = super::load_local_source(&root.to_string_lossy(), Some(&rig.id)) {
            failures.push(format!(
                "{}: {}",
                display_relative(root, &rig.rig_path),
                error.message
            ));
        }
    }
    Ok(failures)
}

fn empty_outcome() -> PipelineOutcome {
    PipelineOutcome {
        name: "check".to_string(),
        steps: Vec::new(),
        passed: 0,
        failed: 0,
    }
}

fn package_lint_root(rig: &RigSpec) -> Option<PathBuf> {
    if let Some(package_root) = super::local_package_root(&rig.id) {
        return package_root.is_dir().then(|| package_root.clone());
    }
    let metadata = read_source_metadata(&rig.id)?;
    metadata
        .discovery_path
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(metadata.package_path)))
        .filter(|path| path.is_dir())
}

fn collect_files(
    root: &Path,
    files: &mut Vec<PathBuf>,
    ignore_policy: &IgnorePolicy,
) -> Result<()> {
    if let Some(authored_files) = collect_git_authored_files(root, ignore_policy)? {
        files.extend(authored_files);
        return Ok(());
    }

    collect_files_inner(root, files, "read rig package directory", ignore_policy)
}

fn collect_git_authored_files(
    root: &Path,
    ignore_policy: &IgnorePolicy,
) -> Result<Option<Vec<PathBuf>>> {
    let Some(git_root) = homeboy_core::git::repo_root(root) else {
        return Ok(None);
    };

    let git_root = git_root.canonicalize().unwrap_or(git_root);
    let root = root.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("canonicalize {}", root.display())),
        )
    })?;
    let Ok(relative_root) = root.strip_prefix(&git_root) else {
        return Ok(None);
    };
    let pathspec = if relative_root.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative_root.to_string_lossy().to_string()
    };
    let output = Command::new("git")
        .args(["ls-files", "-z", "--", &pathspec])
        .current_dir(&git_root)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| Error::internal_io(error.to_string(), Some("git ls-files".into())))?;
    if !output.status.success() {
        return Ok(None);
    }

    let files = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| std::str::from_utf8(entry).ok())
        .map(|entry| git_root.join(entry))
        .filter(|path| path.is_file())
        .filter(|path| !has_ignored_directory(root.as_path(), path, ignore_policy))
        .collect();
    Ok(Some(files))
}

fn collect_files_inner(
    directory: &Path,
    files: &mut Vec<PathBuf>,
    context: &'static str,
    ignore_policy: &IgnorePolicy,
) -> Result<()> {
    let entries = fs::read_dir(directory)
        .map_err(|error| Error::internal_io(error.to_string(), Some(context.to_string())))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| Error::internal_io(error.to_string(), Some(context.to_string())))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| Error::internal_io(error.to_string(), Some(context.to_string())))?;
        if file_type.is_dir() {
            if !ignore_policy.ignored_directory(entry.file_name().as_os_str()) {
                collect_files_inner(&path, files, context, ignore_policy)?;
            }
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

struct IgnorePolicy {
    directories: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
struct RigPackageLintManifest {
    #[serde(default)]
    lint_ignore_directories: Vec<String>,
}

fn package_manifest_ignores(root: &Path) -> Result<Vec<String>> {
    let manifest = root.join("homeboy-rig-package.json");
    if !manifest.is_file() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&manifest).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read {}", manifest.display())),
        )
    })?;
    let parsed: RigPackageLintManifest = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_argument(
            "homeboy-rig-package.json",
            format!("rig package lint manifest is invalid: {error}"),
            Some(manifest.to_string_lossy().to_string()),
            None,
        )
    })?;
    Ok(parsed.lint_ignore_directories)
}

impl IgnorePolicy {
    fn new(package_ignores: &[String]) -> Self {
        let mut directories = STRUCTURAL_IGNORED_DIRECTORIES
            .iter()
            .map(|value| value.to_string())
            .collect::<BTreeSet<_>>();
        directories.extend(
            package_ignores
                .iter()
                .filter(|value| !value.trim().is_empty())
                .cloned(),
        );
        Self { directories }
    }

    fn ignored_directory(&self, name: &OsStr) -> bool {
        self.directories
            .iter()
            .any(|ignored| name == OsStr::new(ignored))
    }
}

fn has_ignored_directory(root: &Path, path: &Path, ignore_policy: &IgnorePolicy) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .parent()
        .map(|parent| {
            parent
                .components()
                .any(|component| ignore_policy.ignored_directory(component.as_os_str()))
        })
        .unwrap_or(false)
}

fn conflict_marker_failures(root: &Path, files: &[PathBuf]) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for file in files {
        let content = fs::read_to_string(file).unwrap_or_default();
        let mut active_conflict: Option<(usize, &str)> = None;
        let mut saw_separator = false;
        for (index, line) in content.lines().enumerate() {
            match conflict_marker_line(line) {
                Some(ConflictMarkerLine::Start) => {
                    active_conflict = Some((index + 1, line.trim()));
                    saw_separator = false;
                }
                Some(ConflictMarkerLine::Base) if active_conflict.is_some() => {}
                Some(ConflictMarkerLine::Separator) if active_conflict.is_some() => {
                    saw_separator = true;
                }
                Some(ConflictMarkerLine::End) if active_conflict.is_some() && saw_separator => {
                    let (start_line, marker) = active_conflict.expect("active conflict");
                    failures.push(format!(
                        "{}:{} unresolved conflict marker: {}",
                        display_relative(root, file),
                        start_line,
                        marker
                    ));
                    active_conflict = None;
                    saw_separator = false;
                }
                _ => {}
            }
        }
    }
    Ok(failures)
}

enum ConflictMarkerLine {
    Start,
    Base,
    Separator,
    End,
}

fn conflict_marker_line(line: &str) -> Option<ConflictMarkerLine> {
    if line == "=======" {
        return Some(ConflictMarkerLine::Separator);
    }
    if conflict_edge_marker(line, "<<<<<<<") {
        return Some(ConflictMarkerLine::Start);
    }
    if conflict_edge_marker(line, "|||||||") {
        return Some(ConflictMarkerLine::Base);
    }
    if conflict_edge_marker(line, ">>>>>>>") {
        return Some(ConflictMarkerLine::End);
    }
    None
}

fn conflict_edge_marker(line: &str, marker: &str) -> bool {
    let Some(rest) = line.strip_prefix(marker) else {
        return false;
    };
    rest.is_empty() || rest.starts_with(char::is_whitespace)
}

fn json_parse_failures(root: &Path, files: &[PathBuf]) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for file in files
        .iter()
        .filter(|path| path.extension() == Some(OsStr::new("json")))
    {
        let content = fs::read_to_string(file).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", file.display())))
        })?;
        if let Err(error) = serde_json::from_str::<serde_json::Value>(&content) {
            failures.push(format!(
                "{} invalid JSON: {}",
                display_relative(root, file),
                error
            ));
            continue;
        }
        for duplicate in duplicate_json_keys(&content) {
            failures.push(format!(
                "{} duplicate JSON key `{}` in {} at line {} and line {}; the later value silently wins",
                display_relative(root, file),
                duplicate.key,
                display_pointer(&duplicate.pointer),
                duplicate.first_line,
                duplicate.second_line
            ));
        }
    }
    Ok(failures)
}

/// A key declared more than once inside the same JSON object.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DuplicateJsonKey {
    /// RFC 6901 pointer to the containing object.
    pointer: String,
    key: String,
    first_line: usize,
    second_line: usize,
}

struct ScanFrame {
    object: bool,
    pointer: String,
    keys: BTreeMap<String, usize>,
    index: usize,
    current_key: String,
}

/// Report keys declared more than once inside the same JSON object.
///
/// `serde_json` accepts duplicate object keys and silently keeps the last one,
/// so a rig spec can lose an entire declaration with no signal at all. In
/// chubes4/homeboy-rigs, `woocommerce-wp-codebox-target.base.json` declares
/// `"lifecycle"` twice and the first block's `intent: "external"` is dropped,
/// changing that rig's cleanup contract invisibly.
///
/// This is a raw token pass rather than a `serde` visitor because the visitor
/// API cannot report where in the source each declaration sits, and "the key
/// appears twice" is only actionable with both locations. The scan assumes the
/// input already parses — callers run it only after `serde_json` has accepted
/// the document.
fn duplicate_json_keys(content: &str) -> Vec<DuplicateJsonKey> {
    let chars: Vec<char> = content.chars().collect();
    let mut duplicates = Vec::new();
    let mut stack: Vec<ScanFrame> = Vec::new();
    let mut expect_key = false;
    let mut line = 1usize;
    let mut index = 0usize;

    while index < chars.len() {
        match chars[index] {
            '\n' => {
                line += 1;
                index += 1;
            }
            '"' => {
                let start_line = line;
                let (value, next) = read_json_string(&chars, index, &mut line);
                index = next;
                if expect_key {
                    if let Some(frame) = stack.last_mut() {
                        match frame.keys.get(&value) {
                            Some(first_line) => duplicates.push(DuplicateJsonKey {
                                pointer: frame.pointer.clone(),
                                key: value.clone(),
                                first_line: *first_line,
                                second_line: start_line,
                            }),
                            None => {
                                frame.keys.insert(value.clone(), start_line);
                            }
                        }
                        frame.current_key = value;
                    }
                    expect_key = false;
                }
            }
            open @ ('{' | '[') => {
                let pointer = child_pointer(&stack);
                stack.push(ScanFrame {
                    object: open == '{',
                    pointer,
                    keys: BTreeMap::new(),
                    index: 0,
                    current_key: String::new(),
                });
                expect_key = open == '{';
                index += 1;
            }
            '}' | ']' => {
                stack.pop();
                expect_key = false;
                index += 1;
            }
            ',' => {
                if let Some(frame) = stack.last_mut() {
                    if !frame.object {
                        frame.index += 1;
                    }
                    expect_key = frame.object;
                } else {
                    expect_key = false;
                }
                index += 1;
            }
            _ => index += 1,
        }
    }

    duplicates
}

/// Consume a JSON string literal starting at `open_quote`, returning its
/// decoded value and the index just past the closing quote.
fn read_json_string(chars: &[char], open_quote: usize, line: &mut usize) -> (String, usize) {
    let mut value = String::new();
    let mut index = open_quote + 1;
    while index < chars.len() {
        match chars[index] {
            '\\' => {
                if let Some(escaped) = chars.get(index + 1) {
                    match escaped {
                        'n' => value.push('\n'),
                        't' => value.push('\t'),
                        'r' => value.push('\r'),
                        'b' => value.push('\u{8}'),
                        'f' => value.push('\u{c}'),
                        'u' => {
                            let hex: String = chars.iter().skip(index + 2).take(4).collect();
                            if let Some(decoded) =
                                u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                            {
                                value.push(decoded);
                            }
                            index += 4;
                        }
                        other => value.push(*other),
                    }
                }
                index += 2;
            }
            '"' => return (value, index + 1),
            '\n' => {
                *line += 1;
                value.push('\n');
                index += 1;
            }
            other => {
                value.push(other);
                index += 1;
            }
        }
    }
    (value, index)
}

fn child_pointer(stack: &[ScanFrame]) -> String {
    match stack.last() {
        None => String::new(),
        Some(parent) if parent.object => {
            format!("{}/{}", parent.pointer, escape_pointer(&parent.current_key))
        }
        Some(parent) => format!("{}/{}", parent.pointer, parent.index),
    }
}

fn escape_pointer(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

fn display_pointer(pointer: &str) -> &str {
    if pointer.is_empty() {
        "the document root"
    } else {
        pointer
    }
}

fn template_source_root(root: &Path, file: &Path) -> Result<PathBuf> {
    let id = file
        .parent()
        .and_then(Path::file_name)
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_string();
    let rig = super::DiscoveredRig {
        id,
        description: String::new(),
        rig_path: file.to_path_buf(),
    };
    super::install::local_package_source_root_for_dependencies(root, &[rig])?;
    root.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("resolve rig package root {}", root.display())),
        )
    })
}

fn portability_failures(
    root: &Path,
    files: &[PathBuf],
    rigs: &[MaterializedRig],
) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for file in files.iter().filter(|path| portable_source_file(path)) {
        let content = fs::read_to_string(file).unwrap_or_default();
        let rel = display_relative(root, file);
        let scan_content = portable_scan_content(file, &content);
        if scan_content.contains("/Users/") {
            failures.push(format!(
                "{rel}: use $HOME, homedir(), component paths, or settings instead of hard-coded /Users paths"
            ));
        }
        if scan_content.contains("~/Developer/") || scan_content.contains("$HOME/Developer/") {
            failures.push(format!(
                "{rel}: use portable component path settings instead of committed ~/Developer or $HOME/Developer checkout paths"
            ));
        }
    }

    for rig in rigs {
        failures.extend(shared_path_failures(&rig.rel, &rig.value));
        failures.extend(resource_retention_failures(&rig.rel, &rig.value));
        failures.extend(lifecycle_step_source_failures(&rig.rel, &rig.value));
    }

    Ok(failures)
}

fn portable_scan_content(path: &Path, content: &str) -> String {
    if matches!(path.extension().and_then(OsStr::to_str), Some("js" | "mjs")) {
        return content
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
    }

    content.to_string()
}

fn portable_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some("json" | "mjs" | "js")
    )
}

fn shared_path_failures(rel: &str, rig: &serde_json::Value) -> Vec<String> {
    let Some(shared_paths) = rig.get("shared_paths") else {
        return Vec::new();
    };
    let Some(shared_paths) = shared_paths.as_array() else {
        return vec![format!("{rel}: shared_paths must be an array")];
    };

    let mut failures = Vec::new();
    for (index, shared_path) in shared_paths.iter().enumerate() {
        let Some(object) = shared_path.as_object() else {
            failures.push(format!("{rel}: shared_paths[{index}] must be an object"));
            continue;
        };
        let link = object
            .get("link")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim();
        let target = object
            .get("target")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim();
        let allow_self_target = object
            .get("allow_self_target")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if !link.is_empty() && link == target && !allow_self_target {
            failures.push(format!(
                "{rel}: shared_paths[{index}] link and target must differ unless allow_self_target is true"
            ));
        }
    }
    failures
}

/// A `kind: "lifecycle"` step takes its contract from exactly one source:
/// an inline `lifecycle` payload, or a `workload` reference.
///
/// Both fields are optional in the schema so either shape parses, which means
/// serde no longer rejects a step that declares neither. The executor still
/// fails closed at run time, but a rig author should not have to run `rig up`
/// to find out — that is what this lint is for.
fn lifecycle_step_source_failures(rel: &str, rig: &serde_json::Value) -> Vec<String> {
    let Some(pipelines) = rig.get("pipeline").and_then(|value| value.as_object()) else {
        return Vec::new();
    };

    let mut failures = Vec::new();
    for (name, steps) in pipelines {
        let Some(steps) = steps.as_array() else {
            continue;
        };
        for (index, step) in steps.iter().enumerate() {
            let Some(step) = step.as_object() else {
                continue;
            };
            if step.get("kind").and_then(|value| value.as_str()) != Some("lifecycle") {
                continue;
            }
            let inline = step.contains_key("lifecycle");
            let reference = step.contains_key("workload");
            let location = format!("{rel}: pipeline.{name}[{index}] lifecycle step");
            match (inline, reference) {
                (true, true) => failures.push(format!(
                    "{location} declares both `lifecycle` and `workload`; use exactly one contract source"
                )),
                (false, false) => failures.push(format!(
                    "{location} declares neither `lifecycle` nor `workload`; use exactly one contract source"
                )),
                _ => {}
            }

            if let Some(workload) = step.get("workload") {
                failures.extend(lifecycle_workload_ref_failures(&location, workload));
            }
        }
    }
    failures
}

/// Shape checks for a `workload` reference. Resolution against the declared
/// workload maps stays at run time — lint only rejects a reference that could
/// never resolve for anyone.
fn lifecycle_workload_ref_failures(location: &str, workload: &serde_json::Value) -> Vec<String> {
    let Some(workload) = workload.as_object() else {
        return vec![format!("{location} `workload` must be an object")];
    };

    let mut failures = Vec::new();
    match workload.get("kind").and_then(|value| value.as_str()) {
        Some("bench" | "fuzz" | "trace") => {}
        Some(other) => failures.push(format!(
            "{location} `workload.kind` must be one of bench, fuzz, trace (found '{other}')"
        )),
        None => failures.push(format!(
            "{location} `workload.kind` is required and must be one of bench, fuzz, trace"
        )),
    }
    let extension = workload
        .get("extension")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim();
    if extension.is_empty() {
        failures.push(format!(
            "{location} `workload.extension` is required and must be a non-empty extension id"
        ));
    }
    failures
}

/// `delete_after_ttl` without a `ttl` is contract-invalid: the lifecycle record
/// would fail validation and the run's whole lifecycle index would be dropped.
/// Catch it at lint time instead of losing the artifact at runtime.
fn resource_retention_failures(rel: &str, rig: &serde_json::Value) -> Vec<String> {
    let Some(resources) = rig.get("resources").and_then(|value| value.as_object()) else {
        return Vec::new();
    };

    let mut failures = Vec::new();

    if let Some(lifecycle) = resources.get("lifecycle") {
        failures.extend(retention_failure(rel, "resources.lifecycle", lifecycle));
    }
    if let Some(by_class) = resources.get("lifecycle_by_class") {
        match by_class.as_object() {
            Some(classes) => {
                for (class, retention) in classes {
                    if !crate::spec::RIG_RESOURCE_CLASSES.contains(&class.as_str()) {
                        failures.push(format!(
                            "{rel}: resources.lifecycle_by_class has unknown resource class '{class}'"
                        ));
                        continue;
                    }
                    failures.extend(retention_failure(
                        rel,
                        &format!("resources.lifecycle_by_class.{class}"),
                        retention,
                    ));
                }
            }
            None => failures.push(format!(
                "{rel}: resources.lifecycle_by_class must be an object"
            )),
        }
    }

    failures
}

fn retention_failure(rel: &str, label: &str, retention: &serde_json::Value) -> Option<String> {
    let Some(object) = retention.as_object() else {
        return Some(format!("{rel}: {label} must be an object"));
    };
    let policy = object
        .get("cleanup_policy")
        .and_then(|value| value.as_str());
    let ttl = object
        .get("ttl")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|ttl| !ttl.is_empty());
    if policy == Some("delete_after_ttl") && ttl.is_none() {
        return Some(format!(
            "{rel}: {label} declares delete_after_ttl without a ttl"
        ));
    }
    None
}

fn workload_reference_failures(
    root: &Path,
    files: &[PathBuf],
    rigs: &[MaterializedRig],
) -> Result<Vec<String>> {
    let fuzz_workloads = collect_fuzz_workloads(root, files)?;
    let mut failures = Vec::new();
    for rig in rigs {
        failures.extend(fuzz_workload_failures(
            root,
            &rig.package_root,
            &rig.rel,
            &rig.value,
            &fuzz_workloads,
        ));
        failures.extend(profile_reference_failures(
            &rig.rel,
            &rig.value,
            "fuzz_workloads",
            "fuzz_profiles",
        ));
        failures.extend(bench_reference_failures(
            &rig.rel,
            &rig.value,
            &fuzz_workloads,
            &rig.package_root,
        ));
    }
    Ok(failures)
}

fn collect_fuzz_workloads(
    root: &Path,
    files: &[PathBuf],
) -> Result<BTreeMap<PathBuf, BTreeMap<String, PathBuf>>> {
    let mut workloads: BTreeMap<PathBuf, BTreeMap<String, PathBuf>> = BTreeMap::new();
    for file in files
        .iter()
        .filter(|path| path.extension() == Some(OsStr::new("json")))
    {
        let Some(package_root) = package_root_for_fuzz_workload(root, file) else {
            continue;
        };
        let content = fs::read_to_string(file).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", file.display())))
        })?;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        if value.get("schema").and_then(|schema| schema.as_str())
            != Some("homeboy/fuzz-workload/v1")
        {
            continue;
        }
        let ids = workload_ids(file, &value);
        let package_workloads = workloads.entry(package_root).or_default();
        for id in ids {
            package_workloads.insert(id, file.clone());
        }
    }
    Ok(workloads)
}

fn workload_ids(file: &Path, workload: &serde_json::Value) -> BTreeSet<String> {
    let mut ids = BTreeSet::from([workload_id_from_path(file)]);
    if let Some(id) = workload.get("id").and_then(|value| value.as_str()) {
        if !id.trim().is_empty() {
            ids.insert(id.to_string());
        }
    }
    ids
}

fn fuzz_workload_failures(
    root: &Path,
    package_root: &Path,
    rel: &str,
    rig: &serde_json::Value,
    fuzz_workloads: &BTreeMap<PathBuf, BTreeMap<String, PathBuf>>,
) -> Vec<String> {
    let mut failures = Vec::new();
    let package_workloads = fuzz_workloads
        .get(package_root)
        .cloned()
        .unwrap_or_default();
    let mut rig_workload_ids = BTreeSet::new();
    let Some(workloads) = rig.get("fuzz_workloads") else {
        return failures;
    };
    let Some(workloads) = workloads.as_object() else {
        failures.push(format!("{rel}: fuzz_workloads must be an object"));
        return failures;
    };
    for (runner, declarations) in workloads {
        let Some(declarations) = declarations.as_array() else {
            failures.push(format!(
                "{rel}: fuzz_workloads {runner} must be an array of workload declarations"
            ));
            continue;
        };
        for declaration in declarations {
            let declaration_path = declaration
                .as_str()
                .or_else(|| declaration.get("path").and_then(|value| value.as_str()));
            let Some(resolved) =
                declaration_path.and_then(|path| resolve_package_path(path, package_root))
            else {
                failures.push(format!(
                    "{rel}: fuzz_workloads {runner} declaration must use a resolvable path"
                ));
                continue;
            };
            if !resolved.exists() {
                failures.push(format!(
                    "{rel}: fuzz_workloads {runner} declares missing file {}",
                    display_relative(root, &resolved)
                ));
                continue;
            }
            let workload_id = workload_id_from_path(&resolved);
            if package_workloads.get(&workload_id) != Some(&resolved) {
                failures.push(format!(
                    "{rel}: fuzz_workloads {runner} declares {}, but fuzz workload id {workload_id} is not unique within this package",
                    display_relative(root, &resolved)
                ));
            }
            if !rig_workload_ids.insert(workload_id.clone()) {
                failures.push(format!(
                    "{rel}: fuzz workload id {workload_id} is declared more than once in this rig"
                ));
            }
        }
    }
    failures
}

fn profile_reference_failures(
    rel: &str,
    rig: &serde_json::Value,
    workloads_key: &str,
    profiles_key: &str,
) -> Vec<String> {
    let declared = declared_workload_ids(rig, workloads_key);
    let mut failures = Vec::new();
    let Some(profiles) = rig.get(profiles_key) else {
        return failures;
    };
    let Some(profiles) = profiles.as_object() else {
        failures.push(format!("{rel}: {profiles_key} must be an object"));
        return failures;
    };
    for (profile, refs) in profiles {
        let Some(refs) = refs.as_array() else {
            failures.push(format!(
                "{rel}: {} profile {profile} must be an array of workload ids",
                profiles_key.trim_end_matches('s')
            ));
            continue;
        };
        for workload_ref in refs.iter().filter_map(|value| value.as_str()) {
            if !declared.contains(workload_ref) {
                failures.push(format!(
                    "{rel}: {} profile {profile} references {workload_ref}, but {workloads_key} does not declare a matching workload file",
                    profiles_key.trim_end_matches('s')
                ));
            }
        }
    }
    failures
}

fn bench_reference_failures(
    rel: &str,
    rig: &serde_json::Value,
    fuzz_workloads: &BTreeMap<PathBuf, BTreeMap<String, PathBuf>>,
    package_root: &Path,
) -> Vec<String> {
    let mut failures = profile_reference_failures(rel, rig, "bench_workloads", "bench_profiles");
    let fuzz_ids: BTreeSet<String> = fuzz_workloads
        .get(package_root)
        .map(|workloads| workloads.keys().cloned().collect())
        .unwrap_or_default();
    for workload_id in declared_workload_ids(rig, "bench_workloads") {
        if fuzz_ids.contains(&workload_id) {
            failures.push(format!(
                "{rel}: bench_workloads declares {workload_id}, but that id belongs to a fuzz workload in this package"
            ));
        }
    }
    failures
}

fn declared_workload_ids(rig: &serde_json::Value, key: &str) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    let Some(workloads) = rig.get(key).and_then(|value| value.as_object()) else {
        return ids;
    };
    for declarations in workloads.values().filter_map(|value| value.as_array()) {
        for declaration in declarations {
            let path = declaration
                .as_str()
                .or_else(|| declaration.get("path").and_then(|value| value.as_str()));
            if let Some(path) = path {
                ids.insert(workload_id_from_path(Path::new(path)));
            }
        }
    }
    ids
}

fn package_root_for_rig(root: &Path, file: &Path) -> PathBuf {
    let rel = file.strip_prefix(root).unwrap_or(file);
    let parts: Vec<_> = rel.components().collect();
    for index in 0..parts.len() {
        if parts[index].as_os_str() == OsStr::new("rigs") {
            return parts[..index]
                .iter()
                .fold(root.to_path_buf(), |path, part| path.join(part));
        }
    }
    file.parent().unwrap_or(root).to_path_buf()
}

fn package_root_for_fuzz_workload(root: &Path, file: &Path) -> Option<PathBuf> {
    let rel = file.strip_prefix(root).unwrap_or(file);
    let parts: Vec<_> = rel.components().collect();
    for index in 0..parts.len() {
        if parts[index].as_os_str() == OsStr::new("fuzz") {
            return Some(
                parts[..index]
                    .iter()
                    .fold(root.to_path_buf(), |path, part| path.join(part)),
            );
        }
    }
    None
}

fn resolve_package_path(path: &str, package_root: &Path) -> Option<PathBuf> {
    if path.trim().is_empty() {
        return None;
    }
    let expanded = path.replace("${package.root}", &package_root.to_string_lossy());
    if expanded.contains("${") {
        return None;
    }
    let path = PathBuf::from(expanded);
    Some(if path.is_absolute() {
        path
    } else {
        package_root.join(path)
    })
}

fn workload_id_from_path(path: &Path) -> String {
    let mut name = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_string();
    for suffix in [
        ".workload.json",
        ".bench.mjs",
        ".php",
        ".mjs",
        ".js",
        ".json",
    ] {
        if let Some(stripped) = name.strip_suffix(suffix) {
            name = stripped.to_string();
            break;
        }
    }
    name
}

fn aggregate_step(kind: &str, label: &str, failures: Vec<String>) -> PipelineStepOutcome {
    if failures.is_empty() {
        return PipelineStepOutcome {
            kind: kind.to_string(),
            label: label.to_string(),
            status: "pass".to_string(),
            error: None,
        };
    }

    PipelineStepOutcome {
        kind: kind.to_string(),
        label: label.to_string(),
        status: "fail".to_string(),
        error: Some(failures.join("\n")),
    }
}

fn display_relative(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run git fixture command");

        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn portability_step(outcome: &PipelineOutcome) -> &PipelineStepOutcome {
        outcome
            .steps
            .iter()
            .find(|step| step.label.contains("paths are portable"))
            .expect("portability step")
    }

    fn conflict_marker_step(outcome: &PipelineOutcome) -> &PipelineStepOutcome {
        outcome
            .steps
            .iter()
            .find(|step| step.label.contains("unresolved conflict markers"))
            .expect("conflict marker step")
    }

    #[test]
    fn aggregate_step_reports_failure_details() {
        let outcome = aggregate_step(
            "rig-package-lint",
            "JSON parses",
            vec!["rig.json invalid".to_string()],
        );
        assert_eq!(outcome.status, "fail");
        assert!(outcome.error.unwrap().contains("rig.json invalid"));
    }

    #[test]
    fn aggregate_step_passes_without_failures() {
        let outcome = aggregate_step("rig-package-lint", "JSON parses", Vec::new());
        assert_eq!(outcome.status, "pass");
        assert_eq!(outcome.error, None);
    }

    #[test]
    fn lint_ignores_homeboy_metadata_directory() {
        assert!(IgnorePolicy::new(&[]).ignored_directory(OsStr::new(".homeboy")));
    }

    #[test]
    fn lint_ignores_standard_dependency_directories() {
        let policy = IgnorePolicy::new(&[]);

        assert!(policy.ignored_directory(OsStr::new("vendor")));
        assert!(policy.ignored_directory(OsStr::new("node_modules")));
    }

    #[test]
    fn lint_ignores_shared_rig_package_metadata_directories() {
        let policy = IgnorePolicy::new(&[]);

        for directory in [".claude", ".datamachine", ".opencode", ".sampleplugin"] {
            assert!(
                policy.ignored_directory(OsStr::new(directory)),
                "{directory} should be ignored by the generic package walk"
            );
        }
    }

    #[test]
    fn conflict_marker_scan_ignores_banners_headings_and_vendored_content() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let fixture_dir = temp.path().join("tests").join("fixtures");
        let vendor_dir = temp
            .path()
            .join("vendor")
            .join("psr")
            .join("event-dispatcher");
        fs::create_dir_all(&fixture_dir).expect("fixture dir");
        fs::create_dir_all(&vendor_dir).expect("vendor dir");
        fs::write(
            fixture_dir.join("banner.css"),
            "/* ================================ */\n/* <<<<<<< visual separator >>>>>>> */\n",
        )
        .expect("write fixture css");
        fs::write(
            fixture_dir.join("README.md"),
            "Package Fixture\n=======\n\nNot a conflict block.\n",
        )
        .expect("write fixture markdown");
        fs::write(
            vendor_dir.join("README.md"),
            "<<<<<<< dependency docs\n=======\n>>>>>>> dependency docs\n",
        )
        .expect("write vendor readme");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");

        assert_eq!(conflict_marker_step(&outcome).status, "pass");
    }

    #[test]
    fn conflict_marker_scan_reports_real_git_conflict_blocks() {
        let temp = tempfile::TempDir::new().expect("temp package");
        fs::write(
            temp.path().join("rig.json"),
            "<<<<<<< HEAD\n{\"id\":\"ours\"}\n=======\n{\"id\":\"theirs\"}\n>>>>>>> main\n",
        )
        .expect("write conflicted rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let step = conflict_marker_step(&outcome);

        assert_eq!(step.status, "fail");
        assert!(step
            .error
            .as_ref()
            .expect("conflict marker error")
            .contains("rig.json:1 unresolved conflict marker: <<<<<<< HEAD"));
    }

    #[test]
    fn portability_scan_ignores_js_line_comment_path_examples() {
        let content = "// local path example: /Users/chubes/Developer/project\nconst path = process.env.HOME;\n";
        let scanned = portable_scan_content(Path::new("tools/run-fixture-matrix.mjs"), content);

        assert!(!scanned.contains("/Users/"));
    }

    #[test]
    fn portability_scan_keeps_js_string_paths_actionable() {
        let content = "const path = '/Users/chubes/Developer/project';\n";
        let scanned = portable_scan_content(Path::new("tools/run-fixture-matrix.mjs"), content);

        assert!(scanned.contains("/Users/"));
    }

    #[test]
    fn package_lint_reports_contract_failures_for_all_rigs() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("bad");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "bad",
                "requirements": {
                    "dependency_materialization": [
                        { "id": "bad", "inputs": { "paths": ["artifact.txt"] } }
                    ]
                }
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");

        let contract_step = outcome
            .steps
            .iter()
            .find(|step| step.label.contains("Homeboy rig contract"))
            .expect("contract step");
        assert_eq!(contract_step.status, "fail");
        assert!(contract_step
            .error
            .as_ref()
            .expect("contract error")
            .contains("dependency materialization"));
    }

    #[test]
    fn package_lint_rejects_unsupported_component_interpolation_in_materialization_cwd() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("bad");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "bad",
                "components": { "jetpack": { "path": "/tmp/jetpack" } },
                "requirements": {
                    "dependency_materialization": [{
                        "id": "install",
                        "command": "npm install",
                        "cwd": "${components.jetpack.checkout_root}",
                        "safety": "writes_working_tree"
                    }]
                }
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let contract_step = outcome
            .steps
            .iter()
            .find(|step| step.label.contains("Homeboy rig contract"))
            .expect("contract step");

        assert_eq!(contract_step.status, "fail");
        let error = contract_step.error.as_ref().expect("contract error");
        assert!(error.contains("${components.jetpack.checkout_root}"));
        assert!(error.contains("${components.<id>.path}"));
    }

    #[test]
    fn package_lint_reports_unknown_top_level_rig_fields() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("bad");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "bad",
                "components": {},
                "lifecycle": { "cleanup": "dry_run" },
                "trace": { "default_component": "app" },
                "x-owner": { "team": "fixtures" },
                "typoed_field": true
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let contract_step = outcome
            .steps
            .iter()
            .find(|step| step.label.contains("Homeboy rig contract"))
            .expect("contract step");

        assert_eq!(contract_step.status, "fail");
        let error = contract_step.error.as_ref().expect("contract error");
        assert!(error.contains("unknown top-level rig field `typoed_field`"));
        assert!(!error.contains("x-owner"));
        assert!(!error.contains("lifecycle"));
        assert!(!error.contains("trace"));
    }

    #[test]
    fn package_lint_materializes_extends_from_declared_repo_shared_root() {
        let temp = tempfile::TempDir::new().expect("temp repo");
        git(temp.path(), &["init", "--quiet"]);
        let package = temp.path().join("Product").join("plugin");
        let rig_dir = package.join("rigs").join("browser-coverage");
        let shared = temp.path().join("shared").join("wordpress-plugin");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::create_dir_all(&shared).expect("shared dir");
        fs::write(
            shared.join("browser-coverage.base.json"),
            r#"{
                "components": {
                    "plugin": { "path": "${env.PLUGIN_PATH}" }
                },
                "trace": { "default_component": "plugin" },
                "trace_workloads": {
                    "nodejs": [
                        { "path": "${package.root}/bench/browser-coverage.trace.mjs" }
                    ]
                }
            }"#,
        )
        .expect("write shared base");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "browser-coverage",
                "shared_templates": ["../../../../shared/wordpress-plugin"],
                "extends": "../../../../shared/wordpress-plugin/browser-coverage.base.json",
                "trace_profiles": { "smoke": { "scenario": "browser-coverage" } }
            }"#,
        )
        .expect("write rig");
        fs::create_dir_all(package.join("bench")).expect("bench dir");
        fs::write(
            package.join("bench/browser-coverage.trace.mjs"),
            "// fixture\n",
        )
        .expect("workload");
        git(temp.path(), &["add", "."]);

        let outcome = run_package_lint_at(&package).expect("lint package");
        let template_step = outcome
            .steps
            .iter()
            .find(|step| step.label.contains("template specs materialize"))
            .expect("template step");
        let contract_step = outcome
            .steps
            .iter()
            .find(|step| step.label.contains("Homeboy rig contract"))
            .expect("contract step");

        assert_eq!(template_step.status, "pass");
        assert_eq!(
            contract_step.status, "pass",
            "contract error: {:?}",
            contract_step.error
        );
    }

    fn json_step(outcome: &PipelineOutcome) -> &PipelineStepOutcome {
        outcome
            .steps
            .iter()
            .find(|step| step.label.contains("JSON specs parse"))
            .expect("json step")
    }

    #[test]
    fn duplicate_json_keys_reports_both_declaration_lines() {
        let content = "{\n  \"lifecycle\": {\n    \"cleanup\": { \"intent\": \"external\" }\n  },\n  \"id\": \"x\",\n  \"lifecycle\": {\n    \"cleanup\": \"dry_run\"\n  }\n}\n";

        let duplicates = duplicate_json_keys(content);

        assert_eq!(
            duplicates,
            vec![DuplicateJsonKey {
                pointer: String::new(),
                key: "lifecycle".to_string(),
                first_line: 2,
                second_line: 6,
            }]
        );
    }

    #[test]
    fn duplicate_json_keys_points_at_the_containing_object() {
        let nested = duplicate_json_keys("{\n  \"a\": {\n    \"b\": 1,\n    \"b\": 2\n  }\n}\n");
        assert_eq!(nested.len(), 1, "{nested:?}");
        assert_eq!(nested[0].pointer, "/a");

        let in_array = duplicate_json_keys(
            "{\n  \"list\": [\n    { \"k\": 1 },\n    { \"k\": 1, \"k\": 2 }\n  ]\n}\n",
        );
        assert_eq!(in_array.len(), 1, "{in_array:?}");
        assert_eq!(in_array[0].pointer, "/list/1");
    }

    #[test]
    fn duplicate_json_keys_ignores_object_syntax_inside_string_values() {
        let content = "{\n  \"cmd\": \"{ \\\"a\\\": 1, \\\"a\\\": 2 }\",\n  \"note\": \"he said \\\"hi\\\" }\",\n  \"other\": \"[,{\",\n  \"a\": 1\n}\n";

        assert!(duplicate_json_keys(content).is_empty());
    }

    #[test]
    fn duplicate_json_keys_accepts_a_clean_document() {
        let content = "{\n  \"a\": 1,\n  \"b\": { \"a\": 2 },\n  \"c\": [1, 2, {\"a\": 3}]\n}\n";

        assert!(duplicate_json_keys(content).is_empty());
    }

    #[test]
    fn duplicate_json_keys_decodes_escaped_key_names() {
        // `"a\u0062"` and `"ab"` are the same key; serde_json keeps only the last.
        let duplicates = duplicate_json_keys("{\n  \"a\\u0062\": 1,\n  \"ab\": 2\n}\n");

        assert_eq!(duplicates.len(), 1, "{duplicates:?}");
        assert_eq!(duplicates[0].key, "ab");
    }

    #[test]
    fn package_lint_reports_duplicate_keys_in_a_rig_spec() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("duplicate");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            "{\n  \"id\": \"duplicate\",\n  \"lifecycle\": {\n    \"cleanup\": { \"intent\": \"external\", \"reason\": \"sandbox owns teardown\" }\n  },\n  \"lifecycle\": {\n    \"cleanup\": \"dry_run\"\n  }\n}\n",
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let step = json_step(&outcome);

        assert_eq!(step.status, "fail");
        let error = step.error.as_ref().expect("json error");
        assert!(error.contains("duplicate JSON key `lifecycle`"), "{error}");
        assert!(error.contains("line 3 and line 6"), "{error}");
    }

    #[test]
    fn package_lint_reports_duplicate_keys_in_a_base_spec() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("duplicate");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("duplicate.base.json"),
            "{\n  \"trace\": { \"default_component\": \"app\" },\n  \"trace\": { \"default_component\": \"other\" }\n}\n",
        )
        .expect("write base");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{ "extends": "./duplicate.base.json", "id": "duplicate" }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let step = json_step(&outcome);

        assert_eq!(step.status, "fail");
        assert!(step
            .error
            .as_ref()
            .expect("json error")
            .contains("duplicate.base.json duplicate JSON key `trace`"));
    }

    fn contract_step(outcome: &PipelineOutcome) -> &PipelineStepOutcome {
        outcome
            .steps
            .iter()
            .find(|step| step.label.contains("Homeboy rig contract"))
            .expect("contract step")
    }

    #[test]
    fn env_reference_names_extracts_every_interpolation() {
        let names = env_reference_names(
            "${env.HOMEBOY_RIG_COMPONENT_PATH__A__B}/x/${env.HOMEBOY_SETTINGS_NS} ${broken",
        );

        assert_eq!(
            names,
            vec![
                "HOMEBOY_RIG_COMPONENT_PATH__A__B".to_string(),
                "HOMEBOY_SETTINGS_NS".to_string()
            ]
        );
    }

    #[test]
    fn package_lint_reports_component_env_var_naming_a_different_rig() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("studio-agent-claude-trunk");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "studio-agent-claude-trunk",
                "components": {
                    "studio": {
                        "path": "${env.HOMEBOY_RIG_COMPONENT_PATH__STUDIO_AGENT_CLAUDE_EVALTIMING__STUDIO}"
                    }
                }
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let step = contract_step(&outcome);

        assert_eq!(step.status, "fail");
        let error = step.error.as_ref().expect("contract error");
        assert!(
            error.contains("HOMEBOY_RIG_COMPONENT_PATH__STUDIO_AGENT_CLAUDE_EVALTIMING__STUDIO"),
            "{error}"
        );
        assert!(
            error.contains("`HOMEBOY_RIG_COMPONENT_PATH__STUDIO_AGENT_CLAUDE_TRUNK__<COMPONENT>`"),
            "{error}"
        );
        assert!(error.contains("/components/studio/path"), "{error}");
    }

    #[test]
    fn package_lint_reports_component_env_var_naming_an_undeclared_component() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("agreement");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "agreement",
                "components": {
                    "studio": { "path": "/tmp/studio" }
                },
                "resources": {
                    "paths": ["${env.HOMEBOY_RIG_COMPONENT_CHECKOUT_ROOT__AGREEMENT__WORDPRESS}"]
                }
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let step = contract_step(&outcome);

        assert_eq!(step.status, "fail");
        let error = step.error.as_ref().expect("contract error");
        assert!(
            error.contains("declares no component matching `WORDPRESS`"),
            "{error}"
        );
        assert!(error.contains("declared: studio"), "{error}");
    }

    #[test]
    fn package_lint_accepts_component_env_vars_that_agree_with_the_rig_id() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("gutenberg-pattern-assets");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "gutenberg-pattern-assets",
                "components": {
                    "gutenberg": {
                        "path": "${env.HOMEBOY_RIG_COMPONENT_PATH__GUTENBERG_PATTERN_ASSETS__GUTENBERG}"
                    }
                },
                "resources": {
                    "exclusive": ["ns:${env.HOMEBOY_SETTINGS_NAMESPACE}"]
                }
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let step = contract_step(&outcome);

        assert_eq!(step.status, "pass", "{:?}", step.error);
    }

    #[test]
    fn package_lint_checks_component_env_agreement_through_extends() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("renamed");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("renamed.base.json"),
            r#"{
                "components": {
                    "app": { "path": "${env.HOMEBOY_RIG_COMPONENT_PATH__OLD_NAME__APP}" }
                }
            }"#,
        )
        .expect("write base");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{ "extends": "./renamed.base.json", "id": "renamed" }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let step = contract_step(&outcome);

        assert_eq!(step.status, "fail");
        assert!(step
            .error
            .as_ref()
            .expect("contract error")
            .contains("`HOMEBOY_RIG_COMPONENT_PATH__RENAMED__<COMPONENT>`"));
    }

    #[test]
    fn package_lint_reports_rig_shaped_failures_inherited_from_a_base_spec() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("inherited");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("inherited.base.json"),
            r#"{
                "components": {},
                "shared_paths": [{"link":"shared","target":"shared"}],
                "resources": {
                    "lifecycle_by_class": {
                        "ports": {"cleanup_policy": "delete_after_ttl"}
                    }
                },
                "pipeline": {
                    "up": [{ "kind": "lifecycle", "op": "prepare" }]
                },
                "typoed_field": true
            }"#,
        )
        .expect("write base");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "extends": "./inherited.base.json",
                "id": "inherited"
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");

        let portability = portability_step(&outcome);
        assert_eq!(portability.status, "fail", "{:?}", portability.error);
        let error = portability.error.as_ref().expect("portability error");
        assert!(error.contains("shared_paths[0]"), "{error}");
        assert!(error.contains("delete_after_ttl without a ttl"), "{error}");
        assert!(
            error.contains("declares neither `lifecycle` nor `workload`"),
            "{error}"
        );

        let contract = outcome
            .steps
            .iter()
            .find(|step| step.label.contains("Homeboy rig contract"))
            .expect("contract step");
        assert!(contract
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("unknown top-level rig field `typoed_field`"));
    }

    #[test]
    fn package_lint_does_not_lint_base_specs_as_standalone_rigs() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("inherited");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        // A base spec has no `id` and no `components` of its own. Linting it as a
        // standalone rig would report contract failures that are not defects.
        fs::write(
            rig_dir.join("inherited.base.json"),
            r#"{ "trace": { "default_component": "app" } }"#,
        )
        .expect("write base");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "extends": "./inherited.base.json",
                "id": "inherited",
                "components": { "app": { "path": "/tmp/app" } }
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");

        assert_eq!(outcome.failed, 0, "{:?}", outcome.steps);
    }

    #[test]
    fn package_lint_reports_portable_path_failures() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("portable");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "portable",
                "shared_paths": [{"link":"shared","target":"shared"}],
                "pipeline": {"check": [{"command": "ls ~/Developer/example"}]}
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let portability_step = outcome
            .steps
            .iter()
            .find(|step| step.label.contains("paths are portable"))
            .expect("portability step");

        assert_eq!(portability_step.status, "fail");
        let error = portability_step.error.as_ref().expect("error");
        assert!(error.contains("~/Developer"));
        assert!(error.contains("shared_paths[0]"));
    }

    #[test]
    fn package_lint_reports_delete_after_ttl_without_ttl() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("retention");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "retention",
                "resources": {
                    "ports": [9724],
                    "lifecycle_by_class": {
                        "ports": {"cleanup_policy": "delete_after_ttl"},
                        "sockets": {"ttl": "PT1H"}
                    }
                }
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let step = portability_step(&outcome);

        assert_eq!(step.status, "fail");
        let error = step.error.as_ref().expect("error");
        assert!(error.contains("delete_after_ttl without a ttl"));
        assert!(error.contains("unknown resource class 'sockets'"));
    }

    #[test]
    fn package_lint_accepts_declared_resource_retention() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("retention");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "retention",
                "resources": {
                    "ports": [9724],
                    "lifecycle": {"cleanup_policy": "delete_on_terminal"},
                    "lifecycle_by_class": {
                        "ports": {"ttl": "PT1H", "cleanup_policy": "delete_after_ttl"}
                    }
                }
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let step = portability_step(&outcome);

        assert!(
            !step
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("delete_after_ttl"),
            "a ttl-backed delete_after_ttl declaration is valid: {:?}",
            step.error
        );
    }

    #[test]
    fn package_lint_ignores_gitignored_untracked_generated_files() {
        let temp = tempfile::TempDir::new().expect("temp package");
        git(temp.path(), &["init", "--quiet"]);
        let rig_dir = temp.path().join("rigs").join("portable");
        let artifact_dir = temp.path().join("artifacts");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(temp.path().join(".gitignore"), "artifacts/\n").expect("write gitignore");
        fs::write(rig_dir.join("rig.json"), r#"{ "id": "portable" }"#).expect("write rig");
        fs::write(
            artifact_dir.join("run.homeboy-bench.json"),
            r#"{ "path": "/Users/chubes/Developer/static-site-importer" }"#,
        )
        .expect("write generated artifact");
        git(
            temp.path(),
            &["add", ".gitignore", "rigs/portable/rig.json"],
        );

        let outcome = run_package_lint_at(temp.path()).expect("lint package");

        assert_eq!(portability_step(&outcome).status, "pass");
    }

    #[test]
    fn package_lint_reports_tracked_authored_portability_failures() {
        let temp = tempfile::TempDir::new().expect("temp package");
        git(temp.path(), &["init", "--quiet"]);
        let rig_dir = temp.path().join("rigs").join("portable");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{ "id": "portable", "pipeline": { "check": [{ "command": "ls /Users/chubes/project" }] } }"#,
        )
        .expect("write rig");
        git(temp.path(), &["add", "rigs/portable/rig.json"]);

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let portability_step = portability_step(&outcome);

        assert_eq!(portability_step.status, "fail");
        assert!(portability_step
            .error
            .as_ref()
            .expect("portability error")
            .contains("/Users paths"));
    }

    #[test]
    fn package_lint_reports_profile_reference_failures() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("profiles");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "profiles",
                "fuzz_workloads": {"node": ["fuzz/read.workload.json"]},
                "fuzz_profiles": {"missing": ["write"]},
                "bench_workloads": {"node": ["bench/read.bench.mjs"]},
                "bench_profiles": {"missing": ["slow"]}
            }"#,
        )
        .expect("write rig");
        let fuzz_dir = temp.path().join("fuzz");
        fs::create_dir_all(&fuzz_dir).expect("fuzz dir");
        fs::write(
            fuzz_dir.join("read.workload.json"),
            r#"{"schema":"homeboy/fuzz-workload/v1","id":"read"}"#,
        )
        .expect("write workload");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let reference_step = outcome
            .steps
            .iter()
            .find(|step| step.label.contains("profiles reference declared workloads"))
            .expect("reference step");

        assert_eq!(reference_step.status, "fail");
        let error = reference_step.error.as_ref().expect("error");
        assert!(error.contains("fuzz_profile profile missing references write"));
        assert!(error.contains("bench_profile profile missing references slow"));
    }

    #[test]
    fn package_lint_reports_lifecycle_step_without_a_contract_source() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("lifecycle");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "lifecycle",
                "pipeline": {
                    "up": [{ "kind": "lifecycle", "op": "prepare" }]
                }
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let step = portability_step(&outcome);

        assert_eq!(step.status, "fail");
        let error = step.error.as_ref().expect("error");
        assert!(
            error.contains("declares neither `lifecycle` nor `workload`"),
            "{error}"
        );
        assert!(error.contains("pipeline.up[0]"), "{error}");
    }

    #[test]
    fn package_lint_reports_lifecycle_step_with_two_contract_sources() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("lifecycle");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "lifecycle",
                "pipeline": {
                    "up": [{
                        "kind": "lifecycle",
                        "lifecycle": { "phases": [] },
                        "workload": { "kind": "fuzz", "extension": "generic" }
                    }]
                }
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let step = portability_step(&outcome);

        assert_eq!(step.status, "fail");
        let error = step.error.as_ref().expect("error");
        assert!(
            error.contains("declares both `lifecycle` and `workload`"),
            "{error}"
        );
    }

    #[test]
    fn package_lint_reports_malformed_lifecycle_workload_reference() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("lifecycle");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "lifecycle",
                "pipeline": {
                    "up": [{
                        "kind": "lifecycle",
                        "workload": { "kind": "sandbox", "extension": "  " }
                    }]
                }
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let step = portability_step(&outcome);

        assert_eq!(step.status, "fail");
        let error = step.error.as_ref().expect("error");
        assert!(error.contains("`workload.kind` must be one of"), "{error}");
        assert!(
            error.contains("`workload.extension` is required"),
            "{error}"
        );
    }

    #[test]
    fn package_lint_accepts_both_lifecycle_contract_sources_used_alone() {
        let temp = tempfile::TempDir::new().expect("temp package");
        let rig_dir = temp.path().join("rigs").join("lifecycle");
        fs::create_dir_all(&rig_dir).expect("rig dir");
        fs::write(
            rig_dir.join("rig.json"),
            r#"{
                "id": "lifecycle",
                "fuzz_workloads": {
                    "generic": [{
                        "path": "fuzz/a.workload.json",
                        "lifecycle": {
                            "phases": [{ "id": "make", "phase": "prepare", "command": "true" }]
                        }
                    }]
                },
                "pipeline": {
                    "up": [{
                        "kind": "lifecycle",
                        "workload": { "kind": "fuzz", "extension": "generic" }
                    }],
                    "down": [{
                        "kind": "lifecycle",
                        "op": "teardown",
                        "lifecycle": {
                            "phases": [{ "id": "reap", "phase": "teardown", "command": "true" }]
                        }
                    }]
                }
            }"#,
        )
        .expect("write rig");

        let outcome = run_package_lint_at(temp.path()).expect("lint package");
        let step = portability_step(&outcome);

        assert_eq!(step.status, "pass", "{:?}", step.error);
    }
}
