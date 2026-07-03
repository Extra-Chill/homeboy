//! Generic rig package lint checks used by `homeboy rig check` and `homeboy rig lint`.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use super::install::read_source_metadata;
use super::pipeline::{PipelineOutcome, PipelineStepOutcome};
use super::spec::RigSpec;
use crate::core::error::{Error, Result};

/// Directories the rig package lint never descends into.
///
/// This MUST stay the union of the directories the downstream
/// `homeboy-rigs` linter ignores (`scripts/lint-rig-packages.mjs`) so a rig
/// package can never pass one linter and fail the other. `.sampleplugin` holds
/// generated WP Codebox sample-plugin scaffolds; `.datamachine` is the
/// top-level Data Machine working directory the homeboy-rigs package carries.
const IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    ".claude",
    ".sampleplugin",
    ".datamachine",
    ".opencode",
    "node_modules",
    "vendor",
];

pub fn run_package_lint(rig: &RigSpec) -> Result<PipelineOutcome> {
    let Some(root) = package_lint_root(rig) else {
        return Ok(empty_outcome());
    };

    run_package_lint_at(&root)
}

pub fn run_package_lint_at(root: &Path) -> Result<PipelineOutcome> {
    if !root.is_dir() {
        return Err(Error::validation_invalid_argument(
            "source",
            "Rig package lint target must be a directory",
            Some(root.to_string_lossy().to_string()),
            None,
        ));
    }

    let mut files = Vec::new();
    collect_files(root, &mut files)?;

    let conflict_failures = conflict_marker_failures(root, &files)?;
    let json_failures = json_parse_failures(root, &files)?;
    let template_failures = template_materialization_failures(root, &files)?;
    let contract_failures = contract_validation_failures(root)?;
    let portability_failures = portability_failures(root, &files)?;
    let reference_failures = workload_reference_failures(root, &files)?;
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
            contract_failures,
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

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    collect_files_inner(root, files, "read rig package directory")
}

fn collect_files_inner(
    directory: &Path,
    files: &mut Vec<PathBuf>,
    context: &'static str,
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
            if !ignored_directory(entry.file_name().as_os_str()) {
                collect_files_inner(&path, files, context)?;
            }
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn ignored_directory(name: &OsStr) -> bool {
    IGNORED_DIRECTORIES
        .iter()
        .any(|ignored| name == OsStr::new(ignored))
}

fn conflict_marker_failures(root: &Path, files: &[PathBuf]) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for file in files {
        let content = fs::read_to_string(file).unwrap_or_default();
        for (index, line) in content.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("<<<<<<<")
                || trimmed.starts_with("=======")
                || trimmed.starts_with(">>>>>>>")
            {
                failures.push(format!(
                    "{}:{} unresolved conflict marker: {}",
                    display_relative(root, file),
                    index + 1,
                    line.trim()
                ));
            }
        }
    }
    Ok(failures)
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
        }
    }
    Ok(failures)
}

fn template_materialization_failures(root: &Path, files: &[PathBuf]) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for file in files
        .iter()
        .filter(|path| path.file_name() == Some(OsStr::new("rig.json")))
    {
        let content = fs::read_to_string(file).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", file.display())))
        })?;
        if !content.contains("\"extends\"") {
            continue;
        }
        if let Err(error) = super::install::materialize_rig_spec(file, root) {
            failures.push(format!(
                "{} template materialization failed: {}",
                display_relative(root, file),
                error.message
            ));
        }
    }
    Ok(failures)
}

fn portability_failures(root: &Path, files: &[PathBuf]) -> Result<Vec<String>> {
    let mut failures = Vec::new();
    for file in files.iter().filter(|path| portable_source_file(path)) {
        let content = fs::read_to_string(file).unwrap_or_default();
        let rel = display_relative(root, file);
        if content.contains("/Users/") {
            failures.push(format!(
                "{rel}: use $HOME, homedir(), component paths, or settings instead of hard-coded /Users paths"
            ));
        }
        if content.contains("~/Developer/") || content.contains("$HOME/Developer/") {
            failures.push(format!(
                "{rel}: use portable component path settings instead of committed ~/Developer or $HOME/Developer checkout paths"
            ));
        }
    }

    for file in files
        .iter()
        .filter(|path| path.file_name() == Some(OsStr::new("rig.json")))
    {
        let content = fs::read_to_string(file).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", file.display())))
        })?;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        failures.extend(shared_path_failures(root, file, &value));
    }

    Ok(failures)
}

fn portable_source_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(OsStr::to_str),
        Some("json" | "mjs" | "js")
    )
}

fn shared_path_failures(root: &Path, file: &Path, rig: &serde_json::Value) -> Vec<String> {
    let rel = display_relative(root, file);
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

fn workload_reference_failures(root: &Path, files: &[PathBuf]) -> Result<Vec<String>> {
    let fuzz_workloads = collect_fuzz_workloads(root, files)?;
    let mut failures = Vec::new();
    for file in files
        .iter()
        .filter(|path| path.file_name() == Some(OsStr::new("rig.json")))
    {
        let content = fs::read_to_string(file).map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", file.display())))
        })?;
        let Ok(rig) = serde_json::from_str::<serde_json::Value>(&content) else {
            continue;
        };
        let package_root = package_root_for_rig(root, file);
        let rel = display_relative(root, file);
        failures.extend(fuzz_workload_failures(
            root,
            &package_root,
            &rel,
            &rig,
            &fuzz_workloads,
        ));
        failures.extend(profile_reference_failures(
            &rel,
            &rig,
            "fuzz_workloads",
            "fuzz_profiles",
        ));
        failures.extend(bench_reference_failures(
            &rel,
            &rig,
            &fuzz_workloads,
            &package_root,
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
}
