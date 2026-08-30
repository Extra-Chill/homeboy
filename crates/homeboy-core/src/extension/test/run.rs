use crate::extension;
use crate::extension::runner::tail_lines;
use crate::extension::test::analyze::{analyze, TestAnalysisInput};
use crate::extension::test::baseline::{self, TestCounts};
use crate::extension::test::durations::{
    build_test_durations, parse_duration_samples, parse_test_durations_file, SlowTestPolicy,
    TestDurations,
};
use crate::extension::test::{
    build_test_runner, build_test_summary, compute_changed_test_scope,
    normalize_test_passthrough_args, parse_coverage_file, parse_failures_file,
    parse_test_results_file_with_spec, parse_test_results_text, parse_test_results_text_with_spec,
};
use crate::extension::{ExtensionCapability, ExtensionPhaseTiming};
use homeboy_core::component::Component;
use homeboy_core::engine::run_dir::{self, RunDir};
use homeboy_core::error::{Error, ErrorCode};
use homeboy_core::finding::HomeboyFinding;
use homeboy_core::observation::homeboy_findings_from_test_analysis_input;
use homeboy_core::validation_progress::{write_command_artifact, ValidationProgressRecorder};
use homeboy_engine_primitives::baseline::BaselineFlags;
use homeboy_engine_primitives::local_files;
use homeboy_engine_primitives::measurement::{Measurement, Verdict};
use homeboy_engine_primitives::output_parse::ParseSpec;
pub use homeboy_extension_contract::test_results::{
    TestInventoryOutput, TestInventoryRejection, TestRunWorkflowResult, TestRuntimeEvidence,
    TestRuntimeIdentity,
};
pub use homeboy_extension_contract::test_workflow::RawTestOutput;
use homeboy_refactor_contract::AppliedRefactor;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[cfg(unix)]
use std::io::{Read, Write};

#[derive(Debug, Clone)]
pub struct TestRunWorkflowArgs {
    pub component_label: String,
    pub component_id: String,
    pub path_override: Option<String>,
    pub settings: Vec<(String, String)>,
    pub settings_json: Vec<(String, serde_json::Value)>,
    pub skip_lint: bool,
    pub coverage: bool,
    pub coverage_min: Option<f64>,
    pub analyze: bool,
    pub baseline_flags: BaselineFlags,
    pub changed_since: Option<String>,
    pub precomputed_changed_files: Option<Vec<String>>,
    pub json_summary: bool,
    pub restore_checkout: bool,
    pub ci_env: Vec<(String, String)>,
    pub passthrough_args: Vec<String>,
}

const RAW_OUTPUT_TAIL_LINES: usize = 80;
const COMPILER_FAILURE_LIMIT: usize = 20;
const NO_TESTS_APPLICABLE_SCHEMA: &str = "homeboy/no-tests-applicable/v1";
const NO_TESTS_APPLICABLE_FILE_ENV: &str = "HOMEBOY_NO_TESTS_APPLICABLE_FILE";
const NO_TESTS_APPLICABLE_NONCE_ENV: &str = "HOMEBOY_NO_TESTS_APPLICABLE_NONCE";
const NO_TESTS_APPLICABLE_EXTENSION_ENV: &str = "HOMEBOY_NO_TESTS_APPLICABLE_EXTENSION_ID";
const NO_TESTS_APPLICABLE_STEP: &str = "test";
const TEST_INVENTORY_ONLY_ENV: &str = "HOMEBOY_TEST_INVENTORY_ONLY";
const TEST_INVENTORY_FILE_ENV: &str = "HOMEBOY_TEST_INVENTORY_FILE";
const TEST_SHARD_MANIFEST_ENV: &str = "HOMEBOY_TEST_SHARD_MANIFEST";
const TEST_INVENTORY_SCHEMA: &str = "homeboy/test-inventory/v1";
#[cfg(unix)]
const TEST_INVENTORY_FILE: &str = "test-inventory.json";
#[cfg(unix)]
const TEST_INVENTORY_PUBLIC_FILE: &str = "homeboy-test-inventory.json";
#[cfg(unix)]
const MAX_TEST_INVENTORY_BYTES: u64 = 64 * 1024 * 1024;
const MAX_CHANGED_TEST_FILES_ENV: &str = "HOMEBOY_MAX_CHANGED_TEST_FILES";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TestShardManifest {
    schema: String,
    id: String,
    runner: String,
    runner_fingerprint: String,
    workspace_fingerprint: String,
    inventory_fingerprint: String,
    tests: Vec<String>,
    #[serde(default, rename = "estimated_duration_ms")]
    _estimated_duration_ms: Option<u64>,
}

#[derive(Debug)]
struct RuntimeTestPlan {
    runner: String,
    runner_fingerprint: String,
    workspace_fingerprint: String,
    execution_fingerprint: String,
    tests: Vec<String>,
}

fn runtime_test_evidence(
    ci_env: &[(String, String)],
    source_path: &Path,
    inventory_profile: Option<&InventoryProfile>,
    runner_succeeded: bool,
    counts: Option<&TestCounts>,
    failures: Option<&TestAnalysisInput>,
    internal_plan: Option<Result<RuntimeTestPlan, String>>,
) -> TestRuntimeEvidence {
    let invalid = |reason: &str| TestRuntimeEvidence::InvalidEvidence {
        reason: reason.to_string(),
    };
    let plan = match runtime_test_plan(ci_env, source_path, inventory_profile, internal_plan) {
        Ok(plan) => plan,
        Err(reason) => return invalid(&reason),
    };

    let mut failed_test_ids = failures
        .map(|input| {
            input
                .failures
                .iter()
                .map(|failure| failure.test_name.trim().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if failed_test_ids
        .iter()
        .any(|id| id.is_empty() || id == "unknown test")
    {
        return invalid("failed-test detail parser did not expose stable test IDs");
    }
    failed_test_ids.sort();
    if failed_test_ids.windows(2).any(|ids| ids[0] == ids[1]) {
        return invalid("failed-test detail parser exposed duplicate test IDs");
    }
    if failed_test_ids
        .iter()
        .any(|id| plan.tests.binary_search(id).is_err())
    {
        return invalid("observed failed test ID is absent from the runtime plan");
    }
    if !runner_succeeded {
        let Some(counts) = counts else {
            return invalid("failed test command did not expose structured TestCounts");
        };
        if failed_test_ids.is_empty() {
            return invalid("failed test command did not expose an observed failed test ID");
        }
        if counts.failed as usize != failed_test_ids.len() {
            return invalid("failed-test detail IDs do not account for every reported failure");
        }
    } else if !failed_test_ids.is_empty() {
        return invalid("successful test command exposed failed-test detail IDs");
    }

    TestRuntimeEvidence::Complete {
        runner: plan.runner,
        runner_fingerprint: plan.runner_fingerprint,
        workspace_fingerprint: plan.workspace_fingerprint,
        execution_fingerprint: plan.execution_fingerprint,
        tests: plan
            .tests
            .into_iter()
            .map(|id| TestRuntimeIdentity { id })
            .collect(),
        failed_test_ids,
    }
}

fn runtime_test_plan(
    ci_env: &[(String, String)],
    source_path: &Path,
    inventory_profile: Option<&InventoryProfile>,
    internal_plan: Option<Result<RuntimeTestPlan, String>>,
) -> Result<RuntimeTestPlan, String> {
    if let Some(path) = runtime_evidence_path(ci_env, TEST_SHARD_MANIFEST_ENV) {
        let Some(profile) = inventory_profile else {
            return Err("runtime shard manifest cannot be bound to this extension".to_string());
        };
        return runtime_plan_from_shard(path, source_path, profile);
    }
    let Some(path) = runtime_evidence_path(ci_env, TEST_INVENTORY_FILE_ENV) else {
        return internal_plan.unwrap_or_else(|| {
            Err("test adapter did not provide an exact runtime test plan".to_string())
        });
    };
    let Some(profile) = inventory_profile else {
        return Err("test adapter inventory cannot be bound to this extension".to_string());
    };
    runtime_plan_from_inventory(path, source_path, profile)
}

fn runtime_evidence_path<'a>(ci_env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    ci_env
        .iter()
        .find_map(|(candidate, value)| (candidate == key).then_some(value.trim()))
        .filter(|path| !path.is_empty())
}

fn runtime_plan_from_shard(
    path: &str,
    source_path: &Path,
    profile: &InventoryProfile,
) -> Result<RuntimeTestPlan, String> {
    let raw = std::fs::read(path)
        .map_err(|_| "runtime shard manifest is missing or unreadable".to_string())?;
    let mut manifest = serde_json::from_slice::<TestShardManifest>(&raw)
        .map_err(|_| "runtime shard manifest is malformed".to_string())?;
    if manifest.schema != "homeboy/test-shard-manifest/v1"
        || manifest.id.trim().is_empty()
        || manifest.runner.trim().is_empty()
        || !canonical_sha256(&manifest.runner_fingerprint)
        || !canonical_sha256(&manifest.workspace_fingerprint)
        || !canonical_sha256(&manifest.inventory_fingerprint)
        || manifest.tests.is_empty()
        || manifest.tests.iter().any(|id| id.trim().is_empty())
    {
        return Err("runtime shard manifest violates the identity/provenance contract".to_string());
    }
    manifest
        .tests
        .iter_mut()
        .for_each(|id| *id = id.trim().to_string());
    manifest.tests.sort();
    if manifest.tests.windows(2).any(|ids| ids[0] == ids[1]) {
        return Err("runtime shard manifest contains duplicate test IDs".to_string());
    }
    let workspace_root = inventory_workspace_root(source_path, profile)
        .ok_or_else(|| "runtime shard manifest workspace cannot be resolved".to_string())?;
    if workspace_fingerprint(&workspace_root, profile).as_deref()
        != Some(manifest.workspace_fingerprint.as_str())
        || runner_fingerprint(&workspace_root, &manifest.runner, profile).as_deref()
            != Some(manifest.runner_fingerprint.as_str())
    {
        return Err(
            "runtime shard manifest provenance does not match the current execution".to_string(),
        );
    }
    let execution_fingerprint = execution_fingerprint(
        "shard_manifest",
        &manifest.id,
        &manifest.runner,
        &manifest.runner_fingerprint,
        &manifest.workspace_fingerprint,
        &manifest.inventory_fingerprint,
        &manifest.tests,
    );
    Ok(RuntimeTestPlan {
        runner: manifest.runner,
        runner_fingerprint: manifest.runner_fingerprint,
        workspace_fingerprint: manifest.workspace_fingerprint,
        execution_fingerprint,
        tests: manifest.tests,
    })
}

fn runtime_plan_from_inventory(
    path: &str,
    source_path: &Path,
    profile: &InventoryProfile,
) -> Result<RuntimeTestPlan, String> {
    let raw = std::fs::read(path)
        .map_err(|_| "runtime adapter inventory is missing or unreadable".to_string())?;
    runtime_plan_from_inventory_bytes(&raw, source_path, profile)
}

fn runtime_plan_from_inventory_bytes(
    raw: &[u8],
    source_path: &Path,
    profile: &InventoryProfile,
) -> Result<RuntimeTestPlan, String> {
    let inventory = serde_json::from_slice::<TestInventoryEvidence>(&raw)
        .map_err(|_| "runtime adapter inventory is malformed".to_string())?;
    if inventory.schema != TEST_INVENTORY_SCHEMA
        || inventory.runner.trim().is_empty()
        || !canonical_sha256(&inventory.runner_fingerprint)
        || !canonical_sha256(&inventory.workspace_fingerprint)
        || !canonical_sha256(&inventory.inventory_fingerprint)
        || inventory.tests.is_empty()
        || inventory.tests.iter().any(|test| {
            test.id.trim().is_empty()
                || !matches!(
                    test.expected_outcome.as_deref(),
                    Some("executed" | "skipped")
                )
        })
        || homeboy_engine_primitives::content_hash::sha256_hex(&canonical_inventory_json(
            &inventory,
        )) != inventory.inventory_fingerprint
    {
        return Err(
            "runtime adapter inventory violates the identity/fingerprint contract".to_string(),
        );
    }
    let workspace_root = inventory_workspace_root(source_path, profile)
        .ok_or_else(|| "runtime adapter inventory workspace cannot be resolved".to_string())?;
    if workspace_fingerprint(&workspace_root, profile).as_deref()
        != Some(inventory.workspace_fingerprint.as_str())
        || runner_fingerprint(&workspace_root, &inventory.runner, profile).as_deref()
            != Some(inventory.runner_fingerprint.as_str())
    {
        return Err(
            "runtime adapter inventory provenance does not match the current execution".to_string(),
        );
    }
    let mut tests = inventory
        .tests
        .iter()
        .filter(|test| test.expected_outcome.as_deref() == Some("executed"))
        .map(|test| test.id.trim().to_string())
        .collect::<Vec<_>>();
    tests.sort();
    if tests.is_empty() || tests.windows(2).any(|ids| ids[0] == ids[1]) {
        return Err("runtime adapter inventory has no unique executed test IDs".to_string());
    }
    let execution_fingerprint = execution_fingerprint(
        "adapter_inventory",
        &inventory.inventory_fingerprint,
        &inventory.runner,
        &inventory.runner_fingerprint,
        &inventory.workspace_fingerprint,
        &inventory.inventory_fingerprint,
        &tests,
    );
    Ok(RuntimeTestPlan {
        runner: inventory.runner,
        runner_fingerprint: inventory.runner_fingerprint,
        workspace_fingerprint: inventory.workspace_fingerprint,
        execution_fingerprint,
        tests,
    })
}

fn canonical_sha256(value: &str) -> bool {
    homeboy_engine_primitives::content_hash::is_sha256_hex(value)
        && value == value.to_ascii_lowercase()
}

fn execution_fingerprint(
    source: &str,
    source_id: &str,
    runner: &str,
    runner_fingerprint: &str,
    workspace_fingerprint: &str,
    inventory_fingerprint: &str,
    tests: &[String],
) -> String {
    let canonical = serde_json::json!({
        "inventory_fingerprint": inventory_fingerprint,
        "runner": runner,
        "runner_fingerprint": runner_fingerprint,
        "source": source,
        "source_id": source_id,
        "tests": tests,
        "workspace_fingerprint": workspace_fingerprint,
    });
    homeboy_engine_primitives::content_hash::sha256_hex(
        serde_json::to_vec(&canonical)
            .expect("runtime execution provenance serializes")
            .as_slice(),
    )
}

pub(crate) fn test_timeout() -> Duration {
    homeboy_engine_primitives::test_execution::suite_timeout_from_env().suite_timeout()
}

/// Resolve the optional changed-scope selection cap.
///
/// Mirrors `test_timeout()`: the value is read from the process environment
/// (what a workflow step `env:` block sets and the CLI process inherits),
/// must parse as a positive integer, and unset, unparseable, or zero values
/// disable the guard entirely. Every existing consumer is therefore
/// byte-for-byte unaffected when the cap is not configured (#12365).
fn max_changed_test_files() -> Option<usize> {
    std::env::var(MAX_CHANGED_TEST_FILES_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|cap| *cap > 0)
}

#[derive(Deserialize)]
struct NoTestsApplicableEvidence {
    schema: String,
    extension_id: String,
    step: String,
    nonce: String,
    reason: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TestInventoryEvidence {
    schema: String,
    runner: String,
    runner_fingerprint: String,
    workspace_fingerprint: String,
    tests: Vec<TestInventoryTest>,
    inventory_fingerprint: String,
    #[serde(default)]
    fallback_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TestInventoryTest {
    id: String,
    package: String,
    target: String,
    target_kind: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expected_outcome: Option<String>,
}

fn test_inventory_mode(ci_env: &[(String, String)]) -> bool {
    ci_env
        .iter()
        .any(|(key, value)| key == TEST_INVENTORY_ONLY_ENV && value == "1")
}

/// Resolved inputs for the inventory binding.
///
/// Every one of these was Cargo-derived and unreachable for other toolchains
/// before #12394. `InventoryProfile::cargo()` reproduces that behaviour exactly,
/// and is what an extension without a `test.inventory` block still gets.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InventoryProfile {
    /// Marker files identifying the workspace root, searched upward. Empty
    /// means "ask Cargo", which is the historical behaviour.
    root_markers: Vec<String>,
    fingerprint_names: Vec<String>,
    fingerprint_extensions: Vec<String>,
    fingerprint_skip_dirs: Vec<String>,
    /// Runner id -> argv reporting that runner's version.
    runner_commands: BTreeMap<String, Vec<String>>,
}

impl InventoryProfile {
    fn cargo() -> Self {
        Self {
            root_markers: Vec::new(),
            fingerprint_names: vec!["Cargo.toml".to_string(), "Cargo.lock".to_string()],
            fingerprint_extensions: vec!["rs".to_string()],
            fingerprint_skip_dirs: vec![".git".to_string(), "target".to_string()],
            runner_commands: BTreeMap::from([
                (
                    "cargo".to_string(),
                    vec!["cargo".to_string(), "--version".to_string()],
                ),
                (
                    "nextest".to_string(),
                    vec![
                        "cargo".to_string(),
                        "nextest".to_string(),
                        "--version".to_string(),
                    ],
                ),
            ]),
        }
    }

    /// Build a profile from an extension manifest, falling back to Cargo when
    /// the extension declares nothing.
    ///
    /// A declared config that selects no fingerprint files, or names no usable
    /// runner, is refused rather than silently degraded: an inventory bound to
    /// a constant fingerprint would compare equal across unrelated workspaces.
    fn resolve(config: Option<&crate::extension::TestInventoryConfig>) -> Option<Self> {
        let Some(config) = config else {
            return Some(Self::cargo());
        };
        if !config.selects_files() {
            return None;
        }
        let runner_commands: BTreeMap<String, Vec<String>> = config
            .runners
            .iter()
            .filter(|runner| runner.is_executable())
            .map(|runner| (runner.id.clone(), runner.version_command.clone()))
            .collect();
        if runner_commands.is_empty() {
            return None;
        }
        Some(Self {
            root_markers: config.root_markers.clone(),
            fingerprint_names: config.fingerprint_names.clone(),
            fingerprint_extensions: config.fingerprint_extensions.clone(),
            fingerprint_skip_dirs: config.fingerprint_skip_dirs.clone(),
            runner_commands,
        })
    }

    fn selects(&self, path: &Path) -> bool {
        let name = path.file_name().and_then(|name| name.to_str());
        if name.is_some_and(|name| self.fingerprint_names.iter().any(|want| want == name)) {
            return true;
        }
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                self.fingerprint_extensions
                    .iter()
                    .any(|want| want == extension)
            })
    }

    fn skips_dir(&self, name: &std::ffi::OsStr) -> bool {
        name.to_str()
            .is_some_and(|name| self.fingerprint_skip_dirs.iter().any(|skip| skip == name))
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct TestInventoryBinding {
    child_path: PathBuf,
    workspace_fingerprint: String,
    /// Runner id -> fingerprint, for every runner the profile could identify.
    runner_fingerprints: BTreeMap<String, String>,
    profile: InventoryProfile,
    project_root: std::fs::File,
    run_dir: std::fs::File,
    run_dir_device: u64,
    run_dir_inode: u64,
}

#[cfg(unix)]
fn test_inventory_binding(
    ci_env: &[(String, String)],
    source_path: &Path,
    run_dir: &RunDir,
    profile: &InventoryProfile,
    require_public_path: bool,
) -> Result<TestInventoryBinding, TestInventoryRejection> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let child_path = run_dir.path().join(TEST_INVENTORY_FILE);
    let workspace_root = inventory_workspace_root(source_path, profile)
        .ok_or(TestInventoryRejection::BindingUnavailable)?;
    if require_public_path {
        requested_test_inventory_path(ci_env, &workspace_root)?;
    }
    let project_root = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&workspace_root)
        .map_err(|_| TestInventoryRejection::BindingUnavailable)?;
    let run_dir = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(run_dir.path())
        .map_err(|_| TestInventoryRejection::BindingUnavailable)?;
    let metadata = run_dir
        .metadata()
        .map_err(|_| TestInventoryRejection::BindingUnavailable)?;
    if !metadata.is_dir() {
        return Err(TestInventoryRejection::BindingUnavailable);
    }
    let workspace_fingerprint = workspace_fingerprint(&workspace_root, profile)
        .ok_or(TestInventoryRejection::BindingUnavailable)?;
    Ok(TestInventoryBinding {
        child_path,
        workspace_fingerprint,
        // Inventory producers select one runner. Record each independently so a
        // Cargo inventory remains valid on systems without cargo-nextest.
        runner_fingerprints: profile
            .runner_commands
            .keys()
            .filter_map(|runner| {
                Some((
                    runner.clone(),
                    runner_fingerprint(&workspace_root, runner, profile)?,
                ))
            })
            .collect(),
        profile: profile.clone(),
        run_dir_device: metadata.dev(),
        run_dir_inode: metadata.ino(),
        project_root,
        run_dir,
    })
}

/// Resolve the workspace root an inventory is bound to.
///
/// With no declared markers this asks Cargo, preserving the original
/// behaviour. With markers it walks upward from the component source path for
/// the first ancestor holding one, which needs no toolchain subprocess.
fn inventory_workspace_root(source_path: &Path, profile: &InventoryProfile) -> Option<PathBuf> {
    if profile.root_markers.is_empty() {
        return cargo_workspace_root(source_path);
    }
    let start = source_path.canonicalize().ok()?;
    for ancestor in start.ancestors() {
        if profile
            .root_markers
            .iter()
            .any(|marker| ancestor.join(marker).exists())
        {
            return ancestor.canonicalize().ok();
        }
    }
    Some(start)
}

#[cfg(unix)]
fn requested_test_inventory_path(
    ci_env: &[(String, String)],
    workspace_root: &Path,
) -> Result<(), TestInventoryRejection> {
    let requested = ci_env
        .iter()
        .find_map(|(key, value)| (key == TEST_INVENTORY_FILE_ENV).then_some(value))
        .ok_or(TestInventoryRejection::BindingUnavailable)?;
    let requested = Path::new(requested);
    let requested = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace_root.join(requested)
    };
    // The Action contract permits one output, immediately below the canonical
    // Cargo project root. Lexical equality rejects aliases such as nested paths.
    if requested != workspace_root.join(TEST_INVENTORY_PUBLIC_FILE) {
        return Err(TestInventoryRejection::RequestedPathRejected);
    }
    Ok(())
}

fn cargo_workspace_root(source_path: &Path) -> Option<PathBuf> {
    let output = Command::new("cargo")
        .args(["metadata", "--no-deps", "--format-version=1"])
        .current_dir(source_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let metadata = serde_json::from_slice::<serde_json::Value>(&output.stdout).ok()?;
    let workspace_root = metadata.get("workspace_root")?.as_str()?;
    PathBuf::from(workspace_root).canonicalize().ok()
}

fn runner_fingerprint(
    workspace_root: &Path,
    runner: &str,
    profile: &InventoryProfile,
) -> Option<String> {
    let argv = profile.runner_commands.get(runner)?;
    let (program, args) = argv.split_first()?;
    let output = Command::new(program)
        .args(args)
        .current_dir(workspace_root)
        .output()
        .ok()?;
    output.status.success().then(|| {
        let version = String::from_utf8(output.stdout).ok()?;
        Some(runner_fingerprint_from_version(runner, version.trim()))
    })?
}

fn runner_fingerprint_from_version(runner: &str, version: &str) -> String {
    homeboy_engine_primitives::content_hash::sha256_hex(format!("{runner}\0{version}").as_bytes())
}

/// Concatenate the selected workspace files exactly as the inventory producers
/// must, and hash the result.
///
/// # The ordering is by path *component*, and a producer must say so explicitly
///
/// `Ord for PathBuf` compares component by component, so this function is the
/// arbiter of an ordering that is not the same as sorting the joined path text.
/// The two disagree whenever a directory name is a prefix of a sibling followed
/// by a byte below `/`:
///
/// ```text
/// src/auth/handler.rs        # first here: "auth" < "auth-tokens"
/// src/auth-tokens/token.rs   # first when sorting the joined text: '-' < '/'
/// ```
///
/// Both sides then hash the same files in a different order, the workspace
/// fingerprints never agree, and every inventory that producer writes is
/// rejected as `WorkspaceFingerprintMismatch` — permanently, because no rerun
/// changes a sort order. `Extra-Chill/extrachill-users` has `inc/auth/` beside
/// `inc/auth-tokens/`, and its PRs could not obtain differential test evidence
/// at all until its producer was fixed (#13494).
///
/// A Python producer must therefore sort by `PurePath.parts` explicitly and
/// never by the joined string — and never by relying on `sorted()` over `Path`
/// objects either, whose result *changed in Python 3.12* from string order to
/// component order. An implicit sort makes the fingerprint depend on the
/// interpreter that happened to be on the runner.
fn workspace_fingerprint(root: &Path, profile: &InventoryProfile) -> Option<String> {
    fn collect(
        root: &Path,
        directory: &Path,
        profile: &InventoryProfile,
        files: &mut Vec<PathBuf>,
    ) -> Option<()> {
        for entry in std::fs::read_dir(directory).ok()? {
            let entry = entry.ok()?;
            let path = entry.path();
            let file_type = entry.file_type().ok()?;
            if file_type.is_dir() {
                if !profile.skips_dir(&entry.file_name()) {
                    collect(root, &path, profile, files)?;
                }
            } else if path.is_file() && profile.selects(&path) {
                files.push(path.strip_prefix(root).ok()?.to_path_buf());
            }
        }
        Some(())
    }

    let mut files = Vec::new();
    collect(root, root, profile, &mut files)?;
    files.sort();
    let mut content = String::new();
    for relative in files {
        let path = root.join(&relative);
        content.push_str(relative.to_str()?);
        content.push('\0');
        // Path.read_text() translates CRLF and lone CR to LF before the Python
        // inventory producer concatenates its fingerprint input.
        content.push_str(
            &std::fs::read_to_string(path)
                .ok()?
                .replace("\r\n", "\n")
                .replace('\r', "\n"),
        );
        content.push('\0');
    }
    Some(homeboy_engine_primitives::content_hash::sha256_hex(
        content.as_bytes(),
    ))
}

#[cfg(unix)]
fn revalidate_test_inventory_binding(
    binding: &TestInventoryBinding,
    source_path: &Path,
    runner: &str,
) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(metadata) = binding.run_dir.metadata() else {
        return false;
    };
    // This detects replacement for diagnostics, but the held descriptor remains
    // the authority even when its original name has been renamed by the child.
    if metadata.dev() != binding.run_dir_device || metadata.ino() != binding.run_dir_inode {
        return false;
    }
    let Some(workspace_root) = inventory_workspace_root(source_path, &binding.profile) else {
        return false;
    };
    workspace_fingerprint(&workspace_root, &binding.profile)
        == Some(binding.workspace_fingerprint.clone())
        && runner_fingerprint(&workspace_root, runner, &binding.profile)
            == expected_runner_fingerprint(binding, runner)
}

/// An inventory may only name a runner the binding actually fingerprinted, so
/// an unknown or undeclared runner id is rejected rather than trusted.
#[cfg(unix)]
fn expected_runner_fingerprint(binding: &TestInventoryBinding, runner: &str) -> Option<String> {
    binding.runner_fingerprints.get(runner).cloned()
}

#[cfg(unix)]
fn unlink_test_inventory(binding: &TestInventoryBinding) -> bool {
    use std::os::fd::AsRawFd;
    let result = unsafe {
        libc::unlinkat(
            binding.run_dir.as_raw_fd(),
            c"test-inventory.json".as_ptr(),
            0,
        )
    };
    result == 0 || std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
}

/// The last thing the inventory producer said, for the reason string.
///
/// The producer runs with `passthrough(false)`, so its output goes nowhere:
/// when it fails, the only surviving fact is a nonzero exit code. Every WordPress
/// and Rust inventory producer explains itself on stderr before exiting
/// (`WordPress test inventory error: ...`), so carrying a couple of lines turns
/// an unattributable evidence failure into a named producer defect. Bounded on
/// purpose — this is a diagnosis, not a log.
#[cfg(unix)]
fn producer_failure_detail(output: &crate::extension::runner::RunnerOutput) -> String {
    const DETAIL_LINES: usize = 3;
    const DETAIL_CHARS: usize = 400;

    let source = [output.stderr.as_str(), output.stdout.as_str()]
        .into_iter()
        .find(|stream| !stream.trim().is_empty());
    let Some(source) = source else {
        return String::new();
    };
    let (tail, _) = tail_lines(source.trim_end(), DETAIL_LINES);
    let detail = tail.trim();
    if detail.is_empty() {
        return String::new();
    }
    let truncated = detail
        .char_indices()
        .nth(DETAIL_CHARS)
        .map(|(index, _)| &detail[..index]);
    match truncated {
        Some(prefix) => format!(": {}…", prefix.replace('\n', " ")),
        None => format!(": {}", detail.replace('\n', " ")),
    }
}

#[cfg(unix)]
fn prepare_test_inventory(binding: &TestInventoryBinding) -> Result<(), TestInventoryRejection> {
    unlink_test_inventory(binding)
        .then_some(())
        .ok_or(TestInventoryRejection::PreparationFailed)
}

#[cfg(unix)]
fn valid_test_inventory(
    binding: &TestInventoryBinding,
) -> Result<(TestInventoryOutput, Vec<u8>), TestInventoryRejection> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let file = unsafe {
        let fd = libc::openat(
            binding.run_dir.as_raw_fd(),
            c"test-inventory.json".as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        );
        if fd >= 0 {
            Ok(std::fs::File::from_raw_fd(fd))
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
    let mut file = match file {
        Ok(file) => file,
        Err(error) => {
            let _ = unlink_test_inventory(binding);
            return Err(classify_test_inventory_open_error(&error));
        }
    };
    let Ok(opened_metadata) = file.metadata() else {
        let _ = unlink_test_inventory(binding);
        return Err(TestInventoryRejection::ChildFileUnsafe);
    };
    if !opened_metadata.is_file() || opened_metadata.len() > MAX_TEST_INVENTORY_BYTES {
        let _ = unlink_test_inventory(binding);
        return Err(if opened_metadata.len() > MAX_TEST_INVENTORY_BYTES {
            TestInventoryRejection::ChildFileOversized
        } else {
            TestInventoryRejection::ChildFileUnsafe
        });
    }
    let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
    let read = file.read_to_end(&mut bytes).is_ok() && bytes.len() as u64 == opened_metadata.len();
    let unlinked = unlink_test_inventory(binding);
    if !read || !unlinked {
        return Err(if !read {
            TestInventoryRejection::ChildFileUnreadable
        } else {
            TestInventoryRejection::ChildFileCleanupFailed
        });
    }
    let Ok(inventory) = serde_json::from_slice::<TestInventoryEvidence>(&bytes) else {
        return Err(TestInventoryRejection::InvalidJson);
    };
    valid_test_inventory_payload(&inventory, binding).map(|inventory| (inventory, bytes))
}

#[cfg(unix)]
fn classify_test_inventory_open_error(error: &std::io::Error) -> TestInventoryRejection {
    match error.kind() {
        std::io::ErrorKind::NotFound => TestInventoryRejection::ChildFileMissing,
        std::io::ErrorKind::PermissionDenied => TestInventoryRejection::ChildFileUnreadable,
        _ => TestInventoryRejection::ChildFileUnsafe,
    }
}

#[cfg(unix)]
fn remove_published_test_inventory(binding: &TestInventoryBinding, file: &std::fs::File) {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let Ok(created) = file.metadata() else {
        return;
    };
    let mut entry = unsafe { std::mem::zeroed::<libc::stat>() };
    let matched = unsafe {
        libc::fstatat(
            binding.project_root.as_raw_fd(),
            c"homeboy-test-inventory.json".as_ptr(),
            &mut entry,
            libc::AT_SYMLINK_NOFOLLOW,
        ) == 0
            && (entry.st_mode & libc::S_IFMT) == libc::S_IFREG
            && entry.st_dev as u64 == created.dev()
            && entry.st_ino as u64 == created.ino()
    };
    if matched {
        let _ = unsafe {
            libc::unlinkat(
                binding.project_root.as_raw_fd(),
                c"homeboy-test-inventory.json".as_ptr(),
                0,
            )
        };
    }
}

#[cfg(unix)]
fn publish_test_inventory_with<W, S, M>(
    binding: &TestInventoryBinding,
    write: W,
    sync: S,
    metadata: M,
) -> bool
where
    W: FnOnce(&mut std::fs::File) -> std::io::Result<()>,
    S: FnOnce(&std::fs::File) -> std::io::Result<()>,
    M: FnOnce(&std::fs::File) -> std::io::Result<std::fs::Metadata>,
{
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::MetadataExt;

    let fd = unsafe {
        libc::openat(
            binding.project_root.as_raw_fd(),
            c"homeboy-test-inventory.json".as_ptr(),
            libc::O_WRONLY | libc::O_NOFOLLOW | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return false;
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    if write(&mut file).is_err() || sync(&file).is_err() {
        remove_published_test_inventory(binding, &file);
        return false;
    }
    let Ok(created) = metadata(&file) else {
        remove_published_test_inventory(binding, &file);
        return false;
    };
    let mut published = unsafe { std::mem::zeroed::<libc::stat>() };
    let verified = unsafe {
        libc::fstatat(
            binding.project_root.as_raw_fd(),
            c"homeboy-test-inventory.json".as_ptr(),
            &mut published,
            libc::AT_SYMLINK_NOFOLLOW,
        ) == 0
            && (published.st_mode & libc::S_IFMT) == libc::S_IFREG
            && published.st_dev as u64 == created.dev()
            && published.st_ino as u64 == created.ino()
    };
    if !verified {
        remove_published_test_inventory(binding, &file);
    }
    verified
}

/// The parent publishes exactly the bytes it validated, never a reserialized
/// approximation of child evidence.
#[cfg(unix)]
fn publish_test_inventory(binding: &TestInventoryBinding, bytes: &[u8]) -> bool {
    publish_test_inventory_with(
        binding,
        |file| file.write_all(bytes),
        |file| file.sync_all(),
        |file| file.metadata(),
    )
}

#[cfg(unix)]
fn valid_test_inventory_payload(
    inventory: &TestInventoryEvidence,
    binding: &TestInventoryBinding,
) -> Result<TestInventoryOutput, TestInventoryRejection> {
    // The set of legal runner names belongs to the binding's InventoryProfile,
    // not to this function. `expected_runner_fingerprint` below returns None for
    // any runner the binding did not fingerprint, which rejects an unknown
    // runner and additionally proves the declared fingerprint matches -- a
    // strictly stronger check than a name allowlist. Hardcoding "cargo" |
    // "nextest" here re-pinned the mechanism to Rust after #12396 made the rest
    // of it extension-driven, so a declared runner such as "wordpress" was
    // refused no matter what its profile said. (#12394)
    if inventory.schema != TEST_INVENTORY_SCHEMA {
        return Err(TestInventoryRejection::InvalidSchema);
    }
    if inventory.runner.trim().is_empty()
        || !homeboy_engine_primitives::content_hash::is_sha256_hex(&inventory.runner_fingerprint)
        || !homeboy_engine_primitives::content_hash::is_sha256_hex(&inventory.workspace_fingerprint)
        || !homeboy_engine_primitives::content_hash::is_sha256_hex(&inventory.inventory_fingerprint)
        || inventory.inventory_fingerprint != inventory.inventory_fingerprint.to_ascii_lowercase()
        || inventory.tests.is_empty()
        || inventory
            .tests
            .iter()
            .all(|test| test.expected_outcome.as_deref() == Some("skipped"))
        || inventory
            .fallback_reason
            .as_deref()
            .is_some_and(|reason| reason.trim().is_empty() || reason.len() > 1024)
    {
        return Err(TestInventoryRejection::InvalidPayload);
    }
    if expected_runner_fingerprint(binding, &inventory.runner).as_deref()
        != Some(inventory.runner_fingerprint.as_str())
    {
        return Err(TestInventoryRejection::RunnerFingerprintMismatch);
    }
    if inventory.workspace_fingerprint != binding.workspace_fingerprint {
        return Err(TestInventoryRejection::WorkspaceFingerprintMismatch);
    }
    if inventory.tests.iter().any(|test| {
        [
            &test.id,
            &test.package,
            &test.target,
            &test.target_kind,
            &test.name,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
            || test
                .expected_outcome
                .as_deref()
                .is_none_or(|outcome| !matches!(outcome, "executed" | "skipped"))
    }) {
        return Err(TestInventoryRejection::InvalidTests);
    }
    let mut test_ids = std::collections::HashSet::with_capacity(inventory.tests.len());
    if inventory
        .tests
        .iter()
        .any(|test| !test_ids.insert(&test.id))
    {
        return Err(TestInventoryRejection::InvalidTests);
    }

    // Rebuild the signed shape from parent-bound provenance rather than accepting
    // the child fields as the material that defines the schema fingerprint.
    let mut bound_inventory = inventory.clone();
    bound_inventory.runner_fingerprint = expected_runner_fingerprint(binding, &inventory.runner)
        .ok_or(TestInventoryRejection::RunnerFingerprintMismatch)?;
    bound_inventory.workspace_fingerprint = binding.workspace_fingerprint.clone();
    if homeboy_engine_primitives::content_hash::sha256_hex(&canonical_inventory_json(
        &bound_inventory,
    )) != inventory.inventory_fingerprint
    {
        return Err(TestInventoryRejection::InventoryFingerprintMismatch);
    }
    Ok(TestInventoryOutput {
        schema: inventory.schema.clone(),
        runner: inventory.runner.clone(),
        runner_fingerprint: inventory.runner_fingerprint.clone(),
        workspace_fingerprint: inventory.workspace_fingerprint.clone(),
        test_count: inventory.tests.len(),
        inventory_fingerprint: inventory.inventory_fingerprint.clone(),
        fallback_reason: inventory.fallback_reason.clone(),
    })
}

/// The Rust inventory producer fingerprints `json.dumps(..., sort_keys=True,
/// separators=(",", ":"))`. Its default `ensure_ascii=True` is part of the v1
/// identity, including for non-ASCII test names.
fn canonical_inventory_json(inventory: &TestInventoryEvidence) -> Vec<u8> {
    let mut json = String::from("{\"runner\":");
    append_python_json_string(&mut json, &inventory.runner);
    json.push_str(",\"runner_fingerprint\":");
    append_python_json_string(&mut json, &inventory.runner_fingerprint);
    json.push_str(",\"schema\":");
    append_python_json_string(&mut json, &inventory.schema);
    json.push_str(",\"tests\":[");
    for (index, test) in inventory.tests.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push('{');
        if let Some(expected_outcome) = &test.expected_outcome {
            json.push_str("\"expected_outcome\":");
            append_python_json_string(&mut json, expected_outcome);
            json.push(',');
        }
        json.push_str("\"id\":");
        append_python_json_string(&mut json, &test.id);
        json.push_str(",\"name\":");
        append_python_json_string(&mut json, &test.name);
        json.push_str(",\"package\":");
        append_python_json_string(&mut json, &test.package);
        json.push_str(",\"target\":");
        append_python_json_string(&mut json, &test.target);
        json.push_str(",\"target_kind\":");
        append_python_json_string(&mut json, &test.target_kind);
        json.push('}');
    }
    json.push_str("],\"workspace_fingerprint\":");
    append_python_json_string(&mut json, &inventory.workspace_fingerprint);
    json.push('}');
    json.into_bytes()
}

fn append_python_json_string(json: &mut String, value: &str) {
    json.push('"');
    for character in value.chars() {
        match character {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            '\u{08}' => json.push_str("\\b"),
            '\u{0c}' => json.push_str("\\f"),
            '\n' => json.push_str("\\n"),
            '\r' => json.push_str("\\r"),
            '\t' => json.push_str("\\t"),
            character if character.is_ascii() && !character.is_control() => json.push(character),
            character if character.is_ascii() => {
                use std::fmt::Write;

                write!(json, "\\u{:04x}", character as u32).expect("write to string");
            }
            character => {
                use std::fmt::Write;

                for unit in character.encode_utf16(&mut [0; 2]) {
                    write!(json, "\\u{unit:04x}").expect("write to string");
                }
            }
        }
    }
    json.push('"');
}
/// Classify what the test runner actually measured.
///
/// Executed assertions -- `passed + failed` -- are the unit of evidence.
/// `skipped` is deliberately excluded: an all-skipped result proves only that
/// the runner started. Absent counts are [`Measurement::unreported`], which is
/// a different state from a counted zero and reads differently to an operator.
///
/// No population is supplied. The runner does not independently know how many
/// tests *should* have run (that is what `--filter` and the extension's
/// selection are for), so a zero here is honestly `ZeroUnits`
/// rather than a provably broken instrument.
fn test_measurement(test_counts: Option<&TestCounts>) -> Measurement {
    match test_counts {
        Some(counts) => Measurement::units(counts.passed + counts.failed),
        None => Measurement::unreported(),
    }
}

fn test_run_status(
    runner_success: bool,
    test_counts: Option<&TestCounts>,
    no_tests_applicable: bool,
) -> &'static str {
    if !runner_success {
        return "failed";
    }

    // The one legitimate escape from the measurement requirement, and it is
    // gated on POSITIVE evidence rather than on absence: `no_tests_applicable`
    // is only true when the extension wrote a nonce-matched, schema-matched
    // evidence file naming its reason. "The instrument reported nothing" can
    // never reach here.
    if no_tests_applicable {
        return "skipped";
    }

    // A zero count or an all-skipped result proves only that the runner
    // started. A passing test gate needs evidence that it executed a test.
    //
    // This predicate is now shared (#10685). The behaviour is unchanged in
    // every case -- see `test_run_status_matches_the_shared_predicate` -- but
    // the reasoning is no longer private to this function, and the audit and
    // lint gates answer the same question the same way.
    let intended = if test_counts.is_some_and(|counts| counts.failed == 0) {
        Verdict::Pass
    } else {
        Verdict::Fail
    };
    match test_measurement(test_counts).assess().constrain(intended) {
        Ok(Verdict::Pass) => "passed",
        // `Unknown` collapses to `"failed"` here, and only here. This status is
        // a published string in the command output envelope that downstream
        // consumers match on, so introducing a fourth value is a breaking
        // change rather than an additive one. The *label* is therefore lossy
        // while the *decision* is not: an unmeasured run has never rendered
        // green on this path and still does not. `test_phase_report` in
        // `report.rs` carries the distinction that a reader needs -- "test
        // runner reported zero executed tests" versus a timeout versus real
        // failures -- so nothing an operator acts on is lost.
        Ok(Verdict::Unknown) | Ok(Verdict::Fail) => "failed",
        // Unreachable on this path: `test_measurement` never establishes a
        // population, so `Contradicted` cannot be produced. Fail closed.
        Err(_) => "failed",
    }
}

fn test_run_status_with_inventory(
    runner_success: bool,
    test_counts: Option<&TestCounts>,
    no_tests_applicable: bool,
    inventory_mode: bool,
    test_inventory: Option<&TestInventoryOutput>,
) -> &'static str {
    if !runner_success {
        return "failed";
    }
    if inventory_mode {
        return if test_inventory.is_some() {
            "passed"
        } else {
            "failed"
        };
    }

    test_run_status(runner_success, test_counts, no_tests_applicable)
}

/// Re-finalize a test result when persisted artifacts add missing test counts.
///
/// Only a successful runner may be promoted by delayed evidence. A nonzero
/// runner exit remains the primary failure even if its eventual counts pass.
pub fn finalize_test_result_after_artifact_hydration(workflow: &mut TestRunWorkflowResult) {
    if workflow.test_inventory.is_some()
        || workflow.runner_exit_code != Some(0)
        || workflow.test_counts.is_none()
    {
        return;
    }

    let status = test_run_status(true, workflow.test_counts.as_ref(), false);
    workflow.status = status.to_string();
    workflow.exit_code = if status == "passed" { 0 } else { 1 };
    if workflow
        .baseline_comparison
        .as_ref()
        .is_some_and(|comparison| comparison.regression)
    {
        workflow.exit_code = 1;
    }
}

fn no_tests_applicable(
    policy_enabled: bool,
    evidence_file: &Path,
    extension_id: &str,
    nonce: &str,
    test_counts: Option<&TestCounts>,
) -> bool {
    if !policy_enabled || test_counts.is_some_and(|counts| counts.passed + counts.failed > 0) {
        return false;
    }
    let Ok(raw) = std::fs::read_to_string(evidence_file) else {
        return false;
    };
    let Ok(evidence) = serde_json::from_str::<NoTestsApplicableEvidence>(&raw) else {
        return false;
    };
    evidence.schema == NO_TESTS_APPLICABLE_SCHEMA
        && evidence.extension_id == extension_id
        && evidence.step == NO_TESTS_APPLICABLE_STEP
        && evidence.nonce == nonce
        && !evidence.reason.trim().is_empty()
}

pub fn run_main_test_workflow(
    component: &Component,
    source_path: &Path,
    args: TestRunWorkflowArgs,
    run_dir: &RunDir,
) -> homeboy_core::Result<TestRunWorkflowResult> {
    if !args.restore_checkout {
        return run_main_test_workflow_inner(component, source_path, args, run_dir);
    }

    let component_label = args.component_label.clone();
    let json_summary = args.json_summary;
    run_review_test_lifecycle(source_path, component_label, json_summary, || {
        run_main_test_workflow_inner(component, source_path, args, run_dir)
    })
}

fn run_main_test_workflow_inner(
    component: &Component,
    source_path: &Path,
    args: TestRunWorkflowArgs,
    run_dir: &RunDir,
) -> homeboy_core::Result<TestRunWorkflowResult> {
    let changed_scope = if let Some(ref git_ref) = args.changed_since {
        Some(match args.precomputed_changed_files.as_ref() {
            Some(changed_files) => crate::extension::test::compute_changed_test_scope_for_files(
                component,
                git_ref,
                changed_files,
            )?,
            None => compute_changed_test_scope(component, git_ref)?,
        })
    } else {
        None
    };
    let inventory_mode = test_inventory_mode(&args.ci_env);

    let coverage_enabled = args.coverage || args.coverage_min.is_some();
    let results_file = run_dir.step_file(run_dir::files::TEST_RESULTS);
    let coverage_file = if coverage_enabled {
        Some(run_dir.step_file(run_dir::files::COVERAGE))
    } else {
        None
    };
    let failures_file = run_dir.step_file(run_dir::files::TEST_FAILURES);
    let durations_file = run_dir.step_file(run_dir::files::TEST_DURATIONS);

    // Inventory is a complete producer operation, not a changed-test execution.
    // It must not inherit an empty changed scope that tells the extension there is
    // nothing to enumerate.
    let changed_test_files = if inventory_mode {
        None
    } else {
        changed_scope
            .as_ref()
            .map(|scope| scope.selected_files.as_slice())
    };

    if let Some(ref scope) = changed_scope {
        if scope.selected_files.is_empty() && !inventory_mode {
            let changed_ref = scope.changed_since.as_deref().unwrap_or("unknown");

            // Fail closed when production/test source changed but the scope
            // selected zero tests: passing green there is not release evidence,
            // it just means the change-to-test mapping missed the impacted
            // files. Documentation/config-only changes leave
            // `source_changes_without_tests` empty and still pass. (#8340)
            //
            // Restated through the shared predicate (#10685) without changing
            // behaviour: zero tests selected is the observation, and the
            // impacted source files are the independently-known population that
            // says whether that zero is honest. A non-empty population makes
            // this `Contradicted` -- a provably broken instrument, and the one
            // outcome that is a hard error rather than an `unknown`. #8340
            // reached that conclusion on its own, three months before #10685
            // named it; the two agree exactly, which is the main reason this
            // predicate is worth sharing.
            let scope_measurement = Measurement::units(scope.selected_files.len() as u64)
                .against_population(scope.source_changes_without_tests.len() as u64);
            if scope_measurement.assess().is_broken_instrument() {
                let impacted = &scope.source_changes_without_tests;
                let preview = impacted
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                let more = impacted.len().saturating_sub(10);
                let impacted_summary = if more > 0 {
                    format!("{preview}, and {more} more")
                } else {
                    preview
                };

                let message = format!(
                    "Changed-scope test gate selected zero tests, but {} source file(s) changed since {changed_ref}: {impacted_summary}. Zero selection is not valid test evidence for a source change.",
                    impacted.len(),
                );
                let findings = Some(vec![HomeboyFinding::builder("test", message.clone())
                    .rule("changed_scope_zero_tests_for_source_change")
                    .category("test-scope")
                    .severity("error")
                    .build()]);
                let hints = Some(vec![
                    format!(
                        "Add or route a test for the changed source, or run the full suite: homeboy review test {}",
                        args.component_id
                    ),
                    "If these changes are intentionally test-exempt, exclude them from the release/test scope so the gate can pass with a typed reason.".to_string(),
                ]);

                return Ok(TestRunWorkflowResult {
                    status: "failed".to_string(),
                    component: args.component_label,
                    exit_code: 1,
                    runner_exit_code: None,
                    test_counts: None,
                    test_inventory: None,
                    test_inventory_rejection: None,
                    test_runtime_evidence: Some(TestRuntimeEvidence::InvalidEvidence {
                        reason: "test execution stopped before an exact runtime plan was available"
                            .to_string(),
                    }),
                    test_durations: None,
                    findings,
                    failure_analysis_input: None,
                    coverage: None,
                    baseline_comparison: None,
                    analysis: None,
                    autofix: None,
                    hints,
                    test_scope: Some(scope.clone()),
                    summary: if args.json_summary {
                        Some(build_test_summary(None, None, 0))
                    } else {
                        None
                    },
                    raw_output: None,
                    extension_phase_timings: Vec::new(),
                    cargo_target: None,
                });
            }

            // No source-relevant change: a genuine no-test scope
            // (documentation/config only) may pass.
            let hints = Some(vec![
                format!(
                    "No impacted tests found for --changed-since {changed_ref} (no production or test source changed)"
                ),
                format!(
                    "Run full suite if needed: homeboy review test {}",
                    args.component_id
                ),
            ]);

            return Ok(TestRunWorkflowResult {
                status: "passed".to_string(),
                component: args.component_label,
                exit_code: 0,
                runner_exit_code: None,
                test_counts: None,
                test_inventory: None,
                test_inventory_rejection: None,
                test_runtime_evidence: Some(TestRuntimeEvidence::InvalidEvidence {
                    reason: "no Test phase executed for the selected change scope".to_string(),
                }),
                test_durations: None,
                findings: None,
                failure_analysis_input: None,
                coverage: None,
                baseline_comparison: None,
                analysis: None,
                autofix: None,
                hints,
                test_scope: Some(scope.clone()),
                summary: if args.json_summary {
                    Some(build_test_summary(None, None, 0))
                } else {
                    None
                },
                raw_output: None,
                extension_phase_timings: Vec::new(),
                cargo_target: None,
            });
        }
    }

    // A bounded, opt-in cap on the changed-scope selection, enforced before the
    // extension is invoked. Drift detection can map a small raw diff onto a
    // very large test-file selection (Data Machine PR #3173: 3 changed files,
    // 161 selected test files), and handing all of it to the extension as one
    // invocation blows the HOMEBOY_TEST_TIMEOUT_SECONDS budget before a single
    // test count is reported. When the cap is exceeded the run dies in seconds
    // with a named finding instead of consuming the whole budget (#12365). The
    // full selection is still carried on `test_scope` so `runs.json` and
    // `metadata_json.test_scope.selected_files` show exactly what was selected.
    if let Some(ref scope) = changed_scope {
        if let Some(cap) = max_changed_test_files() {
            let selected = scope.selected_files.len();
            if selected > cap && !inventory_mode {
                let preview = scope
                    .selected_files
                    .iter()
                    .take(10)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                let more = selected.saturating_sub(10);
                let selected_summary = if more > 0 {
                    format!("{preview}, and {more} more")
                } else {
                    preview
                };

                let message = format!(
                    "Changed-scope test gate selected {selected} test file(s), exceeding the HOMEBOY_MAX_CHANGED_TEST_FILES cap of {cap}: {selected_summary}. A single extension invocation of this size would exceed the test-phase budget before reporting any test counts.",
                );
                let findings = Some(vec![HomeboyFinding::builder("test", message.clone())
                    .rule("changed_scope_selection_exceeds_cap")
                    .category("test-scope")
                    .severity("error")
                    .build()]);
                let hints = Some(vec![
                    format!(
                        "Narrow the drift/changed-test mapping for this component so the change selects fewer test files, or run the full suite explicitly: homeboy review test {}",
                        args.component_id
                    ),
                    format!(
                        "Raise the cap if the selection is legitimately large: set HOMEBOY_MAX_CHANGED_TEST_FILES to at least {selected} (unset or <= 0 disables the guard)"
                    ),
                    "Split the selection across shards, or raise HOMEBOY_TEST_TIMEOUT_SECONDS so the phase fits in its budget.".to_string(),
                ]);

                return Ok(TestRunWorkflowResult {
                    status: "failed".to_string(),
                    component: args.component_label,
                    exit_code: 1,
                    runner_exit_code: None,
                    test_counts: None,
                    test_inventory: None,
                    test_inventory_rejection: None,
                    test_runtime_evidence: Some(TestRuntimeEvidence::InvalidEvidence {
                        reason: "test execution stopped before an exact runtime plan was available"
                            .to_string(),
                    }),
                    test_durations: None,
                    findings,
                    failure_analysis_input: None,
                    coverage: None,
                    baseline_comparison: None,
                    analysis: None,
                    autofix: None,
                    hints,
                    test_scope: Some(scope.clone()),
                    summary: if args.json_summary {
                        Some(build_test_summary(None, None, 0))
                    } else {
                        None
                    },
                    raw_output: None,
                    extension_phase_timings: Vec::new(),
                    cargo_target: None,
                });
            }
        }
    }

    let test_context = crate::extension::test::resolve_test_command(component).ok();
    let test_config = test_context
        .as_ref()
        .and_then(|context| crate::extension_store::load_extension(&context.extension_id).ok())
        .and_then(|extension| extension.test);
    let result_parse = test_config
        .as_ref()
        .and_then(|test| test.result_parse.as_ref());
    let no_tests_policy_enabled = test_config
        .as_ref()
        .and_then(|test| test.no_tests_applicable.as_ref())
        .is_some();
    let no_tests_evidence_file = run_dir.step_file(run_dir::files::NO_TESTS_APPLICABLE);
    let no_tests_nonce = uuid::Uuid::new_v4().to_string();
    let write_results_helper = write_test_results_helper(run_dir)?;
    let test_plan = homeboy_engine_primitives::test_execution::suite_timeout_from_env();
    let (suite_timeout_env, suite_timeout_value) = test_plan.suite_timeout_env();
    let inventory_profile = test_context.as_ref().and_then(|_| {
        InventoryProfile::resolve(
            test_config
                .as_ref()
                .and_then(|test| test.inventory.as_ref()),
        )
    });

    #[cfg(unix)]
    let inventory_binding = if inventory_mode {
        match inventory_profile.as_ref() {
            Some(profile) => {
                test_inventory_binding(&args.ci_env, source_path, run_dir, profile, true)
                    .and_then(|binding| prepare_test_inventory(&binding).map(|()| binding))
                    .map(Some)
            }
            None => Err(TestInventoryRejection::BindingUnavailable),
        }
    } else {
        Ok(None)
    };

    #[cfg(unix)]
    let internal_runtime_plan = if !inventory_mode
        && runtime_evidence_path(&args.ci_env, TEST_SHARD_MANIFEST_ENV).is_none()
        && runtime_evidence_path(&args.ci_env, TEST_INVENTORY_FILE_ENV).is_none()
    {
        Some(match inventory_profile.as_ref() {
            None => Err("test adapter inventory cannot be bound to this extension".to_string()),
            Some(profile) => {
                match test_inventory_binding(&args.ci_env, source_path, run_dir, profile, false)
                    .and_then(|binding| prepare_test_inventory(&binding).map(|()| binding))
                {
                    // The typed rejection names which check failed. Collapsing
                    // it into one static sentence is how #13494 stayed
                    // undiagnosable across two full CI runs: the artifact said
                    // the evidence was invalid and nothing anywhere said why.
                    Err(rejection) => Err(format!(
                        "internal runtime inventory binding could not be established: {}",
                        rejection.message()
                    )),
                    Ok(binding) => {
                        let producer = build_test_runner(
                            component,
                            args.path_override.clone(),
                            &args.settings,
                            &args.settings_json,
                            args.skip_lint,
                            false,
                            None,
                            None,
                            run_dir,
                        )?;
                        let producer = args
                            .ci_env
                            .iter()
                            .fold(producer, |producer, (key, value)| producer.env(key, value));
                        let output = producer
                            .env(suite_timeout_env, &suite_timeout_value)
                            .env(TEST_INVENTORY_ONLY_ENV, "1")
                            .env(
                                TEST_INVENTORY_FILE_ENV,
                                binding.child_path.to_string_lossy().as_ref(),
                            )
                            .env_remove_if(true, "SCOPE_MODE")
                            .env_remove_if(true, "HOMEBOY_CHANGED_SINCE")
                            .env_remove_if(true, "HOMEBOY_CHANGED_TEST_FILES")
                            .passthrough(false)
                            .timeout(Some(test_timeout()))
                            .run()?;
                        if !output.success {
                            let _ = unlink_test_inventory(&binding);
                            Err(format!(
                                "internal runtime inventory producer failed (exit {}){}",
                                output.exit_code,
                                producer_failure_detail(&output)
                            ))
                        } else {
                            match valid_test_inventory(&binding) {
                                Err(rejection) => Err(format!(
                                    "internal runtime inventory producer emitted invalid evidence: {}",
                                    rejection.message()
                                )),
                                Ok((inventory, bytes)) => {
                                    if !revalidate_test_inventory_binding(
                                        &binding,
                                        source_path,
                                        &inventory.runner,
                                    ) {
                                        Err("internal runtime inventory provenance changed before execution".to_string())
                                    } else {
                                        runtime_plan_from_inventory_bytes(
                                            &bytes,
                                            source_path,
                                            profile,
                                        )
                                    }
                                }
                            }
                        }
                    }
                }
            }
        })
    } else {
        None
    };
    #[cfg(not(unix))]
    let internal_runtime_plan: Option<Result<RuntimeTestPlan, String>> = None;

    let runner = build_test_runner(
        component,
        args.path_override.clone(),
        &args.settings,
        &args.settings_json,
        args.skip_lint,
        coverage_enabled,
        args.coverage_min,
        changed_test_files,
        run_dir,
    )?;
    let runner = args
        .ci_env
        .iter()
        .fold(runner, |runner, (key, value)| runner.env(key, value));
    let runner = runner
        // Normalize inherited configuration before a Cargo adapter reaches the
        // global runner: an explicit zero falls back to the declared default,
        // never to the runner's debug-only unbounded mode.
        .env(suite_timeout_env, &suite_timeout_value)
        .env(
            "HOMEBOY_TEST_RESULTS_FILE",
            results_file.to_string_lossy().as_ref(),
        )
        .env(
            crate::extension::runtime_helper::WRITE_TEST_RESULTS_ENV,
            write_results_helper.to_string_lossy().as_ref(),
        )
        .env_if(
            no_tests_policy_enabled,
            NO_TESTS_APPLICABLE_FILE_ENV,
            no_tests_evidence_file.to_string_lossy().as_ref(),
        )
        .env_if(
            no_tests_policy_enabled,
            NO_TESTS_APPLICABLE_NONCE_ENV,
            &no_tests_nonce,
        )
        .env_if(
            no_tests_policy_enabled,
            NO_TESTS_APPLICABLE_EXTENSION_ENV,
            test_context
                .as_ref()
                .map(|context| context.extension_id.as_str())
                .unwrap_or_default(),
        );
    // The child receives a fixed descriptor-bound output path; CI input cannot
    // select it. Non-Unix inventory mode deliberately receives no producer path.
    #[cfg(unix)]
    let runner = runner.env_if(
        inventory_mode,
        TEST_INVENTORY_FILE_ENV,
        inventory_binding
            .as_ref()
            .ok()
            .and_then(Option::as_ref)
            .map(|binding| binding.child_path.to_string_lossy())
            .as_deref()
            .unwrap_or_default(),
    );
    // In summary mode, capture the child's stdout/stderr into run evidence
    // instead of tee-ing the full compiler/test stream to the terminal. The
    // output is still persisted to artifacts below and a bounded failure tail
    // is surfaced by the summary, so `--summary` stays actionable on large
    // repositories instead of overflowing the caller's display limit (#9845).
    let runner = runner.passthrough(!args.json_summary);
    let passthrough_args = normalize_test_passthrough_args(component, &args.passthrough_args)?;
    let mut progress = ValidationProgressRecorder::new(
        run_dir,
        None,
        vec![("test runner".to_string(), args.component_label.clone())],
    )?;
    progress.start(0)?;
    let timeout = test_plan.suite_timeout();
    homeboy_core::log_status!(
        "test",
        "phase=child command=test runner timeout={}s; streaming bounded child supervision",
        timeout.as_secs()
    );
    // Homeboy's own clock. Unlike anything parsed out of runner output it is
    // always available — including when the child is killed before it prints a
    // single summary line — so the suite-level duration survives a timeout.
    let child_started = std::time::Instant::now();
    let output = runner
        .env_remove_if(inventory_mode, "SCOPE_MODE")
        .env_remove_if(inventory_mode, "HOMEBOY_CHANGED_SINCE")
        .env_remove_if(inventory_mode, "HOMEBOY_CHANGED_TEST_FILES")
        .env_if(
            !inventory_mode && args.changed_since.is_some(),
            "SCOPE_MODE",
            "changed",
        )
        .env_if(
            !inventory_mode && args.changed_since.is_some(),
            "HOMEBOY_CHANGED_SINCE",
            args.changed_since.as_deref().unwrap_or_default(),
        )
        .env_if(
            !inventory_mode && args.changed_since.is_some(),
            "HOMEBOY_STRICT_VALIDATION_DEPENDENCIES",
            "1",
        )
        .script_args(&passthrough_args)
        .timeout(Some(timeout))
        .run()?;
    let child_elapsed = child_started.elapsed().as_secs_f64();
    let stdout_artifact = write_command_artifact(run_dir, 0, "stdout", &output.stdout)?;
    let stderr_artifact = write_command_artifact(run_dir, 0, "stderr", &output.stderr)?;
    progress.finish(0, output.exit_code, stdout_artifact, stderr_artifact)?;

    if let (Some(context), Some(spec)) = (test_context.as_ref(), result_parse.as_ref()) {
        run_declared_result_parser(component, context, spec, &output.stdout, run_dir)?;
    }

    let test_counts =
        parse_test_results_file_with_spec(&results_file, result_parse)?.or_else(|| {
            result_parse
                .as_ref()
                .and_then(|spec| parse_test_results_text_with_spec(&output.stdout, spec))
                .or_else(|| parse_test_results_text(&output.stdout))
        });
    // Duration capture. Advisory throughout: it is derived from evidence that
    // already exists, it is attached to its own field, and nothing below reads
    // it when deciding status, exit code, or baseline comparison. A slow test
    // is a finding, not a failure. (#10655)
    let test_durations = collect_test_durations(
        &durations_file,
        &output.stdout,
        child_elapsed,
        output.timed_out,
        timeout,
    );

    let no_tests_applicable = no_tests_applicable(
        no_tests_policy_enabled,
        &no_tests_evidence_file,
        test_context
            .as_ref()
            .map(|context| context.extension_id.as_str())
            .unwrap_or_default(),
        &no_tests_nonce,
        test_counts.as_ref(),
    );

    // Autofix is owned by `refactor --from test --write`; the test command is read-only.
    let test_autofix: Option<AppliedRefactor> = None;

    #[cfg(unix)]
    let (test_inventory, test_inventory_rejection) = if inventory_mode {
        match inventory_binding.as_ref() {
            Err(rejection) => (None, Some(*rejection)),
            Ok(None) => (None, Some(TestInventoryRejection::BindingUnavailable)),
            Ok(Some(binding)) => match valid_test_inventory(binding) {
                Err(rejection) => (None, Some(rejection)),
                Ok((inventory, bytes)) => {
                    if !revalidate_test_inventory_binding(binding, source_path, &inventory.runner) {
                        (None, Some(TestInventoryRejection::RevalidationFailed))
                    } else if !publish_test_inventory(binding, &bytes) {
                        (None, Some(TestInventoryRejection::PublicationFailed))
                    } else {
                        (Some(inventory), None)
                    }
                }
            },
        }
    } else {
        (None, None)
    };
    // Descriptor-bound inventory evidence is Unix-only. Other platforms retain
    // normal test execution, but inventory-only mode cannot manufacture a pass.
    #[cfg(not(unix))]
    let test_inventory: Option<TestInventoryOutput> = None;
    #[cfg(not(unix))]
    let test_inventory_rejection =
        inventory_mode.then_some(TestInventoryRejection::BindingUnavailable);
    let status = test_run_status_with_inventory(
        output.success,
        test_counts.as_ref(),
        no_tests_applicable,
        inventory_mode,
        test_inventory.as_ref(),
    );

    let coverage = coverage_file
        .as_deref()
        .map(parse_coverage_file)
        .transpose()?
        .flatten();

    // The failure sidecar is optional enrichment: it feeds `--analyze` findings
    // and failure classification, but the primary execution result (success,
    // counts, phase, raw output) is already resolved above. A malformed sidecar
    // must not replace that primary result with a JSON parse error and mask the
    // real underlying failure (e.g. a pre-test runtime/bind failure whose
    // structured evidence the runner already reported). Degrade to no
    // enrichment and attach the parse problem as a secondary diagnostic. (#8489)
    let (mut failure_analysis_input, sidecar_diagnostic) =
        parse_optional_failure_sidecar(&failures_file);
    if failure_analysis_input.is_none() && !output.success {
        failure_analysis_input = parse_compiler_failures(&output.stdout, &output.stderr);
    }
    let findings = failure_analysis_input
        .as_ref()
        .and_then(homeboy_findings_from_test_analysis_input);
    let test_runtime_evidence = (!inventory_mode).then(|| {
        runtime_test_evidence(
            &args.ci_env,
            source_path,
            inventory_profile.as_ref(),
            output.success,
            test_counts.as_ref(),
            failure_analysis_input.as_ref(),
            internal_runtime_plan,
        )
    });

    let analysis = if args.analyze {
        let analysis_input = failure_analysis_input
            .clone()
            .unwrap_or_else(|| TestAnalysisInput {
                failures: Vec::new(),
                total: test_counts.as_ref().map(|counts| counts.total).unwrap_or(0),
                passed: test_counts
                    .as_ref()
                    .map(|counts| counts.passed)
                    .unwrap_or(0),
            });

        Some(analyze(&args.component_id, &analysis_input))
    } else {
        None
    };

    if args.baseline_flags.baseline && !no_tests_applicable {
        if let Some(ref counts) = test_counts {
            let _ = baseline::save_baseline(source_path, &args.component_id, counts)?;
        }
    }

    let mut baseline_comparison = None;
    let mut baseline_exit_override = None;

    if !args.baseline_flags.baseline && !args.baseline_flags.ignore_baseline && !no_tests_applicable
    {
        if let Some(ref counts) = test_counts {
            let resolved_baseline = baseline::load_baseline(source_path).or_else(|| {
                args.changed_since.as_ref().and_then(|git_ref| {
                    baseline::load_baseline_from_ref(&source_path.to_string_lossy(), git_ref)
                })
            });

            if let Some(existing_baseline) = resolved_baseline {
                let comparison = baseline::compare(counts, &existing_baseline);

                if comparison.regression {
                    baseline_exit_override = Some(1);
                } else if (comparison.passed_delta > 0 || comparison.failed_delta < 0)
                    && args.baseline_flags.ratchet
                {
                    let _ = baseline::save_baseline(source_path, &args.component_id, counts);
                }

                baseline_comparison = Some(comparison);
            }
        }
    }

    let mut hints = Vec::new();

    // Surface an ignored malformed failure sidecar as a secondary diagnostic so
    // the degraded classification is visible without masking the primary result.
    if let Some(diagnostic) = sidecar_diagnostic {
        hints.push(diagnostic);
    }

    if status == "failed" && args.passthrough_args.is_empty() {
        hints.push(format!(
            "To run specific tests: homeboy review test {} -- --filter=TestName",
            args.component_id
        ));
    }
    if let Some(rejection) = test_inventory_rejection {
        hints.push(format!("Test inventory rejected: {}.", rejection.message()));
    }

    if status == "failed" && output.success && test_counts.is_none() {
        hints.push(
            "The test runner succeeded without verifiable test results. Configure its extension result parser or emit a test-results sidecar."
                .to_string(),
        );
    } else if status == "failed"
        && output.success
        && test_counts
            .as_ref()
            .is_some_and(|counts| counts.passed + counts.failed == 0)
    {
        hints.push(
            "The test runner reported no executed tests. Fix the selected test filter or declare an extension no_tests_applicable policy with evidence."
                .to_string(),
        );
    }

    if !args.skip_lint {
        hints.push(format!(
            "Auto-fix lint issues: homeboy refactor {} --from lint --write",
            args.component_id
        ));
    }

    if !coverage_enabled {
        hints.push(format!(
            "Collect coverage: homeboy review test {} --coverage",
            args.component_id
        ));
    }

    if test_counts.is_some()
        && !no_tests_applicable
        && !args.baseline_flags.baseline
        && baseline_comparison.is_none()
    {
        hints.push(format!(
            "Save test baseline: homeboy review test {} --baseline",
            args.component_id
        ));
    }

    if baseline_comparison.is_some() && !args.baseline_flags.ratchet {
        hints.push(format!(
            "Auto-update baseline on improvement: homeboy review test {} --ratchet",
            args.component_id
        ));
    }

    if status == "failed" && !args.analyze {
        hints.push(format!(
            "Analyze failures: homeboy review test {} --analyze",
            args.component_id
        ));
    }

    if args.passthrough_args.is_empty() {
        hints.push(
            "Pass args to test runner: homeboy review test <component> -- [args]".to_string(),
        );
    }

    hints.push("Full options: homeboy self docs commands/test".to_string());

    let hints = if hints.is_empty() { None } else { Some(hints) };
    let test_exit_code = match status {
        "passed" | "skipped" => 0,
        "failed" if output.exit_code == 0 => 1,
        _ => output.exit_code,
    };
    let exit_code = baseline_exit_override.unwrap_or(test_exit_code);
    let summary = if args.json_summary {
        Some(build_test_summary(
            test_counts.as_ref(),
            analysis.as_ref(),
            exit_code,
        ))
    } else {
        None
    };

    // When the run failed, surface a tail of the runner's stdout/stderr so the
    // user can see the actual runner output — including
    // bootstrap errors like database connection failures that produce zero
    // parsed test results. Without this, `status: failed, exit_code: 1, 0
    // tests ran` leaves the user guessing. (#1143)
    let raw_output = if status == "failed" {
        let (stdout_tail, stdout_truncated) = tail_lines(&output.stdout, RAW_OUTPUT_TAIL_LINES);
        let (stderr_tail, stderr_truncated) = tail_lines(&output.stderr, RAW_OUTPUT_TAIL_LINES);
        if stdout_tail.is_empty() && stderr_tail.is_empty() {
            None
        } else {
            Some(RawTestOutput {
                stdout_tail: homeboy_core::redaction::redact_string(&stdout_tail),
                stderr_tail: homeboy_core::redaction::redact_string(&stderr_tail),
                truncated: stdout_truncated || stderr_truncated,
                stdout_truncated,
                stderr_truncated,
                stdout_seen_bytes: output.stdout.len(),
                stdout_retained_bytes: output.stdout.len(),
                stderr_seen_bytes: output.stderr.len(),
                stderr_retained_bytes: output.stderr.len(),
                stdout_limit_bytes: 0,
                stderr_limit_bytes: 0,
            })
        }
    } else {
        None
    };
    let mut extension_phase_timings = output.extension_phase_timings;
    merge_reported_test_artifact_locators(
        &mut extension_phase_timings,
        &output.stdout,
        &output.stderr,
    );

    // When tests failed with no parseable counts, surface a dedicated hint so
    // the user understands `raw_output` is the only signal about what went
    // wrong. A missing sidecar does not prove that no tests executed.
    let mut hints_vec = hints.unwrap_or_default();
    if status == "failed" && test_counts.is_none() && raw_output.is_some() {
        hints_vec.insert(
            0,
            "The test runner failed before producing structured results. \
             See raw_output.stderr_tail / raw_output.stdout_tail for the underlying error \
             (bootstrap failure, missing deps, DB connection, etc.)."
                .to_string(),
        );
    }
    let hints = if hints_vec.is_empty() {
        None
    } else {
        Some(hints_vec)
    };

    Ok(TestRunWorkflowResult {
        status: status.to_string(),
        component: args.component_label,
        exit_code,
        runner_exit_code: Some(output.exit_code),
        test_counts,
        test_inventory,
        test_inventory_rejection,
        test_runtime_evidence,
        test_durations,
        findings,
        failure_analysis_input,
        coverage,
        baseline_comparison,
        analysis,
        autofix: test_autofix,
        hints,
        test_scope: changed_scope,
        summary,
        raw_output,
        cargo_target: output.cargo_target,
        extension_phase_timings,
    })
}

/// Assemble the duration picture for one test child.
///
/// Order of preference: an extension-written `test.durations` sidecar (richer
/// timings than stdout can carry), then the runner's own output. Homeboy's
/// wall-clock measurement of the child is attached either way, because it is
/// the only duration that survives a kill.
///
/// A terminated child never finishes writing its evidence, so its timings are
/// necessarily partial. They are still returned — the run that blows the
/// budget is precisely the one where knowing what consumed it matters — but
/// they are marked `complete: false` and carry an explicit reason, so a
/// partial picture can never be read as a full one. Nothing here can fail the
/// run: an unreadable sidecar or unparseable output yields no durations, not
/// an error.
fn collect_test_durations(
    durations_file: &Path,
    stdout: &str,
    child_elapsed: f64,
    timed_out: bool,
    budget: Duration,
) -> Option<TestDurations> {
    let incomplete_reason = timed_out.then(|| {
        format!(
            "test child terminated at its {}s budget; timings cover only what completed first",
            budget.as_secs()
        )
    });

    if let Ok(Some(mut declared)) = parse_test_durations_file(durations_file) {
        if declared.phase_seconds.is_none() {
            declared.phase_seconds = Some(child_elapsed);
        }
        if declared.budget_seconds.is_none() {
            declared.budget_seconds = Some(budget.as_secs_f64());
        }
        if let Some(reason) = incomplete_reason {
            declared.complete = false;
            declared.incomplete_reason = Some(reason);
        }
        return Some(declared);
    }

    let durations = build_test_durations(
        parse_duration_samples(stdout),
        Some(child_elapsed),
        SlowTestPolicy::for_budget(Some(budget)),
        incomplete_reason,
    );
    (!durations.is_empty()).then_some(durations)
}

fn parse_compiler_failures(stdout: &str, stderr: &str) -> Option<TestAnalysisInput> {
    let diagnostic = Regex::new(r"^error\[(E\d+)\]: (.+)$").expect("compiler regex is valid");
    let location = Regex::new(r"^\s*--> (.+):(\d+):\d+$").expect("location regex is valid");
    let symbol = Regex::new(r"`([^`]+)`").expect("symbol regex is valid");
    let lines = stdout.lines().chain(stderr.lines()).collect::<Vec<_>>();
    let mut failures = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        let Some(captures) = diagnostic.captures(line) else {
            continue;
        };
        let Some((source_file, source_line)) = lines[index + 1..].iter().take(6).find_map(|line| {
            let captures = location.captures(line)?;
            Some((captures[1].to_string(), captures[2].parse::<u32>().ok()?))
        }) else {
            continue;
        };
        let code = captures[1].to_string();
        let message = captures[2].to_string();
        let symbol = symbol
            .captures(&message)
            .map(|captures| captures[1].to_string())
            .unwrap_or_else(|| message.clone());
        failures.push(crate::extension::test::TestFailure {
            test_name: format!("{code}: {symbol}"),
            test_file: String::new(),
            error_type: format!("compiler_error:{code}"),
            message,
            source_file,
            source_line,
        });
        if failures.len() == COMPILER_FAILURE_LIMIT {
            break;
        }
    }

    (!failures.is_empty()).then_some(TestAnalysisInput {
        failures,
        total: 0,
        passed: 0,
    })
}

/// Parse the optional failure sidecar, degrading gracefully on malformed data.
///
/// Returns the parsed enrichment input (or `None` when absent/unparseable) and
/// an optional secondary diagnostic describing an ignored malformed sidecar. A
/// malformed sidecar never propagates as an error: the primary execution result
/// is already resolved and must not be replaced by a sidecar parse failure that
/// masks the real underlying failure. (#8489)
fn parse_optional_failure_sidecar(
    failures_file: &Path,
) -> (Option<TestAnalysisInput>, Option<String>) {
    match parse_failures_file(failures_file) {
        Ok(input) => (input, None),
        Err(error) => {
            let diagnostic = format!(
                "Ignored a malformed test-failures sidecar ({}); the primary run result is preserved. Re-run with --analyze after the extension emits a valid sidecar for failure classification.",
                error.message
            );
            (None, Some(diagnostic))
        }
    }
}

struct TestCheckoutGuard {
    path: std::path::PathBuf,
    head: String,
}

fn run_review_test_lifecycle(
    source_path: &Path,
    component: String,
    json_summary: bool,
    run: impl FnOnce() -> homeboy_core::Result<TestRunWorkflowResult>,
) -> homeboy_core::Result<TestRunWorkflowResult> {
    let guard = TestCheckoutGuard::capture(source_path)?;
    let result =
        run().unwrap_or_else(|error| failed_test_workflow(component, json_summary, &error));
    guard.restore()?;
    Ok(result)
}

impl TestCheckoutGuard {
    fn capture(path: &Path) -> homeboy_core::Result<Self> {
        let changes = homeboy_core::git::get_uncommitted_changes(&path.to_string_lossy())?;
        if changes.has_changes {
            let files = changes
                .staged
                .iter()
                .chain(changes.unstaged.iter())
                .chain(changes.untracked.iter())
                .take(10)
                .cloned()
                .collect::<Vec<_>>();
            return Err(Error::validation_invalid_argument(
                "working_tree",
                "Review tests require a clean component checkout",
                None,
                Some(vec![format!("Dirty files: {}", files.join(", "))]),
            ));
        }

        let head =
            homeboy_core::git::run_git(path, &["rev-parse", "HEAD"], "capture review test HEAD")?;
        Ok(Self {
            path: path.to_path_buf(),
            head: head.trim().to_string(),
        })
    }

    fn restore(&self) -> homeboy_core::Result<()> {
        homeboy_core::git::run_git(
            &self.path,
            &["reset", "--hard", &self.head],
            "restore review test checkout",
        )?;
        homeboy_core::git::run_git(
            &self.path,
            &["clean", "-fd"],
            "remove review test artifacts",
        )?;

        let changes = homeboy_core::git::get_uncommitted_changes(&self.path.to_string_lossy())?;
        if changes.has_changes {
            return Err(Error::internal_unexpected(
                "review test checkout remained dirty after restoration",
            ));
        }
        Ok(())
    }
}

fn failed_test_workflow(
    component: String,
    json_summary: bool,
    error: &Error,
) -> TestRunWorkflowResult {
    let message = error.to_string();
    TestRunWorkflowResult {
        status: "failed".to_string(),
        component,
        exit_code: 2,
        runner_exit_code: None,
        test_counts: None,
        test_inventory: None,
        test_inventory_rejection: None,
        test_runtime_evidence: Some(TestRuntimeEvidence::InvalidEvidence {
            reason: "test execution failed before runtime evidence could be produced".to_string(),
        }),
        test_durations: None,
        findings: None,
        failure_analysis_input: None,
        coverage: None,
        baseline_comparison: None,
        analysis: None,
        autofix: None,
        hints: Some(vec![
            "The test runner failed during setup or execution; inspect raw_output.stderr_tail"
                .to_string(),
        ]),
        test_scope: None,
        summary: json_summary.then(|| build_test_summary(None, None, 2)),
        raw_output: Some(RawTestOutput {
            stdout_tail: String::new(),
            stderr_tail: homeboy_core::redaction::redact_string(&message),
            truncated: false,
            stdout_truncated: false,
            stderr_truncated: false,
            stdout_seen_bytes: 0,
            stdout_retained_bytes: 0,
            stderr_seen_bytes: message.len(),
            stderr_retained_bytes: message.len(),
            stdout_limit_bytes: 0,
            stderr_limit_bytes: 0,
        }),
        extension_phase_timings: Vec::new(),
        cargo_target: None,
    }
}

fn run_declared_result_parser(
    component: &Component,
    context: &crate::extension_execution::ExtensionExecutionContext,
    spec: &ParseSpec,
    stdout: &str,
    run_dir: &RunDir,
) -> homeboy_core::Result<()> {
    let Some(script_path) = spec.extension_script.as_deref() else {
        return Ok(());
    };
    let resolved_script = context.extension_path.join(script_path);
    if !resolved_script.is_file() {
        return Err(declared_result_parser_error(
            component,
            script_path,
            &resolved_script,
            "Declared test result parser script does not exist or is not a file".to_string(),
            None,
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata = resolved_script.metadata().map_err(|err| {
            declared_result_parser_error(
                component,
                script_path,
                &resolved_script,
                format!("Could not inspect declared test result parser script: {err}"),
                None,
            )
        })?;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(declared_result_parser_error(
                component,
                script_path,
                &resolved_script,
                "Declared test result parser script is not executable".to_string(),
                None,
            ));
        }
    }

    std::fs::create_dir_all(run_dir.path()).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("create declared result parser run dir".to_string()),
        )
    })?;

    let results_file = run_dir.step_file(run_dir::files::TEST_RESULTS);
    let provider_results_file = run_dir.path().join("files/test-results.json");
    let source_file = if results_file.is_file() {
        results_file
    } else if provider_results_file.is_file() {
        provider_results_file
    } else {
        let stdout_file = run_dir.path().join("test-output.txt");
        if let Some(parent) = stdout_file.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some("create parser stdout source directory".to_string()),
                )
            })?;
        }
        local_files::write_file_atomic(&stdout_file, stdout, "write test runner stdout")?;
        stdout_file
    };

    let mut args = vec![source_file.to_string_lossy().to_string()];
    args.extend(spec.adapters.iter().cloned());
    let settings_json = "{}";
    let mut env_vars = crate::extension::execution::build_capability_env(
        &context.extension_id,
        &component.id,
        &context.extension_path,
        std::path::Path::new(&component.local_path),
        settings_json,
        &run_dir.legacy_env_vars(),
    )?;
    let write_results_helper = write_test_results_helper(run_dir)?;
    env_vars.push((
        crate::extension::runtime_helper::WRITE_TEST_RESULTS_ENV.to_string(),
        write_results_helper.to_string_lossy().to_string(),
    ));
    env_vars.push((
        "HOMEBOY_TEST_RESULTS_FILE".to_string(),
        run_dir
            .step_file(run_dir::files::TEST_RESULTS)
            .to_string_lossy()
            .to_string(),
    ));
    env_vars.push((
        "HOMEBOY_RESULT_PARSE_ADAPTERS".to_string(),
        spec.adapters.join(" "),
    ));

    let output = crate::extension::execution::execute_capability_script(
        &context.extension_path,
        script_path,
        &args,
        &env_vars,
        None,
        None,
        crate::extension::execution::CapabilityScriptOptions {
            passthrough: false,
            stderr_passthrough: false,
            timeout: None,
        },
    )?;
    if !output.success {
        let mut command =
            homeboy_engine_primitives::shell::quote_path(&resolved_script.to_string_lossy());
        if !args.is_empty() {
            command.push(' ');
            command.push_str(&homeboy_engine_primitives::shell::quote_args(&args));
        }
        return Err(declared_result_parser_error(
            component,
            script_path,
            &resolved_script,
            format!(
                "Declared test result parser script failed with exit code {}",
                output.exit_code
            ),
            Some((command, output.exit_code, &output.stdout, &output.stderr)),
        ));
    }

    if !run_dir.step_file(run_dir::files::TEST_RESULTS).is_file() {
        let parser_stdout = output.stdout.trim();
        if !parser_stdout.is_empty() {
            let counts = parse_declared_parser_stdout_json(parser_stdout)?;
            let payload = serde_json::json!({
                "total": counts.total,
                "passed": counts.passed,
                "failed": counts.failed,
                "skipped": counts.skipped,
            });
            let results_path = run_dir.step_file(run_dir::files::TEST_RESULTS);
            if let Some(parent) = results_path.parent() {
                std::fs::create_dir_all(parent).map_err(|err| {
                    Error::internal_io(
                        err.to_string(),
                        Some("create parser stdout test results directory".to_string()),
                    )
                })?;
            }
            local_files::write_file_atomic(
                &results_path,
                &serde_json::to_string_pretty(&payload).map_err(|err| {
                    Error::internal_json(
                        err.to_string(),
                        Some("serialize parser stdout test results".to_string()),
                    )
                })?,
                "write parser stdout test results",
            )?;
        }
    }

    Ok(())
}

fn write_test_results_helper(run_dir: &RunDir) -> homeboy_core::Result<std::path::PathBuf> {
    let helper = run_dir.path().join("write-test-results.sh");
    local_files::write_file_atomic(
        &helper,
        include_str!("../runtime/write-test-results.sh"),
        "write test results runtime helper",
    )?;
    Ok(helper)
}

fn declared_result_parser_error(
    component: &Component,
    script_path: &str,
    resolved_script: &Path,
    problem: String,
    command_output: Option<(String, i32, &str, &str)>,
) -> Error {
    let (command, exit_code, stdout_tail, stderr_tail) =
        if let Some((command, exit_code, stdout, stderr)) = command_output {
            let (stdout_tail, _) = tail_lines(stdout, RAW_OUTPUT_TAIL_LINES);
            let (stderr_tail, _) = tail_lines(stderr, RAW_OUTPUT_TAIL_LINES);
            (Some(command), Some(exit_code), stdout_tail, stderr_tail)
        } else {
            (None, None, String::new(), String::new())
        };

    Error::new(
        ErrorCode::ConfigInvalidValue,
        format!(
            "{} for component '{}' at {}",
            problem,
            component.id,
            resolved_script.display()
        ),
        serde_json::json!({
            "component": component.id,
            "script_path": script_path,
            "resolved_script": resolved_script.to_string_lossy(),
            "problem": problem,
            "command": command,
            "exit_code": exit_code,
            "stdout_tail": stdout_tail,
            "stderr_tail": stderr_tail,
        }),
    )
}

fn parse_declared_parser_stdout_json(stdout: &str) -> homeboy_core::Result<TestCounts> {
    let value: serde_json::Value = serde_json::from_str(stdout).map_err(|err| {
        Error::validation_invalid_json(
            err,
            Some("parse test result adapter stdout".to_string()),
            Some(stdout.to_string()),
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        Error::validation_invalid_argument(
            "test.result_parse.extension_script.stdout",
            "expected a JSON object with unsigned integer total, passed, failed, and skipped fields",
            None,
            None,
        )
    })?;

    let count = |field: &str| -> homeboy_core::Result<u64> {
        object
            .get(field)
            .and_then(|value| value.as_u64())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    format!("test.result_parse.extension_script.stdout.{field}"),
                    "expected an unsigned integer count",
                    None,
                    None,
                )
            })
    };

    Ok(TestCounts::new(
        count("total")?,
        count("passed")?,
        count("failed")?,
        count("skipped")?,
    ))
}

pub fn run_self_check_test_workflow(
    component: &Component,
    source_path: &Path,
    component_label: String,
    json_summary: bool,
) -> homeboy_core::Result<TestRunWorkflowResult> {
    run_self_check_test_workflow_with_progress(
        component,
        source_path,
        component_label,
        json_summary,
        None,
        None,
    )
}

pub fn run_self_check_test_workflow_with_progress(
    component: &Component,
    source_path: &Path,
    component_label: String,
    json_summary: bool,
    run_dir: Option<&RunDir>,
    observation: Option<&homeboy_core::observation::ActiveObservation>,
) -> homeboy_core::Result<TestRunWorkflowResult> {
    let output = extension::self_check::run_self_checks_with_passthrough_and_progress(
        component,
        ExtensionCapability::Test,
        source_path,
        !json_summary,
        run_dir,
        observation,
    )?;
    let status = if output.success { "passed" } else { "failed" }.to_string();
    let raw_output = (!output.success).then(|| {
        let (stdout_tail, stdout_truncated) = tail_lines(&output.stdout, RAW_OUTPUT_TAIL_LINES);
        let (stderr_tail, stderr_truncated) = tail_lines(&output.stderr, RAW_OUTPUT_TAIL_LINES);
        RawTestOutput {
            stdout_tail: homeboy_core::redaction::redact_string(&stdout_tail),
            stderr_tail: homeboy_core::redaction::redact_string(&stderr_tail),
            truncated: stdout_truncated
                || stderr_truncated
                || output.capture.stdout.truncated
                || output.capture.stderr.truncated,
            stdout_truncated: output.capture.stdout.truncated || stdout_truncated,
            stderr_truncated: output.capture.stderr.truncated || stderr_truncated,
            stdout_seen_bytes: output.capture.stdout.seen_bytes,
            stdout_retained_bytes: output.stdout.len(),
            stderr_seen_bytes: output.capture.stderr.seen_bytes,
            stderr_retained_bytes: output.stderr.len(),
            stdout_limit_bytes: output.capture.stdout.limit_bytes,
            stderr_limit_bytes: output.capture.stderr.limit_bytes,
        }
    });

    Ok(TestRunWorkflowResult {
        status,
        component: component_label,
        exit_code: output.exit_code,
        runner_exit_code: Some(output.exit_code),
        test_counts: None,
        test_inventory: None,
        test_inventory_rejection: None,
        test_runtime_evidence: Some(TestRuntimeEvidence::InvalidEvidence {
            reason: "self-check execution does not expose stable test identities".to_string(),
        }),
        test_durations: None,
        findings: None,
        failure_analysis_input: None,
        coverage: None,
        baseline_comparison: None,
        analysis: None,
        autofix: None,
        hints: (!output.success).then(|| {
            vec![format!(
                "Fix the failing self-check command declared in {}'s homeboy.json scripts.test",
                component.id
            )]
        }),
        test_scope: None,
        summary: if json_summary {
            Some(build_test_summary(None, None, output.exit_code))
        } else {
            None
        },
        raw_output,
        extension_phase_timings: Vec::new(),
        cargo_target: output.cargo_target,
    })
}

fn merge_reported_test_artifact_locators(
    timings: &mut Vec<ExtensionPhaseTiming>,
    stdout: &str,
    stderr: &str,
) {
    const MAX_LOCATORS: usize = 32;
    let locator = regex::Regex::new(r"artifact://files/[A-Za-z0-9._/-]+")
        .expect("artifact locator regex is valid");
    let mut reported = std::collections::BTreeSet::new();
    for value in [stdout, stderr] {
        for candidate in locator.find_iter(value).map(|matched| matched.as_str()) {
            let relative = candidate.trim_start_matches("artifact://files/");
            let path = std::path::Path::new(relative);
            if !path.as_os_str().is_empty()
                && !path.is_absolute()
                && path
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
            {
                reported.insert(candidate.to_string());
            }
        }
    }
    if reported.is_empty() {
        return;
    }
    let existing = timings
        .iter()
        .flat_map(|timing| timing.artifacts.iter())
        .filter_map(|artifact| artifact.get("ref").and_then(serde_json::Value::as_str))
        .collect::<std::collections::BTreeSet<_>>();
    let artifacts = reported
        .into_iter()
        .filter(|candidate| !existing.contains(candidate.as_str()))
        .take(MAX_LOCATORS)
        .map(|reference| serde_json::json!({ "ref": reference }))
        .collect::<Vec<_>>();
    if !artifacts.is_empty() {
        timings.push(ExtensionPhaseTiming {
            name: "provider-reported-test-artifacts".to_string(),
            duration_ms: 0,
            status: Some("reported".to_string()),
            message: None,
            artifacts,
            metadata: Default::default(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::test::TestFailure;
    use homeboy_core::component::{ComponentScriptsConfig, ScopedExtensionConfig};
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    static CONDITIONAL_SECRET_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn conditional_secret_env_guard() -> std::sync::MutexGuard<'static, ()> {
        CONDITIONAL_SECRET_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("conditional secret env lock")
    }

    fn failed_identity(id: &str) -> TestFailure {
        TestFailure {
            test_name: id.to_string(),
            test_file: String::new(),
            error_type: "assertion".to_string(),
            message: "failed".to_string(),
            source_file: String::new(),
            source_line: 0,
        }
    }

    fn runtime_fixture_profile(root: &Path) -> InventoryProfile {
        std::fs::write(root.join("fixture.root"), "root\n").unwrap();
        std::fs::write(root.join("suite.fixture"), "suite\n").unwrap();
        InventoryProfile {
            root_markers: vec!["fixture.root".to_string()],
            fingerprint_names: vec!["fixture.root".to_string()],
            fingerprint_extensions: vec!["fixture".to_string()],
            fingerprint_skip_dirs: Vec::new(),
            runner_commands: BTreeMap::from([(
                "rustc".to_string(),
                vec!["rustc".to_string(), "--version".to_string()],
            )]),
        }
    }

    #[test]
    fn shard_runtime_evidence_normalizes_observed_failures_against_exact_plan() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profile = runtime_fixture_profile(temp.path());
        let runner_fingerprint = runner_fingerprint(temp.path(), "rustc", &profile).unwrap();
        let workspace_fingerprint = workspace_fingerprint(temp.path(), &profile).unwrap();
        let manifest = temp.path().join("shard.json");
        std::fs::write(
            &manifest,
            serde_json::to_vec(&serde_json::json!({
                "schema": "homeboy/test-shard-manifest/v1",
                "id": "shard-2",
                "runner": "rustc",
                "runner_fingerprint": runner_fingerprint,
                "workspace_fingerprint": workspace_fingerprint,
                "inventory_fingerprint": "c".repeat(64),
                "tests": ["suite::passes", "suite::fails"],
                "estimated_duration_ms": 120000,
            }))
            .unwrap(),
        )
        .unwrap();
        let failures = TestAnalysisInput {
            failures: vec![failed_identity(" suite::fails ")],
            total: 2,
            passed: 1,
        };

        let evidence = runtime_test_evidence(
            &[(
                TEST_SHARD_MANIFEST_ENV.to_string(),
                manifest.to_string_lossy().to_string(),
            )],
            temp.path(),
            Some(&profile),
            false,
            Some(&TestCounts::new(2, 1, 1, 0)),
            Some(&failures),
            None,
        );

        let TestRuntimeEvidence::Complete {
            tests,
            failed_test_ids,
            execution_fingerprint,
            ..
        } = evidence
        else {
            panic!("expected complete shard evidence");
        };
        assert_eq!(
            tests.into_iter().map(|test| test.id).collect::<Vec<_>>(),
            vec!["suite::fails", "suite::passes"]
        );
        assert_eq!(failed_test_ids, vec!["suite::fails"]);
        assert!(canonical_sha256(&execution_fingerprint));
    }

    #[test]
    fn unknown_duplicate_or_missing_red_runtime_details_are_invalid_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");
        let profile = runtime_fixture_profile(temp.path());
        let runner_fingerprint = runner_fingerprint(temp.path(), "rustc", &profile).unwrap();
        let workspace_fingerprint = workspace_fingerprint(temp.path(), &profile).unwrap();
        let manifest = temp.path().join("shard.json");
        std::fs::write(
            &manifest,
            serde_json::to_vec(&serde_json::json!({
                "schema": "homeboy/test-shard-manifest/v1",
                "id": "shard-1",
                "runner": "rustc",
                "runner_fingerprint": runner_fingerprint,
                "workspace_fingerprint": workspace_fingerprint,
                "inventory_fingerprint": "c".repeat(64),
                "tests": ["suite::planned"],
            }))
            .unwrap(),
        )
        .unwrap();

        let ci_env = [(
            TEST_SHARD_MANIFEST_ENV.to_string(),
            manifest.to_string_lossy().to_string(),
        )];
        let cases = [
            (None, "did not expose an observed failed test ID"),
            (
                Some(TestAnalysisInput {
                    failures: vec![failed_identity("unknown test")],
                    total: 1,
                    passed: 0,
                }),
                "did not expose stable test IDs",
            ),
            (
                Some(TestAnalysisInput {
                    failures: vec![
                        failed_identity("suite::planned"),
                        failed_identity("suite::planned"),
                    ],
                    total: 1,
                    passed: 0,
                }),
                "duplicate test IDs",
            ),
        ];

        for (details, expected_reason) in cases {
            let evidence = runtime_test_evidence(
                &ci_env,
                temp.path(),
                Some(&profile),
                false,
                Some(&TestCounts::new(1, 0, 1, 0)),
                details.as_ref(),
                None,
            );
            assert!(matches!(
                evidence,
                TestRuntimeEvidence::InvalidEvidence { ref reason }
                    if reason.contains(expected_reason)
            ));
        }

        let countless = runtime_test_evidence(
            &ci_env,
            temp.path(),
            Some(&profile),
            false,
            None,
            Some(&TestAnalysisInput {
                failures: vec![failed_identity("suite::planned")],
                total: 1,
                passed: 0,
            }),
            None,
        );
        assert!(matches!(
            countless,
            TestRuntimeEvidence::InvalidEvidence { ref reason }
                if reason.contains("structured TestCounts")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn adapter_inventory_is_canonically_validated_and_selects_only_executed_ids() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("fixture.root"), "root\n").unwrap();
        std::fs::write(temp.path().join("suite.fixture"), "suite\n").unwrap();
        let profile = InventoryProfile {
            root_markers: vec!["fixture.root".to_string()],
            fingerprint_names: vec!["fixture.root".to_string()],
            fingerprint_extensions: vec!["fixture".to_string()],
            fingerprint_skip_dirs: Vec::new(),
            runner_commands: BTreeMap::from([(
                "fixture".to_string(),
                vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf fixture-version".to_string(),
                ],
            )]),
        };
        let runner_fingerprint = runner_fingerprint(temp.path(), "fixture", &profile).unwrap();
        let workspace_fingerprint = workspace_fingerprint(temp.path(), &profile).unwrap();
        let mut inventory = TestInventoryEvidence {
            schema: TEST_INVENTORY_SCHEMA.to_string(),
            runner: "fixture".to_string(),
            runner_fingerprint,
            workspace_fingerprint,
            tests: vec![
                TestInventoryTest {
                    id: "suite::runs".to_string(),
                    package: "fixture".to_string(),
                    target: "suite".to_string(),
                    target_kind: "test".to_string(),
                    name: "runs".to_string(),
                    expected_outcome: Some("executed".to_string()),
                },
                TestInventoryTest {
                    id: "suite::ignored".to_string(),
                    package: "fixture".to_string(),
                    target: "suite".to_string(),
                    target_kind: "test".to_string(),
                    name: "ignored".to_string(),
                    expected_outcome: Some("skipped".to_string()),
                },
            ],
            inventory_fingerprint: String::new(),
            fallback_reason: None,
        };
        inventory.inventory_fingerprint = homeboy_engine_primitives::content_hash::sha256_hex(
            &canonical_inventory_json(&inventory),
        );
        let path = temp.path().join("inventory.json");
        std::fs::write(&path, serde_json::to_vec(&inventory).unwrap()).unwrap();

        let plan = runtime_plan_from_inventory(path.to_str().unwrap(), temp.path(), &profile)
            .expect("validated adapter inventory");

        assert_eq!(plan.tests, vec!["suite::runs"]);
        assert!(canonical_sha256(&plan.execution_fingerprint));
    }

    fn assert_artifact_tree_excludes(root: &Path, needle: &str) {
        for entry in std::fs::read_dir(root).expect("read artifact directory") {
            let path = entry.expect("artifact entry").path();
            if path.is_dir() {
                assert_artifact_tree_excludes(&path, needle);
            } else if let Ok(contents) = std::fs::read_to_string(&path) {
                assert!(
                    !contents.contains(needle),
                    "artifact {} leaked declared secret",
                    path.display()
                );
            }
        }
    }

    fn conditional_test_component(home: &Path, source: &Path, mode: &str) -> Component {
        let extension_dir = home.join(".config/homeboy/extensions/conditional-secret-fixture");
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::write(
            extension_dir.join("conditional-secret-fixture.json"),
            r#"{
                "name":"Conditional secret fixture",
                "version":"1.0.0",
                "settings":[
                    {"id":"service","label":"Service","type":"object","default":{"mode":"local"}}
                ],
                "test":{
                    "extension_script":"test.sh",
                    "secret_env_projections":[{
                        "when":{"path":["service","mode"],"equals":"remote"},
                        "names_path":["service","secret_env"]
                    }]
                }
            }"#,
        )
        .expect("extension manifest");
        std::fs::write(
            extension_dir.join("test.sh"),
            "#!/bin/sh\nprintf 'first=%s second=%s settings=%s\\n' \"${FIRST_PROJECTED_SECRET-unset}\" \"${SECOND_PROJECTED_SECRET-unset}\" \"$HOMEBOY_SETTINGS_JSON\"\nexit 1\n",
        )
        .expect("extension script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = extension_dir.join("test.sh");
            let mut permissions = std::fs::metadata(&script)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(script, permissions).expect("executable script");
        }

        Component {
            id: "conditional-secret-consumer".to_string(),
            local_path: source.to_string_lossy().to_string(),
            extensions: Some(HashMap::from([(
                "conditional-secret-fixture".to_string(),
                ScopedExtensionConfig {
                    settings: HashMap::from([(
                        "service".to_string(),
                        serde_json::json!({
                            "mode": mode,
                            "secret_env": {
                                "first": "FIRST_PROJECTED_SECRET",
                                "second": "SECOND_PROJECTED_SECRET"
                            }
                        }),
                    )]),
                    ..Default::default()
                },
            )])),
            ..Default::default()
        }
    }

    fn fixture_workflow_args(component: &Component) -> TestRunWorkflowArgs {
        TestRunWorkflowArgs {
            component_label: component.id.clone(),
            component_id: component.id.clone(),
            path_override: None,
            settings: Vec::new(),
            settings_json: Vec::new(),
            skip_lint: true,
            coverage: false,
            coverage_min: None,
            analyze: false,
            baseline_flags: Default::default(),
            changed_since: None,
            precomputed_changed_files: None,
            json_summary: true,
            restore_checkout: false,
            ci_env: Vec::new(),
            passthrough_args: Vec::new(),
        }
    }

    #[test]
    fn review_test_projects_matching_secrets_and_skips_non_matching_mode() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let _guard = conditional_secret_env_guard();
            let source = tempfile::tempdir().expect("source dir");
            std::env::set_var("FIRST_PROJECTED_SECRET", "first-review-secret");
            std::env::set_var("SECOND_PROJECTED_SECRET", "second-review-secret");

            let matching = conditional_test_component(home.path(), source.path(), "remote");
            let matching_run = RunDir::create().expect("matching run dir");
            let matching_result = run_main_test_workflow(
                &matching,
                source.path(),
                fixture_workflow_args(&matching),
                &matching_run,
            )
            .expect("matching child runs");
            let rendered = serde_json::to_string(&matching_result).expect("review result");
            assert!(rendered.contains("[REDACTED]"));
            for value in ["first-review-secret", "second-review-secret"] {
                assert!(!rendered.contains(value));
                assert_artifact_tree_excludes(matching_run.path(), value);
            }
            let supervision = std::fs::read_to_string(
                matching_run
                    .path()
                    .join(homeboy_core::engine::run_dir::files::CHILD_SUPERVISION),
            )
            .expect("child supervision evidence");
            assert!(supervision.contains("[REDACTED]"));

            std::env::remove_var("FIRST_PROJECTED_SECRET");
            std::env::remove_var("SECOND_PROJECTED_SECRET");
            let local = conditional_test_component(home.path(), source.path(), "local");
            let local_run = RunDir::create().expect("local run dir");
            let local_result = run_main_test_workflow(
                &local,
                source.path(),
                fixture_workflow_args(&local),
                &local_run,
            )
            .expect("non-matching child needs no secrets");
            assert_eq!(local_result.exit_code, 1, "non-matching child executed");
            let local_stdout = std::fs::read_to_string(
                local_run
                    .path()
                    .join("validation-progress/command-1-stdout.log"),
            )
            .expect("non-matching stdout artifact");
            assert!(local_stdout.contains("first=unset second=unset"));
        });
    }

    /// The names callers can resolve ahead of a run must match what the runner
    /// itself would require, so a pre-run gate cannot disagree with the gate it
    /// is meant to front. (#10402)
    #[test]
    fn declared_secret_env_names_match_the_runner_requirement_without_spawning() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let _guard = conditional_secret_env_guard();
            let source = tempfile::tempdir().expect("source dir");
            let marker = source.path().join("declared-names-child-ran");

            let matching = conditional_test_component(home.path(), source.path(), "remote");
            std::fs::write(
                home.path()
                    .join(".config/homeboy/extensions/conditional-secret-fixture/test.sh"),
                format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
            )
            .expect("marker script");

            assert_eq!(
                crate::extension::test::declared_secret_env_names(&matching)
                    .expect("matching declaration"),
                vec!["FIRST_PROJECTED_SECRET", "SECOND_PROJECTED_SECRET"]
            );

            let local = conditional_test_component(home.path(), source.path(), "local");
            assert!(crate::extension::test::declared_secret_env_names(&local)
                .expect("non-matching declaration")
                .is_empty());

            assert!(
                !marker.exists(),
                "resolving declared names must not spawn the test child"
            );
        });
    }

    #[test]
    fn review_test_missing_projected_secret_fails_before_spawn() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let _guard = conditional_secret_env_guard();
            let source = tempfile::tempdir().expect("source dir");
            let marker = source.path().join("child-ran");
            let component = conditional_test_component(home.path(), source.path(), "remote");
            std::fs::write(
                home.path()
                    .join(".config/homeboy/extensions/conditional-secret-fixture/test.sh"),
                format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
            )
            .expect("marker script");
            std::env::remove_var("FIRST_PROJECTED_SECRET");
            std::env::remove_var("SECOND_PROJECTED_SECRET");

            let error = run_main_test_workflow(
                &component,
                source.path(),
                fixture_workflow_args(&component),
                &RunDir::create().expect("run dir"),
            )
            .expect_err("missing projected identity must fail before spawn");
            assert!(error.message.contains("FIRST_PROJECTED_SECRET"));
            assert!(!marker.exists());
        });
    }

    #[test]
    fn review_test_injects_declared_secret_and_redacts_child_evidence() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source = tempfile::tempdir().expect("source dir");
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions/secret-test-fixture");
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            std::fs::write(
                extension_dir.join("secret-test-fixture.json"),
                r#"{
                    "name":"Secret test fixture",
                    "version":"1.0.0",
                    "test":{
                        "extension_script":"test.sh",
                        "secret_env":{"DECLARED_TEST_SECRET":"DECLARED_TEST_SECRET"}
                    }
                }"#,
            )
            .expect("extension manifest");
            std::fs::write(
                extension_dir.join("test.sh"),
                "#!/bin/sh\nprintf 'received=%s\\n' \"$DECLARED_TEST_SECRET\"\nexit 1\n",
            )
            .expect("extension script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let script = extension_dir.join("test.sh");
                let mut permissions = std::fs::metadata(&script)
                    .expect("script metadata")
                    .permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(script, permissions).expect("executable script");
            }

            let component = Component {
                id: "secret-consumer".to_string(),
                local_path: source.path().to_string_lossy().to_string(),
                extensions: Some(HashMap::from([(
                    "secret-test-fixture".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Default::default()
            };
            let run_dir = RunDir::create().expect("run dir");
            std::env::set_var("DECLARED_TEST_SECRET", "review-fixture-secret");
            let result = run_main_test_workflow(
                &component,
                source.path(),
                TestRunWorkflowArgs {
                    component_label: component.id.clone(),
                    component_id: component.id.clone(),
                    path_override: None,
                    settings: Vec::new(),
                    settings_json: Vec::new(),
                    skip_lint: true,
                    coverage: false,
                    coverage_min: None,
                    analyze: false,
                    baseline_flags: Default::default(),
                    changed_since: None,
                    precomputed_changed_files: None,
                    json_summary: true,
                    restore_checkout: false,
                    ci_env: Vec::new(),
                    passthrough_args: Vec::new(),
                },
                &run_dir,
            )
            .expect("review workflow reaches secret-bearing child");
            std::env::remove_var("DECLARED_TEST_SECRET");

            assert_eq!(result.exit_code, 1, "child ran after secret injection");
            let rendered = serde_json::to_string(&result).expect("review result JSON");
            assert!(rendered.contains("[REDACTED]"));
            assert!(!rendered.contains("review-fixture-secret"));
            let artifact = std::fs::read_to_string(
                run_dir
                    .path()
                    .join("validation-progress/command-1-stdout.log"),
            )
            .expect("review stdout artifact");
            assert!(artifact.contains("[REDACTED]"));
            assert!(!artifact.contains("review-fixture-secret"));
            assert_artifact_tree_excludes(run_dir.path(), "review-fixture-secret");
        });
    }

    /// Two lines of a real cargo test run: one binary that finished and
    /// reported its time, and one that started and never did.
    const PARTIAL_RUNNER_OUTPUT: &str = concat!(
        "     Running tests/fast.rs (/t/deps/fast-0123456789abcdef)\n",
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.00s\n",
        "     Running tests/slow.rs (/t/deps/slow-fedcba9876543210)\n",
        "running 1 test\n",
        "test the_slow_one has been running for over 60 seconds\n",
    );

    #[test]
    fn a_killed_test_child_still_yields_labelled_partial_timings() {
        // `execute_capability_script` returns partial stdout on timeout (see
        // its own test), so the durations path receives exactly this shape.
        let dir = tempfile::tempdir().expect("tempdir");
        let durations = collect_test_durations(
            &dir.path().join("absent.json"),
            PARTIAL_RUNNER_OUTPUT,
            1500.0,
            true,
            Duration::from_secs(1500),
        )
        .expect("partial output still yields durations");

        assert!(!durations.complete, "a killed run is never a full picture");
        assert!(durations
            .incomplete_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("1500s budget")));
        assert_eq!(
            durations.measured_seconds,
            Some(4.0),
            "only what actually reported is counted"
        );
        assert!(durations
            .slow
            .iter()
            .any(|finding| finding.rule == "unfinished-test-unit"
                && finding.name.contains("the_slow_one")
                && finding.seconds.is_none()));
    }

    #[test]
    fn unparseable_output_still_reports_the_wall_clock_and_no_fabricated_totals() {
        // Homeboy's own measurement of the child is real evidence even when the
        // runner printed nothing timeable, so it is reported. What must never
        // be invented is a *measured* total: no binary reported, so the sum is
        // unknown, not zero.
        let dir = tempfile::tempdir().expect("tempdir");
        let durations = collect_test_durations(
            &dir.path().join("absent.json"),
            "error: could not compile `homeboy`\n",
            12.0,
            false,
            Duration::from_secs(1500),
        )
        .expect("the wall clock is always available");

        assert_eq!(durations.phase_seconds, Some(12.0));
        assert_eq!(
            durations.measured_seconds, None,
            "nothing reported means unknown, never zero"
        );
        assert!(durations.binaries.is_empty());
        assert!(durations.tests.is_empty());
        assert!(durations.slow.is_empty());
        assert!(durations.complete);
    }

    #[test]
    fn a_declared_durations_sidecar_wins_over_stdout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test-durations.json");
        std::fs::write(
            &path,
            r#"{"measured_seconds":99.0,"binaries":[{"name":"declared","seconds":99.0,"source":"binary-summary"}]}"#,
        )
        .expect("write sidecar");

        let durations = collect_test_durations(
            &path,
            PARTIAL_RUNNER_OUTPUT,
            120.0,
            false,
            Duration::from_secs(1500),
        )
        .expect("sidecar is consumed");

        assert_eq!(durations.measured_seconds, Some(99.0));
        assert_eq!(durations.binaries.len(), 1);
        // Homeboy's own measurements still fill the gaps the sidecar left.
        assert_eq!(durations.phase_seconds, Some(120.0));
        assert_eq!(durations.budget_seconds, Some(1500.0));
    }

    #[test]
    fn reported_artifact_locators_are_normalized_and_deduplicated() {
        let mut timings = Vec::new();
        merge_reported_test_artifact_locators(
            &mut timings,
            "artifact://files/test-results.json artifact://files/../escape.log",
            "artifact://files/phpunit-output.log artifact://files/test-results.json",
        );

        assert_eq!(timings.len(), 1);
        assert_eq!(timings[0].artifacts.len(), 2);
        assert_eq!(
            timings[0].artifacts[0]["ref"],
            "artifact://files/phpunit-output.log"
        );
        assert_eq!(
            timings[0].artifacts[1]["ref"],
            "artifact://files/test-results.json"
        );
    }
    use homeboy_core::test_support::{exec_capable_tempdir, with_isolated_home};

    fn run_git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn clean_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().expect("temp dir");
        run_git(temp.path(), &["init", "-q", "--initial-branch", "main"]);
        run_git(
            temp.path(),
            &["config", "user.email", "homeboy@example.com"],
        );
        run_git(temp.path(), &["config", "user.name", "Homeboy Test"]);
        std::fs::write(temp.path().join("tracked.txt"), "original\n").expect("tracked file");
        run_git(temp.path(), &["add", "tracked.txt"]);
        run_git(temp.path(), &["commit", "-q", "-m", "fixture"]);
        temp
    }

    fn assert_clean(dir: &Path) {
        assert_eq!(run_git(dir, &["status", "--porcelain=v1"]), "");
        assert_eq!(
            std::fs::read_to_string(dir.join("tracked.txt")).expect("tracked file"),
            "original\n"
        );
        assert!(!dir.join("generated.txt").exists());
    }

    #[test]
    fn setup_failure_returns_structured_result_and_restores_clean_checkout() {
        let repo = clean_repo();

        let result = run_review_test_lifecycle(repo.path(), "fixture".to_string(), true, || {
            std::fs::write(repo.path().join("tracked.txt"), "setup mutation\n")
                .expect("mutate tracked file");
            std::fs::write(repo.path().join("generated.txt"), "setup artifact\n")
                .expect("write setup artifact");
            Err(Error::internal_unexpected("fixture setup failed"))
        })
        .expect("setup failure should become a test result");
        let (output, exit_code) = super::super::report::from_main_workflow(result);
        let json = serde_json::to_value(output).expect("structured output");

        assert_eq!(exit_code, 2);
        assert_eq!(json["passed"], false);
        assert_eq!(json["status"], "failed");
        assert_eq!(json["failure"]["category"], "infrastructure");
        assert!(json["raw_output"]["stderr_tail"]
            .as_str()
            .unwrap_or_default()
            .contains("fixture setup failed"));
        assert_clean(repo.path());
    }

    #[test]
    fn test_failure_returns_structured_result_and_restores_clean_checkout() {
        let repo = clean_repo();

        let result = run_review_test_lifecycle(repo.path(), "fixture".to_string(), true, || {
            std::fs::write(repo.path().join("tracked.txt"), "test mutation\n")
                .expect("mutate tracked file");
            std::fs::write(repo.path().join("generated.txt"), "test artifact\n")
                .expect("write test artifact");
            Ok(TestRunWorkflowResult {
                status: "failed".to_string(),
                component: "fixture".to_string(),
                exit_code: 1,
                runner_exit_code: None,
                test_counts: Some(TestCounts::new(1, 0, 1, 0)),
                test_inventory: None,
                test_inventory_rejection: None,
                test_runtime_evidence: None,
                test_durations: None,
                findings: None,
                failure_analysis_input: None,
                coverage: None,
                baseline_comparison: None,
                analysis: None,
                autofix: None,
                hints: None,
                test_scope: None,
                summary: Some(build_test_summary(
                    Some(&TestCounts::new(1, 0, 1, 0)),
                    None,
                    1,
                )),
                raw_output: None,
                extension_phase_timings: Vec::new(),
                cargo_target: None,
            })
        })
        .expect("test failure should remain a test result");
        let (output, exit_code) = super::super::report::from_main_workflow(result);
        let json = serde_json::to_value(output).expect("structured output");

        assert_eq!(exit_code, 1);
        assert_eq!(json["passed"], false);
        assert_eq!(json["test_counts"]["failed"], 1);
        assert_eq!(json["failure"]["category"], "findings");
        assert_clean(repo.path());
    }

    #[test]
    fn malformed_failure_sidecar_is_ignored_with_a_diagnostic_not_an_error() {
        let temp = tempfile::tempdir().expect("temp dir");
        let sidecar = temp.path().join("test-failures.json");
        // A malformed failure object — here `source_line` is a string where a
        // number is required — the kind of schema-invalid sidecar a failed
        // recipe can emit. It must not abort the run and mask the real failure. (#8489)
        std::fs::write(
            &sidecar,
            r#"[{"test_id":"bootstrap","message":"input.materialize timeout","error_type":"timeout","source_line":"not-a-number"}]"#,
        )
        .expect("write malformed sidecar");

        let (input, diagnostic) = parse_optional_failure_sidecar(&sidecar);

        assert!(
            input.is_none(),
            "a malformed sidecar must not yield enrichment input"
        );
        let diagnostic = diagnostic.expect("a malformed sidecar must attach a diagnostic");
        assert!(
            diagnostic.contains("malformed test-failures sidecar"),
            "diagnostic: {diagnostic}"
        );
        assert!(
            diagnostic.contains("primary run result is preserved"),
            "diagnostic: {diagnostic}"
        );
    }

    #[test]
    fn valid_failure_sidecar_parses_without_a_diagnostic() {
        let temp = tempfile::tempdir().expect("temp dir");
        let sidecar = temp.path().join("test-failures.json");
        std::fs::write(
            &sidecar,
            r#"[{"test_id":"SomeTest::method","message":"assertion failed","error_type":"assertion"}]"#,
        )
        .expect("write valid sidecar");

        let (input, diagnostic) = parse_optional_failure_sidecar(&sidecar);

        let input = input.expect("a valid sidecar must yield enrichment input");
        assert_eq!(input.failures.len(), 1);
        assert_eq!(input.failures[0].error_type, "assertion");
        assert!(
            diagnostic.is_none(),
            "a valid sidecar must not attach a diagnostic"
        );
    }

    #[test]
    fn compiler_diagnostics_become_release_visible_findings_without_a_sidecar() {
        let output = r#"error[E0425]: cannot find function `rollback_refresh_error` in this scope
   --> crates/homeboy-lab-runner/src/homeboy_refresh/tests/part_a.rs:608:21
    |
608 |         let error = rollback_refresh_error::<()>(
    |                     ^^^^^^^^^^^^^^^^^^^^^^ not found in this scope
"#;
        let input = parse_compiler_failures(output, "").expect("compiler finding");
        let findings = homeboy_findings_from_test_analysis_input(&input).expect("findings");
        let (report, exit_code) = super::super::report::from_main_workflow(TestRunWorkflowResult {
            status: "failed".to_string(),
            component: "homeboy".to_string(),
            exit_code: 101,
            runner_exit_code: None,
            test_counts: None,
            test_inventory: None,
            test_inventory_rejection: None,
            test_runtime_evidence: Some(TestRuntimeEvidence::InvalidEvidence {
                reason: "compiler failure did not map to an executed test identity".to_string(),
            }),
            test_durations: None,
            findings: Some(findings),
            failure_analysis_input: Some(input),
            coverage: None,
            baseline_comparison: None,
            analysis: None,
            autofix: None,
            hints: None,
            test_scope: None,
            summary: None,
            raw_output: None,
            extension_phase_timings: Vec::new(),
            cargo_target: None,
        });
        let json = serde_json::to_value(report).expect("report json");

        assert_eq!(exit_code, 101);
        assert_eq!(json["failure"]["category"], "findings");
        assert_eq!(json["findings"][0]["rule"], "compiler_error:E0425");
        assert!(json["findings"][0]["message"]
            .as_str()
            .expect("finding message")
            .contains("rollback_refresh_error"));
        assert_eq!(
            json["findings"][0]["file"],
            "crates/homeboy-lab-runner/src/homeboy_refresh/tests/part_a.rs"
        );
        assert_eq!(json["findings"][0]["line"], 608);
    }

    #[test]
    fn absent_failure_sidecar_yields_no_input_and_no_diagnostic() {
        let temp = tempfile::tempdir().expect("temp dir");
        let sidecar = temp.path().join("does-not-exist.json");

        let (input, diagnostic) = parse_optional_failure_sidecar(&sidecar);

        assert!(input.is_none());
        assert!(diagnostic.is_none());
    }

    #[test]
    fn tail_lines_returns_full_text_when_under_limit() {
        let input = "line 1\nline 2\nline 3";
        let (tail, truncated) = tail_lines(input, 10);
        assert_eq!(tail, input);
        assert!(!truncated);
    }

    #[test]
    fn tail_lines_handles_empty_input() {
        let (tail, truncated) = tail_lines("", 10);
        assert_eq!(tail, "");
        assert!(!truncated);
    }

    #[test]
    fn tail_lines_at_exact_limit_is_not_truncated() {
        let input = "a\nb\nc";
        let (tail, truncated) = tail_lines(input, 3);
        assert_eq!(tail, input);
        assert!(!truncated);
    }

    #[test]
    fn test_findings_from_analysis_input_preserve_failure_details() {
        let input = TestAnalysisInput {
            failures: vec![TestFailure {
                test_name: "tests::fails".to_string(),
                test_file: "tests/fails.rs".to_string(),
                error_type: "AssertionFailed".to_string(),
                message: "expected true".to_string(),
                source_file: "src/lib.rs".to_string(),
                source_line: 42,
            }],
            total: 2,
            passed: 1,
        };

        let findings = homeboy_findings_from_test_analysis_input(&input).expect("findings");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].metadata_json()["test_name"], "tests::fails");
        assert_eq!(findings[0].message, "AssertionFailed: expected true");
        assert_eq!(findings[0].location.file.as_deref(), Some("tests/fails.rs"));
        assert_eq!(findings[0].location.line, Some(42));
    }

    #[test]
    fn status_requires_successful_runner_even_with_zero_failures() {
        let counts = TestCounts::new(3, 3, 0, 0);
        assert_eq!(test_run_status(false, Some(&counts), false), "failed");
    }

    #[test]
    fn status_passes_successful_runner_with_zero_failures() {
        let counts = TestCounts::new(3, 3, 0, 0);
        assert_eq!(test_run_status(true, Some(&counts), false), "passed");
    }

    #[test]
    fn status_fails_successful_runner_with_parsed_failures() {
        let counts = TestCounts::new(3, 2, 1, 0);
        assert_eq!(test_run_status(true, Some(&counts), false), "failed");
    }

    #[test]
    fn status_fails_successful_runner_without_result_evidence() {
        assert_eq!(test_run_status(true, None, false), "failed");
    }

    #[cfg(not(unix))]
    #[test]
    fn non_unix_inventory_mode_stays_fail_closed_without_disabling_normal_tests() {
        assert_eq!(
            test_run_status_with_inventory(true, None, false, true, None),
            "failed",
            "inventory-only mode requires descriptor-bound evidence unavailable on this platform"
        );
        assert_eq!(
            test_run_status_with_inventory(
                true,
                Some(&TestCounts::new(3, 3, 0, 0)),
                false,
                false,
                None,
            ),
            "passed",
            "normal test execution must continue to use parsed test counts"
        );
    }

    #[cfg(unix)]
    #[test]
    fn inventory_success_is_not_re_finalized_from_execution_counts() {
        let mut workflow = failed_test_workflow(
            "fixture".to_string(),
            false,
            &Error::internal_unexpected("fixture"),
        );
        workflow.status = "passed".to_string();
        workflow.exit_code = 0;
        workflow.runner_exit_code = Some(0);
        workflow.test_counts = Some(TestCounts::new(1, 0, 1, 0));
        workflow.test_inventory = Some(TestInventoryOutput {
            schema: TEST_INVENTORY_SCHEMA.to_string(),
            runner: "nextest".to_string(),
            runner_fingerprint: "a".repeat(64),
            workspace_fingerprint: "b".repeat(64),
            test_count: 1,
            inventory_fingerprint: "c".repeat(64),
            fallback_reason: None,
        });

        finalize_test_result_after_artifact_hydration(&mut workflow);

        assert_eq!(workflow.status, "passed");
        assert_eq!(workflow.exit_code, 0);
    }

    #[cfg(unix)]
    #[test]
    fn inventory_mode_requires_explicit_valid_inventory_evidence() {
        let temp = tempfile::tempdir().expect("temp dir");
        let binding = test_inventory_binding_for_test(temp.path());

        assert!(prepare_test_inventory(&binding).is_ok());
        assert_eq!(
            test_run_status_with_inventory(true, None, false, true, None),
            "failed",
            "requesting inventory mode without evidence must remain unmeasured"
        );

        std::fs::write(&binding.child_path, "not json").expect("write malformed inventory");
        assert!(
            valid_test_inventory(&binding).is_err(),
            "a malformed inventory must remain fail-closed"
        );
        assert!(
            !temp.path().join(TEST_INVENTORY_PUBLIC_FILE).exists(),
            "malformed evidence must never publish a root inventory"
        );

        std::fs::write(
            &binding.child_path,
            valid_inventory_document(&binding, "test", "executed"),
        )
        .expect("write inventory");
        let measured = valid_test_inventory(&binding);
        let (evidence, _) = measured.as_ref().expect("validated inventory");
        assert_eq!(evidence.schema, TEST_INVENTORY_SCHEMA);
        assert_eq!(evidence.test_count, 1);
        assert_eq!(
            test_run_status_with_inventory(true, None, false, true, Some(evidence)),
            "passed"
        );

        let mut widened: TestInventoryEvidence =
            serde_json::from_str(&valid_inventory_document(&binding, "test", "executed"))
                .expect("parse valid inventory");
        widened.fallback_reason =
            Some("Changed test selection no longer matches fixture::lib::fixture.".to_string());
        std::fs::write(
            &binding.child_path,
            serde_json::to_vec(&widened).expect("serialize widened inventory"),
        )
        .expect("write widened inventory");
        assert_eq!(
            valid_test_inventory(&binding)
                .expect("widened inventory remains valid")
                .0
                .fallback_reason
                .as_deref(),
            Some("Changed test selection no longer matches fixture::lib::fixture.")
        );
        assert_eq!(
            test_run_status_with_inventory(false, None, false, true, Some(evidence)),
            "failed",
            "inventory evidence cannot override a failed runner"
        );
        assert_eq!(
            test_run_status_with_inventory(
                true,
                Some(&TestCounts::new(3, 3, 0, 0)),
                false,
                true,
                None
            ),
            "failed",
            "inventory mode cannot fall back to normal execution counts"
        );
        assert_eq!(
            test_run_status_with_inventory(
                true,
                Some(&TestCounts::new(3, 3, 0, 0)),
                false,
                false,
                None,
            ),
            "passed",
            "normal mode must retain its execution-count finalization"
        );

        for (case, document) in [
            (
                "wrong schema",
                valid_inventory_document(&binding, "test", "executed").replacen(
                    TEST_INVENTORY_SCHEMA,
                    "other/schema/v1",
                    1,
                ),
            ),
            (
                "incomplete document",
                "{\"schema\":\"homeboy/test-inventory/v1\"}".to_string(),
            ),
            (
                "arbitrary provenance",
                valid_inventory_document(&binding, "test", "executed").replacen(
                    binding
                        .runner_fingerprints
                        .get("nextest")
                        .map(String::as_str)
                        .expect("nextest fingerprint"),
                    &"c".repeat(64),
                    1,
                ),
            ),
            (
                "stale workspace provenance",
                valid_inventory_document(&binding, "test", "executed").replacen(
                    &binding.workspace_fingerprint,
                    &"d".repeat(64),
                    1,
                ),
            ),
            ("empty inventory", inventory_document(&binding, Vec::new())),
            (
                "all skipped inventory",
                valid_inventory_document(&binding, "test", "skipped"),
            ),
        ] {
            std::fs::write(&binding.child_path, document).expect("write invalid inventory");
            assert!(
                valid_test_inventory(&binding).is_err(),
                "{case} must remain fail-closed"
            );
        }

        let mut duplicate: TestInventoryEvidence =
            serde_json::from_str(&valid_inventory_document(&binding, "test", "executed"))
                .expect("parse valid inventory");
        duplicate.tests.push(duplicate.tests[0].clone());
        duplicate.inventory_fingerprint = homeboy_engine_primitives::content_hash::sha256_hex(
            &canonical_inventory_json(&duplicate),
        );
        std::fs::write(
            &binding.child_path,
            serde_json::to_vec(&duplicate).expect("serialize duplicate inventory"),
        )
        .expect("write duplicate inventory");
        assert!(
            valid_test_inventory(&binding).is_err(),
            "duplicate identities must remain fail-closed even with a matching fingerprint"
        );

        let missing_outcome = TestInventoryTest {
            id: "suite::missing-outcome".to_string(),
            package: "suite".to_string(),
            target: "suite-tests".to_string(),
            target_kind: "test".to_string(),
            name: "missing-outcome".to_string(),
            expected_outcome: None,
        };
        std::fs::write(
            &binding.child_path,
            inventory_document(&binding, vec![missing_outcome]),
        )
        .expect("write missing outcome inventory");
        assert!(
            valid_test_inventory(&binding).is_err(),
            "every inventory identity must declare its expected outcome"
        );
    }

    #[cfg(unix)]
    #[test]
    fn inventory_rejections_are_typed_and_wordpress_shaped_evidence_is_accepted() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut binding = test_inventory_binding_for_test(temp.path());
        binding
            .runner_fingerprints
            .insert("wordpress".to_string(), "d".repeat(64));
        let mut inventory = TestInventoryEvidence {
            schema: TEST_INVENTORY_SCHEMA.to_string(),
            runner: "wordpress".to_string(),
            runner_fingerprint: "d".repeat(64),
            workspace_fingerprint: binding.workspace_fingerprint.clone(),
            tests: vec![TestInventoryTest {
                id: "tests/Unit/Abilities/AgentAbilitiesTest.php".to_string(),
                package: "data-machine".to_string(),
                target: "phpunit".to_string(),
                target_kind: "test".to_string(),
                name: "AgentAbilitiesTest.php".to_string(),
                expected_outcome: Some("executed".to_string()),
            }],
            inventory_fingerprint: String::new(),
            fallback_reason: None,
        };
        inventory.inventory_fingerprint = homeboy_engine_primitives::content_hash::sha256_hex(
            &canonical_inventory_json(&inventory),
        );
        assert!(valid_test_inventory_payload(&inventory, &binding).is_ok());

        inventory.runner_fingerprint = "e".repeat(64);
        assert!(matches!(
            valid_test_inventory_payload(&inventory, &binding),
            Err(TestInventoryRejection::RunnerFingerprintMismatch)
        ));
        inventory.runner_fingerprint = "d".repeat(64);
        inventory.workspace_fingerprint = "e".repeat(64);
        assert!(matches!(
            valid_test_inventory_payload(&inventory, &binding),
            Err(TestInventoryRejection::WorkspaceFingerprintMismatch)
        ));
        inventory.workspace_fingerprint = binding.workspace_fingerprint.clone();
        inventory.inventory_fingerprint = "e".repeat(64);
        assert!(matches!(
            valid_test_inventory_payload(&inventory, &binding),
            Err(TestInventoryRejection::InventoryFingerprintMismatch)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn inventory_file_schema_and_test_entry_rejections_are_typed() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("temp dir");
        let binding = test_inventory_binding_for_test(temp.path());
        assert!(matches!(
            valid_test_inventory(&binding),
            Err(TestInventoryRejection::ChildFileMissing)
        ));

        let target = temp.path().join("inventory-target.json");
        std::fs::write(&target, "{}").expect("write target");
        symlink(&target, &binding.child_path).expect("link evidence");
        assert!(
            matches!(
                valid_test_inventory(&binding),
                Err(TestInventoryRejection::ChildFileUnsafe)
            ),
            "O_NOFOLLOW must never read symlink evidence"
        );

        std::fs::File::create(&binding.child_path)
            .expect("create oversized evidence")
            .set_len(MAX_TEST_INVENTORY_BYTES + 1)
            .expect("size oversized evidence");
        assert!(matches!(
            valid_test_inventory(&binding),
            Err(TestInventoryRejection::ChildFileOversized)
        ));

        std::fs::write(&binding.child_path, "not json").expect("write invalid JSON");
        assert!(matches!(
            valid_test_inventory(&binding),
            Err(TestInventoryRejection::InvalidJson)
        ));

        let mut inventory: TestInventoryEvidence =
            serde_json::from_str(&valid_inventory_document(&binding, "test", "executed"))
                .expect("parse valid inventory");
        inventory.schema = "other/schema/v1".to_string();
        std::fs::write(
            &binding.child_path,
            serde_json::to_vec(&inventory).expect("serialize wrong schema"),
        )
        .expect("write wrong schema");
        assert!(matches!(
            valid_test_inventory(&binding),
            Err(TestInventoryRejection::InvalidSchema)
        ));

        let mut inventory: TestInventoryEvidence =
            serde_json::from_str(&valid_inventory_document(&binding, "test", "executed"))
                .expect("parse valid inventory");
        inventory.tests.push(inventory.tests[0].clone());
        inventory.inventory_fingerprint = homeboy_engine_primitives::content_hash::sha256_hex(
            &canonical_inventory_json(&inventory),
        );
        std::fs::write(
            &binding.child_path,
            serde_json::to_vec(&inventory).expect("serialize duplicate inventory"),
        )
        .expect("write duplicate inventory");
        assert!(matches!(
            valid_test_inventory(&binding),
            Err(TestInventoryRejection::InvalidTests)
        ));

        std::fs::create_dir(&binding.child_path).expect("create stale evidence directory");
        assert!(
            matches!(
                prepare_test_inventory(&binding),
                Err(TestInventoryRejection::PreparationFailed)
            ),
            "unlinkat must reject a directory at the child evidence path"
        );
    }

    #[cfg(unix)]
    #[test]
    fn inventory_open_errors_preserve_missing_unreadable_and_unsafe_categories() {
        assert_eq!(
            classify_test_inventory_open_error(&std::io::Error::from_raw_os_error(libc::ENOENT)),
            TestInventoryRejection::ChildFileMissing
        );
        assert_eq!(
            classify_test_inventory_open_error(&std::io::Error::from_raw_os_error(libc::EACCES)),
            TestInventoryRejection::ChildFileUnreadable
        );
        assert_eq!(
            classify_test_inventory_open_error(&std::io::Error::from_raw_os_error(libc::ELOOP)),
            TestInventoryRejection::ChildFileUnsafe
        );
    }

    #[cfg(unix)]
    #[test]
    fn inventory_binding_rejects_an_unfixed_requested_output_path() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let source = tempfile::tempdir().expect("source workspace");
            std::fs::write(source.path().join("composer.json"), "{}").expect("workspace marker");
            let profile = InventoryProfile {
                root_markers: vec!["composer.json".to_string()],
                fingerprint_names: vec!["composer.json".to_string()],
                fingerprint_extensions: Vec::new(),
                fingerprint_skip_dirs: Vec::new(),
                runner_commands: BTreeMap::from([(
                    "fixture".to_string(),
                    vec!["php".to_string(), "--version".to_string()],
                )]),
            };
            let run_dir = RunDir::create().expect("run directory");

            assert!(matches!(
                test_inventory_binding(&[], source.path(), &run_dir, &profile, true),
                Err(TestInventoryRejection::BindingUnavailable)
            ));

            assert!(matches!(
                test_inventory_binding(
                    &[(
                        TEST_INVENTORY_FILE_ENV.to_string(),
                        "nested/inventory.json".to_string(),
                    )],
                    source.path(),
                    &run_dir,
                    &profile,
                    true,
                ),
                Err(TestInventoryRejection::RequestedPathRejected)
            ));
        });
    }

    #[cfg(unix)]
    #[test]
    fn inventory_mode_runs_the_producer_for_an_empty_changed_scope_and_fails_without_evidence() {
        homeboy_core::test_support::with_isolated_home(|home| {
            use std::os::unix::fs::PermissionsExt;

            let source = tempfile::tempdir().expect("source workspace");
            std::fs::write(
                source.path().join("Cargo.toml"),
                "[package]\nname = \"inventory-zero-scope\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )
            .expect("workspace manifest");
            std::fs::create_dir(source.path().join("src")).expect("source directory");
            std::fs::write(source.path().join("src/lib.rs"), "pub fn fixture() {}\n")
                .expect("source file");
            let marker = source.path().join("inventory-producer-ran");
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions/inventory-zero-scope-fixture");
            std::fs::create_dir_all(&extension_dir).expect("extension directory");
            std::fs::write(
                extension_dir.join("inventory-zero-scope-fixture.json"),
                r#"{"name":"Inventory zero scope fixture","version":"1.0.0","test":{"extension_script":"test.sh"}}"#,
            )
            .expect("extension manifest");
            let script = extension_dir.join("test.sh");
            std::fs::write(
                &script,
                "#!/bin/sh\nset -e\n[ \"$HOMEBOY_TEST_INVENTORY_ONLY\" = 1 ]\n[ -n \"$HOMEBOY_TEST_INVENTORY_FILE\" ]\n[ -z \"${SCOPE_MODE+x}\" ]\n[ -z \"${HOMEBOY_CHANGED_SINCE+x}\" ]\n[ -z \"${HOMEBOY_CHANGED_TEST_FILES+x}\" ]\n[ ! -e \"$HOMEBOY_COMPONENT_PATH/homeboy-test-inventory.json\" ]\ntouch \"$HOMEBOY_COMPONENT_PATH/inventory-producer-ran\"\n",
            )
            .expect("producer script");
            let mut permissions = std::fs::metadata(&script)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).expect("executable script");

            let component = Component {
                id: "inventory-zero-scope".to_string(),
                local_path: source.path().to_string_lossy().to_string(),
                extensions: Some(HashMap::from([(
                    "inventory-zero-scope-fixture".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Default::default()
            };
            let run_dir = RunDir::create().expect("run directory");
            let mut args = fixture_workflow_args(&component);
            args.changed_since = Some("base".to_string());
            args.precomputed_changed_files = Some(vec!["README.md".to_string()]);
            args.ci_env = vec![
                (TEST_INVENTORY_ONLY_ENV.to_string(), "1".to_string()),
                (
                    TEST_INVENTORY_FILE_ENV.to_string(),
                    TEST_INVENTORY_PUBLIC_FILE.to_string(),
                ),
            ];

            let result = run_main_test_workflow(&component, source.path(), args, &run_dir)
                .expect("missing producer output is a test result");

            assert!(
                marker.exists(),
                "inventory producer must run despite zero selected tests"
            );
            assert_eq!(result.status, "failed");
            assert_eq!(result.exit_code, 1);
            assert_eq!(result.runner_exit_code, Some(0));
            assert!(result.test_inventory.is_none());
            assert_eq!(
                result.test_inventory_rejection,
                Some(TestInventoryRejection::ChildFileMissing)
            );
            assert!(
                !source.path().join(TEST_INVENTORY_PUBLIC_FILE).exists(),
                "a producer that emits no valid inventory must not publish output"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn inventory_mode_publishes_valid_producer_output_for_an_empty_changed_scope() {
        homeboy_core::test_support::with_isolated_home(|home| {
            use std::os::unix::fs::PermissionsExt;

            let source = tempfile::tempdir().expect("source workspace");
            std::fs::write(
                source.path().join("Cargo.toml"),
                "[package]\nname = \"inventory-zero-scope-valid\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )
            .expect("workspace manifest");
            std::fs::create_dir(source.path().join("src")).expect("source directory");
            std::fs::write(source.path().join("src/lib.rs"), "pub fn fixture() {}\n")
                .expect("source file");
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions/inventory-valid-fixture");
            std::fs::create_dir_all(&extension_dir).expect("extension directory");
            std::fs::write(
                extension_dir.join("inventory-valid-fixture.json"),
                r#"{"name":"Inventory valid fixture","version":"1.0.0","test":{"extension_script":"test.py"}}"#,
            )
            .expect("extension manifest");
            let script = extension_dir.join("test.py");
            std::fs::write(
                &script,
                r##"#!/usr/bin/env python3
import hashlib
import json
import os
import subprocess
from pathlib import Path

assert os.environ["HOMEBOY_TEST_INVENTORY_ONLY"] == "1"
assert "SCOPE_MODE" not in os.environ
assert "HOMEBOY_CHANGED_SINCE" not in os.environ
assert "HOMEBOY_CHANGED_TEST_FILES" not in os.environ
root = Path(os.environ["HOMEBOY_COMPONENT_PATH"]).resolve()
files = sorted(
    path for path in root.rglob("*")
    if path.is_file()
    and ".git" not in path.parts
    and "target" not in path.parts
    and (path.name in {"Cargo.toml", "Cargo.lock"} or path.suffix == ".rs")
)
workspace = hashlib.sha256(
    "".join(f"{path.relative_to(root)}\0{path.read_text()}\0" for path in files).encode()
).hexdigest()
version = subprocess.check_output(["cargo", "--version"], cwd=root, text=True).strip()
runner = hashlib.sha256(f"cargo\0{version}".encode()).hexdigest()
inventory = {
    "schema": "homeboy/test-inventory/v1",
    "runner": "cargo",
    "runner_fingerprint": runner,
    "workspace_fingerprint": workspace,
    "tests": [{
        "id": "fixture::inventory",
        "package": "fixture",
        "target": "fixture-tests",
        "target_kind": "test",
        "name": "inventory",
        "expected_outcome": "executed",
    }],
}
inventory["inventory_fingerprint"] = hashlib.sha256(
    json.dumps(inventory, sort_keys=True, separators=(",", ":")).encode()
).hexdigest()
Path(os.environ["HOMEBOY_TEST_INVENTORY_FILE"]).write_text(json.dumps(inventory))
"##,
            )
            .expect("producer script");
            let mut permissions = std::fs::metadata(&script)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).expect("executable script");

            let component = Component {
                id: "inventory-valid".to_string(),
                local_path: source.path().to_string_lossy().to_string(),
                extensions: Some(HashMap::from([(
                    "inventory-valid-fixture".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Default::default()
            };
            let run_dir = RunDir::create().expect("run directory");
            let mut args = fixture_workflow_args(&component);
            args.changed_since = Some("base".to_string());
            args.precomputed_changed_files = Some(vec!["README.md".to_string()]);
            args.ci_env = vec![
                (TEST_INVENTORY_ONLY_ENV.to_string(), "1".to_string()),
                (
                    TEST_INVENTORY_FILE_ENV.to_string(),
                    TEST_INVENTORY_PUBLIC_FILE.to_string(),
                ),
            ];

            let result = run_main_test_workflow(&component, source.path(), args, &run_dir)
                .expect("valid producer output");
            let published = source.path().join(TEST_INVENTORY_PUBLIC_FILE);

            assert_eq!(result.status, "passed");
            assert_eq!(result.exit_code, 0);
            assert_eq!(result.runner_exit_code, Some(0));
            assert_eq!(
                result
                    .test_inventory
                    .as_ref()
                    .map(|inventory| inventory.test_count),
                Some(1)
            );
            assert!(
                published.is_file(),
                "validated inventory must publish to the requested path"
            );
            assert_eq!(
                serde_json::from_slice::<TestInventoryEvidence>(
                    &std::fs::read(&published).expect("published inventory")
                )
                .expect("published inventory JSON")
                .inventory_fingerprint,
                result
                    .test_inventory
                    .as_ref()
                    .expect("workflow inventory")
                    .inventory_fingerprint
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn non_inventory_mode_keeps_the_empty_changed_scope_fast_path() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source = tempfile::tempdir().expect("source workspace");
            let marker = source.path().join("runner-ran");
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions/zero-scope-fast-path-fixture");
            std::fs::create_dir_all(&extension_dir).expect("extension directory");
            std::fs::write(
                extension_dir.join("zero-scope-fast-path-fixture.json"),
                r#"{"name":"Zero scope fast path fixture","version":"1.0.0","test":{"extension_script":"test.sh"}}"#,
            )
            .expect("extension manifest");
            std::fs::write(
                extension_dir.join("test.sh"),
                "#!/bin/sh\ntouch \"$HOMEBOY_COMPONENT_PATH/runner-ran\"\n",
            )
            .expect("runner script");
            let component = Component {
                id: "zero-scope-fast-path".to_string(),
                local_path: source.path().to_string_lossy().to_string(),
                extensions: Some(HashMap::from([(
                    "zero-scope-fast-path-fixture".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Default::default()
            };
            let mut args = fixture_workflow_args(&component);
            args.changed_since = Some("base".to_string());
            args.precomputed_changed_files = Some(vec!["README.md".to_string()]);

            let result = run_main_test_workflow(
                &component,
                source.path(),
                args,
                &RunDir::create().expect("run directory"),
            )
            .expect("zero scope fast path");

            assert_eq!(result.status, "passed");
            assert_eq!(result.exit_code, 0);
            assert_eq!(result.runner_exit_code, None);
            assert!(
                !marker.exists(),
                "normal zero scope must not invoke the runner"
            );
        });
    }

    /// Cap tests mutate the process-global HOMEBOY_MAX_CHANGED_TEST_FILES env
    /// var, so they serialize against each other (#12365).
    ///
    /// Poisoning is absorbed rather than propagated: a genuine assertion
    /// failure in one cap test would otherwise poison the lock and convert
    /// every sibling into a misleading `PoisonError` panic, hiding the one
    /// real failure behind a cascade.
    static CHANGED_SCOPE_CAP_ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn changed_scope_cap_env_guard() -> std::sync::MutexGuard<'static, ()> {
        CHANGED_SCOPE_CAP_ENV_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Sets HOMEBOY_MAX_CHANGED_TEST_FILES for a scope and always clears it on
    /// drop, so a panicking assertion cannot leak the cap into another test.
    struct ChangedScopeCapEnv;

    impl ChangedScopeCapEnv {
        fn set(value: &str) -> Self {
            std::env::set_var(MAX_CHANGED_TEST_FILES_ENV, value);
            Self
        }

        fn unset() -> Self {
            std::env::remove_var(MAX_CHANGED_TEST_FILES_ENV);
            Self
        }
    }

    impl Drop for ChangedScopeCapEnv {
        fn drop(&mut self) {
            std::env::remove_var(MAX_CHANGED_TEST_FILES_ENV);
        }
    }

    /// A fixture extension whose runner touches a marker in the component path
    /// so tests can assert whether the extension was invoked at all.
    ///
    /// The source workspace is its own git repository. `component_relative_changed_files`
    /// strips the component's prefix within its enclosing repo, and the test
    /// tempdir lives under `target/.test-tmp` — inside this very repository — so
    /// a non-git fixture resolves a prefix of `target/.test-tmp/run.X/.tmpY`,
    /// strips it from every changed path, and silently yields an empty
    /// selection. That made an earlier revision of these tests vacuous: they
    /// exercised the empty-scope early return, never the cap. `git init` makes
    /// the component its own repo root, where the prefix is correctly `None`.
    #[cfg(unix)]
    fn changed_scope_cap_component(home: &Path, source: &Path) -> Component {
        use std::os::unix::fs::PermissionsExt;

        run_git(source, &["init", "-q", "--initial-branch", "main"]);
        run_git(source, &["config", "user.email", "homeboy@example.com"]);
        run_git(source, &["config", "user.name", "Homeboy Test"]);

        let extension_dir = home.join(".config/homeboy/extensions/changed-scope-cap-fixture");
        std::fs::create_dir_all(&extension_dir).expect("extension directory");
        std::fs::write(
            extension_dir.join("changed-scope-cap-fixture.json"),
            r#"{"name":"Changed scope cap fixture","version":"1.0.0","test":{"extension_script":"test.sh"}}"#,
        )
        .expect("extension manifest");
        let script = extension_dir.join("test.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\ntouch \"$HOMEBOY_COMPONENT_PATH/runner-ran\"\n",
        )
        .expect("runner script");
        let mut permissions = std::fs::metadata(&script)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(script, permissions).expect("executable script");

        Component {
            id: "changed-scope-cap".to_string(),
            local_path: source.to_string_lossy().to_string(),
            extensions: Some(HashMap::from([(
                "changed-scope-cap-fixture".to_string(),
                ScopedExtensionConfig::default(),
            )])),
            ..Default::default()
        }
    }

    fn changed_scope_cap_args(
        component: &Component,
        changed_files: &[String],
    ) -> TestRunWorkflowArgs {
        let mut args = fixture_workflow_args(component);
        args.changed_since = Some("base".to_string());
        args.precomputed_changed_files = Some(changed_files.to_vec());
        args
    }

    /// A changed-scope selection larger than HOMEBOY_MAX_CHANGED_TEST_FILES
    /// fails fast with a named finding, preserves the full selection, and must
    /// not invoke the extension runner (#12365).
    #[cfg(unix)]
    #[test]
    fn changed_scope_selection_exceeding_the_cap_fails_before_the_runner_spawns() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let _guard = changed_scope_cap_env_guard();
            let source = tempfile::tempdir().expect("source workspace");
            let component = changed_scope_cap_component(home.path(), source.path());
            let changed_files = (0..5)
                .map(|index| format!("tests/component_{index}.php"))
                .collect::<Vec<_>>();

            let _cap = ChangedScopeCapEnv::set("3");
            let result = run_main_test_workflow(
                &component,
                source.path(),
                changed_scope_cap_args(&component, &changed_files),
                &RunDir::create().expect("run directory"),
            )
            .expect("cap failure is a test result");

            assert_eq!(result.status, "failed");
            assert_eq!(result.exit_code, 1);
            assert_eq!(
                result.runner_exit_code, None,
                "the guard must not invoke the runner"
            );
            assert!(
                !source.path().join("runner-ran").exists(),
                "the guard must not invoke the extension"
            );
            let findings = result.findings.expect("cap finding");
            assert_eq!(findings.len(), 1);
            assert_eq!(
                findings[0].rule.as_deref(),
                Some("changed_scope_selection_exceeds_cap")
            );
            assert_eq!(findings[0].category.as_deref(), Some("test-scope"));
            assert_eq!(findings[0].severity.as_deref(), Some("error"));
            assert!(
                findings[0].message.contains("5"),
                "message names the selected count: {}",
                findings[0].message
            );
            assert!(
                findings[0].message.contains("3"),
                "message names the cap: {}",
                findings[0].message
            );
            let scope = result.test_scope.expect("full selection preserved");
            assert_eq!(scope.selected_files, changed_files);
            assert_eq!(scope.selected_count, 5);
        });
    }

    /// A selection at or below the cap must take the normal execution path:
    /// the extension runner runs and no cap finding is emitted (#12365).
    #[cfg(unix)]
    #[test]
    fn changed_scope_selection_at_or_below_the_cap_runs_normally() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let _guard = changed_scope_cap_env_guard();
            let source = tempfile::tempdir().expect("source workspace");
            let component = changed_scope_cap_component(home.path(), source.path());
            let changed_files = vec![
                "tests/component_a.php".to_string(),
                "tests/component_b.php".to_string(),
            ];

            let _cap = ChangedScopeCapEnv::set("2");
            let result = run_main_test_workflow(
                &component,
                source.path(),
                changed_scope_cap_args(&component, &changed_files),
                &RunDir::create().expect("run directory"),
            )
            .expect("at-cap run executes");

            assert_eq!(result.runner_exit_code, Some(0));
            assert!(
                source.path().join("runner-ran").exists(),
                "a selection that fits the cap must invoke the runner"
            );
            assert!(
                result.findings.as_ref().is_none_or(|findings| {
                    !findings.iter().any(|finding| {
                        finding.rule.as_deref() == Some("changed_scope_selection_exceeds_cap")
                    })
                }),
                "an at-cap selection must not emit the cap finding"
            );
            let scope = result.test_scope.expect("scope retained");
            assert_eq!(scope.selected_files, changed_files);
        });
    }

    /// An unset cap must leave the unchanged selection behaviour byte-for-byte
    /// identical: a large selection still runs the extension (#12365).
    #[cfg(unix)]
    #[test]
    fn unset_changed_scope_cap_leaves_selection_behaviour_unchanged() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let _guard = changed_scope_cap_env_guard();
            let source = tempfile::tempdir().expect("source workspace");
            let component = changed_scope_cap_component(home.path(), source.path());
            let changed_files = (0..5)
                .map(|index| format!("tests/component_{index}.php"))
                .collect::<Vec<_>>();

            let _cap = ChangedScopeCapEnv::unset();
            let result = run_main_test_workflow(
                &component,
                source.path(),
                changed_scope_cap_args(&component, &changed_files),
                &RunDir::create().expect("run directory"),
            )
            .expect("unset cap runs the runner");

            assert_eq!(result.runner_exit_code, Some(0));
            assert!(
                source.path().join("runner-ran").exists(),
                "an unset cap must not intercept a large selection"
            );
            assert!(
                result.findings.as_ref().is_none_or(|findings| {
                    !findings.iter().any(|finding| {
                        finding.rule.as_deref() == Some("changed_scope_selection_exceeds_cap")
                    })
                }),
                "an unset cap must not emit the cap finding"
            );
            // Pins the fixture itself: an empty selection would route this run
            // through the no-tests-in-scope early return and make the assertions
            // above vacuous rather than false.
            let scope = result.test_scope.expect("scope retained");
            assert_eq!(scope.selected_files, changed_files);
            assert_eq!(scope.selected_count, 5);
        });
    }

    /// A malformed or non-positive cap value disables the guard rather than
    /// failing the run, mirroring HOMEBOY_TEST_TIMEOUT_SECONDS parsing (#12365).
    #[cfg(unix)]
    #[test]
    fn malformed_or_non_positive_changed_scope_cap_values_are_ignored() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let _guard = changed_scope_cap_env_guard();
            let source = tempfile::tempdir().expect("source workspace");
            let component = changed_scope_cap_component(home.path(), source.path());
            let changed_files = (0..5)
                .map(|index| format!("tests/component_{index}.php"))
                .collect::<Vec<_>>();
            let marker = source.path().join("runner-ran");

            for value in ["not-a-number", "0", "-5", ""] {
                std::fs::remove_file(&marker).ok();
                let _cap = ChangedScopeCapEnv::set(value);
                let result = run_main_test_workflow(
                    &component,
                    source.path(),
                    changed_scope_cap_args(&component, &changed_files),
                    &RunDir::create().expect("run directory"),
                )
                .expect("ignored cap value still runs the runner");

                assert_eq!(
                    result.runner_exit_code,
                    Some(0),
                    "cap value {value:?} must be ignored"
                );
                assert!(
                    marker.exists(),
                    "cap value {value:?} must not block execution"
                );
                assert!(
                    result.findings.as_ref().is_none_or(|findings| {
                        !findings.iter().any(|finding| {
                            finding.rule.as_deref() == Some("changed_scope_selection_exceeds_cap")
                        })
                    }),
                    "cap value {value:?} must not emit the cap finding"
                );
                let scope = result.test_scope.expect("scope retained");
                assert_eq!(
                    scope.selected_count, 5,
                    "cap value {value:?} must leave the selection intact"
                );
            }
        });
    }

    /// Inventory mode is a producer operation, not a changed-test execution, so
    /// the changed-scope cap must not fire there even for an oversized selection
    /// (#12365).
    #[cfg(unix)]
    #[test]
    fn inventory_mode_ignores_the_changed_scope_cap() {
        homeboy_core::test_support::with_isolated_home(|home| {
            use std::os::unix::fs::PermissionsExt;

            let source = tempfile::tempdir().expect("source workspace");
            std::fs::write(
                source.path().join("Cargo.toml"),
                "[package]\nname = \"inventory-cap-ignored\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )
            .expect("workspace manifest");
            std::fs::create_dir(source.path().join("src")).expect("source directory");
            std::fs::write(source.path().join("src/lib.rs"), "pub fn fixture() {}\n")
                .expect("source file");
            let marker = source.path().join("inventory-producer-ran");
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions/inventory-cap-ignored-fixture");
            std::fs::create_dir_all(&extension_dir).expect("extension directory");
            std::fs::write(
                extension_dir.join("inventory-cap-ignored-fixture.json"),
                r#"{"name":"Inventory cap ignored fixture","version":"1.0.0","test":{"extension_script":"test.sh"}}"#,
            )
            .expect("extension manifest");
            let script = extension_dir.join("test.sh");
            std::fs::write(
                &script,
                "#!/bin/sh\nset -e\n[ \"$HOMEBOY_TEST_INVENTORY_ONLY\" = 1 ]\n[ -z \"${HOMEBOY_CHANGED_TEST_FILES+x}\" ]\ntouch \"$HOMEBOY_COMPONENT_PATH/inventory-producer-ran\"\n",
            )
            .expect("producer script");
            let mut permissions = std::fs::metadata(&script)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).expect("executable script");

            let component = Component {
                id: "inventory-cap-ignored".to_string(),
                local_path: source.path().to_string_lossy().to_string(),
                extensions: Some(HashMap::from([(
                    "inventory-cap-ignored-fixture".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Default::default()
            };
            let mut args = fixture_workflow_args(&component);
            args.changed_since = Some("base".to_string());
            args.precomputed_changed_files = Some(vec![
                "tests/alpha.php".to_string(),
                "tests/beta.php".to_string(),
                "tests/gamma.php".to_string(),
            ]);
            args.ci_env = vec![
                (TEST_INVENTORY_ONLY_ENV.to_string(), "1".to_string()),
                (
                    TEST_INVENTORY_FILE_ENV.to_string(),
                    TEST_INVENTORY_PUBLIC_FILE.to_string(),
                ),
            ];

            let _guard = changed_scope_cap_env_guard();
            let _cap = ChangedScopeCapEnv::set("1");
            let result = run_main_test_workflow(
                &component,
                source.path(),
                args,
                &RunDir::create().expect("run directory"),
            )
            .expect("inventory producer runs");

            assert!(
                marker.exists(),
                "the inventory producer must run despite a selection larger than the cap"
            );
            assert_eq!(result.status, "failed", "no valid inventory evidence");
            assert_eq!(result.exit_code, 1);
            assert_eq!(result.runner_exit_code, Some(0));
            assert!(
                result.findings.as_ref().is_none_or(|findings| {
                    !findings.iter().any(|finding| {
                        finding.rule.as_deref() == Some("changed_scope_selection_exceeds_cap")
                    })
                }),
                "the cap must not fire in inventory mode"
            );
        });
    }

    /// An evidence failure has to name its own defect.
    ///
    /// The internal runtime producer path collapsed every `TestInventoryRejection`
    /// into one sentence — "emitted invalid evidence" — so the CI sidecar, the
    /// differential gate, and two full rerun cycles on
    /// `Extra-Chill/extrachill-users#377` all reported that the evidence was
    /// invalid and none of them could say which check failed (#13494). The typed
    /// rejection already exists; the only thing missing was carrying it.
    #[cfg(unix)]
    #[test]
    fn internal_runtime_inventory_rejection_names_the_failing_check() {
        homeboy_core::test_support::with_isolated_home(|home| {
            use std::os::unix::fs::PermissionsExt;

            let source = tempfile::tempdir().expect("source workspace");
            std::fs::write(
                source.path().join("Cargo.toml"),
                "[package]\nname = \"inventory-rejection-reason\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )
            .expect("workspace manifest");
            std::fs::create_dir(source.path().join("src")).expect("source directory");
            std::fs::write(source.path().join("src/lib.rs"), "pub fn fixture() {}\n")
                .expect("source file");
            let extension_dir = home
                .path()
                .join(".config/homeboy/extensions/inventory-rejection-fixture");
            std::fs::create_dir_all(&extension_dir).expect("extension directory");
            std::fs::write(
                extension_dir.join("inventory-rejection-fixture.json"),
                r#"{"name":"Inventory rejection fixture","version":"1.0.0","test":{"extension_script":"test.py"}}"#,
            )
            .expect("extension manifest");
            let script = extension_dir.join("test.py");
            // A producer whose workspace fingerprint disagrees with the parent's
            // is exactly the shape of the reported defect: the document is
            // well-formed, and only the provenance check can tell you so.
            std::fs::write(
                &script,
                r##"#!/usr/bin/env python3
import hashlib
import json
import os
import subprocess
from pathlib import Path

if os.environ.get("HOMEBOY_TEST_INVENTORY_ONLY") != "1":
    raise SystemExit(0)

root = Path(os.environ["HOMEBOY_COMPONENT_PATH"]).resolve()
version = subprocess.check_output(["cargo", "--version"], cwd=root, text=True).strip()
inventory = {
    "schema": "homeboy/test-inventory/v1",
    "runner": "cargo",
    "runner_fingerprint": hashlib.sha256(f"cargo\0{version}".encode()).hexdigest(),
    "workspace_fingerprint": "b" * 64,
    "tests": [{
        "id": "fixture::inventory",
        "package": "fixture",
        "target": "fixture-tests",
        "target_kind": "test",
        "name": "inventory",
        "expected_outcome": "executed",
    }],
}
inventory["inventory_fingerprint"] = hashlib.sha256(
    json.dumps(inventory, sort_keys=True, separators=(",", ":")).encode()
).hexdigest()
Path(os.environ["HOMEBOY_TEST_INVENTORY_FILE"]).write_text(json.dumps(inventory))
"##,
            )
            .expect("producer script");
            let mut permissions = std::fs::metadata(&script)
                .expect("script metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).expect("executable script");

            let component = Component {
                id: "inventory-rejection".to_string(),
                local_path: source.path().to_string_lossy().to_string(),
                extensions: Some(HashMap::from([(
                    "inventory-rejection-fixture".to_string(),
                    ScopedExtensionConfig::default(),
                )])),
                ..Default::default()
            };
            let result = run_main_test_workflow(
                &component,
                source.path(),
                fixture_workflow_args(&component),
                &RunDir::create().expect("run directory"),
            )
            .expect("internal runtime plan is attempted");

            let reason = match result.test_runtime_evidence {
                Some(TestRuntimeEvidence::InvalidEvidence { reason }) => reason,
                other => panic!("expected invalid runtime evidence, got {other:?}"),
            };
            assert!(
                reason.contains(TestInventoryRejection::WorkspaceFingerprintMismatch.message()),
                "the reason must name the failing provenance check, got: {reason}"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn inventory_evidence_is_fresh_confined_and_fingerprint_bound() {
        let temp = tempfile::tempdir().expect("temp dir");
        let binding = test_inventory_binding_for_test(temp.path());

        std::fs::write(
            &binding.child_path,
            valid_inventory_document(&binding, "test", "executed"),
        )
        .expect("write stale inventory");
        assert!(prepare_test_inventory(&binding).is_ok());
        assert!(
            !binding.child_path.exists(),
            "pre-existing evidence must not satisfy a new invocation"
        );

        std::fs::write(
            &binding.child_path,
            valid_inventory_document(&binding, "test", "executed"),
        )
        .expect("write inventory");
        assert!(valid_test_inventory(&binding).is_ok());
        std::fs::write(
            &binding.child_path,
            valid_inventory_document(&binding, "tést", "executed"),
        )
        .expect("write unicode inventory");
        assert!(
            valid_test_inventory(&binding).is_ok(),
            "Python's ASCII-escaped fingerprint must accept Unicode test names"
        );
        let tampered = valid_inventory_document(&binding, "test", "executed").replacen(
            "suite::test",
            "suite::other",
            1,
        );
        std::fs::write(&binding.child_path, tampered).expect("write tampered inventory");
        assert!(
            valid_test_inventory(&binding).is_err(),
            "the inventory fingerprint must bind the listed tests"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cargo_inventory_binds_without_cargo_nextest() {
        let temp = tempfile::tempdir().expect("temp dir");
        let mut binding = test_inventory_binding_for_test(temp.path());
        let test = TestInventoryTest {
            id: "suite::cargo".to_string(),
            package: "suite".to_string(),
            target: "suite-tests".to_string(),
            target_kind: "test".to_string(),
            name: "cargo".to_string(),
            expected_outcome: Some("executed".to_string()),
        };
        let mut inventory: TestInventoryEvidence =
            serde_json::from_str(&inventory_document(&binding, vec![test]))
                .expect("parse inventory");
        binding.runner_fingerprints.remove("nextest");
        inventory.runner = "cargo".to_string();
        inventory.runner_fingerprint = binding
            .runner_fingerprints
            .get("cargo")
            .cloned()
            .expect("cargo fingerprint");
        inventory.inventory_fingerprint = homeboy_engine_primitives::content_hash::sha256_hex(
            &canonical_inventory_json(&inventory),
        );
        std::fs::write(
            &binding.child_path,
            serde_json::to_vec(&inventory).expect("serialize cargo inventory"),
        )
        .expect("write cargo inventory");
        assert!(
            valid_test_inventory(&binding).is_ok(),
            "Cargo inventory must not require cargo-nextest"
        );
    }

    /// Golden values produced by `homeboy-extensions/rust/scripts/test-shard-inventory.py`.
    /// Keep these byte-for-byte values aligned with the producer's v1 contract.
    #[cfg(unix)]
    #[test]
    fn inventory_provenance_fingerprints_match_producer_golden_fixture() {
        let root =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/test_inventory_fingerprint");
        assert_eq!(
            workspace_fingerprint(&root, &InventoryProfile::cargo()).expect("fingerprint fixture"),
            "3ff128fc5701066e7fc0324c88cd18ec1bc6b1ea5aa8390b1661da891e106712"
        );
        assert_eq!(
            runner_fingerprint_from_version("cargo", "cargo 1.85.0 (fixture)"),
            "75505895481f59e56262ce8b0cd07ac303f136fca4dc7cfeafd7dd3b1fcfc66a"
        );
        assert_eq!(
            runner_fingerprint_from_version("nextest", "cargo-nextest 0.9.99 (fixture)"),
            "09c443d61494d183c1a8441ca0f568decd4130b51a0a5c3a66c846efc6991f78"
        );
    }

    /// The ordering half of the producer contract, which #13494 was.
    ///
    /// This function is the arbiter, so what it does *is* the contract, and a
    /// producer that sorts the joined path text instead of the components
    /// disagrees with it exactly when a directory name is a prefix of a sibling
    /// followed by a byte below `/` — `auth/` beside `auth-tokens/`. A workspace
    /// shaped like that (`Extra-Chill/extrachill-users`) could never match its
    /// own inventory, so every PR it opened failed the differential test gate
    /// with `invalid_evidence` and no rerun could help.
    ///
    /// The expected order is written out literally rather than derived from a
    /// Python subprocess: `sorted()` over `pathlib.Path` returned string order
    /// before Python 3.12 and component order after it, so a Python oracle would
    /// pin the interpreter on the runner instead of the contract.
    ///
    /// The fixture is asserted to be discriminating: if the two orders ever
    /// agree here, this test proves nothing and says so.
    #[cfg(unix)]
    #[test]
    fn workspace_fingerprint_orders_by_path_components_not_joined_text() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let root = temp.path();
        std::fs::write(
            root.join("Cargo.toml"),
            b"[package]\nname = \"sibling-prefix-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write manifest");
        std::fs::create_dir_all(root.join("src/auth")).expect("create auth directory");
        std::fs::create_dir_all(root.join("src/auth-tokens"))
            .expect("create auth-tokens directory");
        // Distinct bodies: identical contents would hash the same in either
        // order and the fixture would silently stop discriminating.
        std::fs::write(
            root.join("src/auth/handler.rs"),
            b"pub fn handler() -> &'static str {\n    \"auth\"\n}\n",
        )
        .expect("write auth source");
        std::fs::write(
            root.join("src/auth-tokens/token.rs"),
            b"pub fn token() -> &'static str {\n    \"auth-tokens\"\n}\n",
        )
        .expect("write auth-tokens source");

        let digest_in_order = |relatives: &[&str]| {
            let mut content = String::new();
            for relative in relatives {
                content.push_str(relative);
                content.push('\0');
                content
                    .push_str(&std::fs::read_to_string(root.join(relative)).expect("fixture file"));
                content.push('\0');
            }
            homeboy_engine_primitives::content_hash::sha256_hex(content.as_bytes())
        };
        let component_order = [
            "Cargo.toml",
            "src/auth/handler.rs",
            "src/auth-tokens/token.rs",
        ];
        let joined_text_order = [
            "Cargo.toml",
            "src/auth-tokens/token.rs",
            "src/auth/handler.rs",
        ];
        assert_ne!(
            digest_in_order(&component_order),
            digest_in_order(&joined_text_order),
            "fixture no longer discriminates between path-component and joined-text ordering"
        );

        assert_eq!(
            workspace_fingerprint(root, &InventoryProfile::cargo()),
            Some(digest_in_order(&component_order)),
            "the workspace fingerprint must order its selection by path components"
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_fingerprint_matches_python_producer_for_universal_newlines() {
        let temp = tempfile::tempdir().expect("temp workspace");
        let root = temp.path();
        std::fs::create_dir(root.join("src")).expect("create source directory");
        std::fs::write(
            root.join("Cargo.toml"),
            b"[package]\nname = \"newline-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write LF manifest");
        std::fs::write(
            root.join("Cargo.lock"),
            b"# This file is automatically @generated by Cargo.\r\nversion = 4\r\n\r\n[[package]]\r\nname = \"newline-fixture\"\r\nversion = \"0.1.0\"\r\n",
        )
        .expect("write valid CRLF lockfile");
        std::fs::write(
            root.join("src/lib.rs"),
            b"pub fn newline_fixture() -> &'static str {\r    \"lone CR\"\r}\r",
        )
        .expect("write lone-CR source");
        let metadata = Command::new("cargo")
            .args(["metadata", "--locked", "--no-deps", "--format-version=1"])
            .current_dir(root)
            .output()
            .expect("validate fixture lockfile");
        assert!(
            metadata.status.success(),
            "fixture lockfile must be valid: {}",
            String::from_utf8_lossy(&metadata.stderr)
        );

        let python = r#"
import hashlib
import sys
from pathlib import Path

root = Path(sys.argv[1]).resolve()
files = sorted(
    path for path in root.rglob("*")
    if path.is_file()
    and ".git" not in path.parts
    and "target" not in path.parts
    and (path.name in {"Cargo.toml", "Cargo.lock"} or path.suffix == ".rs")
)
content = "".join(f"{path.relative_to(root)}\0{path.read_text()}\0" for path in files)
print(hashlib.sha256(content.encode()).hexdigest())
"#;
        let output = Command::new("python3")
            .args(["-c", python])
            .arg(root)
            .output()
            .expect("run Python inventory producer");
        assert!(
            output.status.success(),
            "Python inventory producer failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let producer_fingerprint = String::from_utf8(output.stdout)
            .expect("Python producer output is UTF-8")
            .trim()
            .to_string();

        assert_eq!(
            producer_fingerprint,
            "3db8dc90de016e27318c257eb17536777a3770e36384f8c9799f300bcbd1abc0",
            "the Python producer golden fingerprint must remain stable"
        );
        assert_eq!(
            workspace_fingerprint(root, &InventoryProfile::cargo()),
            Some(producer_fingerprint),
            "the Rust verifier must match Path.read_text() universal-newline semantics"
        );

        std::fs::write(root.join("src/invalid.rs"), b"\xff").expect("write invalid UTF-8 source");
        let invalid_utf8 = Command::new("python3")
            .args(["-c", python])
            .arg(root)
            .output()
            .expect("run Python inventory producer with invalid UTF-8");
        assert!(
            !invalid_utf8.status.success(),
            "Path.read_text() must reject invalid UTF-8 fingerprint input"
        );
        assert_eq!(
            workspace_fingerprint(root, &InventoryProfile::cargo()),
            None,
            "the Rust verifier must fail closed when the Python producer cannot decode a file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn inventory_evidence_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("temp dir");
        let target = temp.path().join("target.json");
        let binding = test_inventory_binding_for_test(temp.path());
        std::fs::write(
            &target,
            valid_inventory_document(&binding, "test", "executed"),
        )
        .expect("write target");
        symlink(&target, &binding.child_path).expect("link inventory");

        assert!(valid_test_inventory(&binding).is_err());
        assert!(prepare_test_inventory(&binding).is_ok());
        assert!(target.exists(), "cleanup must remove only the link");
    }

    #[cfg(unix)]
    #[test]
    fn inventory_evidence_uses_held_directory_descriptor_across_rename_symlink_race() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("temp dir");
        let run_dir = temp.path().join("run");
        std::fs::create_dir(&run_dir).expect("create run dir");
        let binding = test_inventory_binding_for_test(&run_dir);
        let outside = tempfile::tempdir().expect("outside dir");
        let moved = temp.path().join("run-renamed");
        std::fs::rename(&run_dir, &moved).expect("rename held run directory");
        symlink(outside.path(), &run_dir).expect("replace run directory with outside link");
        std::fs::write(
            moved.join(TEST_INVENTORY_FILE),
            valid_inventory_document(&binding, "held", "executed"),
        )
        .expect("write evidence into held directory");
        let outside_inventory = outside.path().join(TEST_INVENTORY_FILE);
        std::fs::write(&outside_inventory, "outside evidence must survive")
            .expect("write outside evidence");

        assert!(valid_test_inventory(&binding).is_ok());
        assert!(
            !moved.join(TEST_INVENTORY_FILE).exists(),
            "the consumed evidence must be unlinked relative to the held descriptor"
        );
        assert_eq!(
            std::fs::read_to_string(&outside_inventory).expect("read outside evidence"),
            "outside evidence must survive",
            "the replacement pathname must neither be read nor unlinked"
        );
    }

    #[cfg(unix)]
    #[test]
    fn inventory_publication_requires_the_fixed_root_output_and_preserves_collisions() {
        use std::os::unix::fs::symlink;

        let workspace = tempfile::tempdir().expect("workspace");
        let run = tempfile::tempdir().expect("run directory");
        let binding = test_inventory_binding_for_test_in(workspace.path(), run.path());
        let output = workspace.path().join(TEST_INVENTORY_PUBLIC_FILE);
        let valid = valid_inventory_document(&binding, "published", "executed").into_bytes();

        assert!(requested_test_inventory_path(
            &[(
                TEST_INVENTORY_FILE_ENV.to_string(),
                output.to_string_lossy().into_owned()
            )],
            workspace.path()
        )
        .is_ok());
        for rejected in [
            tempfile::tempdir()
                .expect("outside")
                .path()
                .join(TEST_INVENTORY_PUBLIC_FILE),
            workspace
                .path()
                .join("nested")
                .join(TEST_INVENTORY_PUBLIC_FILE),
        ] {
            assert!(requested_test_inventory_path(
                &[(
                    TEST_INVENTORY_FILE_ENV.to_string(),
                    rejected.to_string_lossy().into_owned()
                )],
                workspace.path()
            )
            .is_err());
        }

        std::fs::write(&output, "existing regular entry").expect("create collision");
        assert!(!publish_test_inventory(&binding, &valid));
        assert_eq!(
            std::fs::read(&output).expect("read collision"),
            b"existing regular entry"
        );
        std::fs::remove_file(&output).expect("remove test collision");

        let target = tempfile::NamedTempFile::new().expect("symlink target");
        symlink(target.path(), &output).expect("create collision symlink");
        assert!(!publish_test_inventory(&binding, &valid));
        assert!(std::fs::symlink_metadata(&output)
            .expect("inspect symlink")
            .file_type()
            .is_symlink());
        std::fs::remove_file(&output).expect("remove test symlink");

        assert!(publish_test_inventory(&binding, &valid));
        assert_eq!(
            std::fs::read(&output).expect("read published output"),
            valid
        );
    }

    #[cfg(unix)]
    #[test]
    fn inventory_publication_uses_held_project_root_and_cleans_only_its_entry_on_failure() {
        use std::os::unix::fs::symlink;

        let parent = tempfile::tempdir().expect("parent");
        let workspace = parent.path().join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        let run = tempfile::tempdir().expect("run directory");
        let binding = test_inventory_binding_for_test_in(&workspace, run.path());
        let moved = parent.path().join("workspace-moved");
        let outside = tempfile::tempdir().expect("outside");
        std::fs::rename(&workspace, &moved).expect("rename project root");
        symlink(outside.path(), &workspace).expect("replace project root");
        let bytes = valid_inventory_document(&binding, "held", "executed").into_bytes();
        assert!(publish_test_inventory(&binding, &bytes));
        assert_eq!(
            std::fs::read(moved.join(TEST_INVENTORY_PUBLIC_FILE)).expect("read held output"),
            bytes
        );
        assert!(!outside.path().join(TEST_INVENTORY_PUBLIC_FILE).exists());

        let cleanup_root = tempfile::tempdir().expect("cleanup root");
        let cleanup_run = tempfile::tempdir().expect("cleanup run");
        for failure in ["write", "sync", "fstat"] {
            let cleanup =
                test_inventory_binding_for_test_in(cleanup_root.path(), cleanup_run.path());
            let failed = match failure {
                "write" => publish_test_inventory_with(
                    &cleanup,
                    |_| Err(std::io::Error::other("simulated write failure")),
                    |_| Ok(()),
                    |file| file.metadata(),
                ),
                "sync" => publish_test_inventory_with(
                    &cleanup,
                    |_| Ok(()),
                    |_| Err(std::io::Error::other("simulated sync failure")),
                    |file| file.metadata(),
                ),
                "fstat" => publish_test_inventory_with(
                    &cleanup,
                    |_| Ok(()),
                    |_| Ok(()),
                    |_| Err(std::io::Error::other("simulated fstat failure")),
                ),
                _ => unreachable!(),
            };
            assert!(!failed, "{failure} failure must reject publication");
            assert!(
                !cleanup_root
                    .path()
                    .join(TEST_INVENTORY_PUBLIC_FILE)
                    .exists(),
                "{failure} failure must remove only the file this parent created"
            );
        }
    }

    #[cfg(unix)]
    pub(super) fn test_inventory_binding_for_test(run_dir: &Path) -> TestInventoryBinding {
        test_inventory_binding_for_test_in(run_dir, run_dir)
    }

    #[cfg(unix)]
    fn test_inventory_binding_for_test_in(
        project_root: &Path,
        run_dir: &Path,
    ) -> TestInventoryBinding {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        let run_dir = run_dir.canonicalize().expect("canonical run dir");
        let project_root = project_root.canonicalize().expect("canonical project root");
        let descriptor = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(&run_dir)
            .expect("open run directory");
        let metadata = descriptor.metadata().expect("run directory metadata");
        TestInventoryBinding {
            child_path: run_dir.join(TEST_INVENTORY_FILE),
            workspace_fingerprint: "b".repeat(64),
            runner_fingerprints: BTreeMap::from([
                ("cargo".to_string(), "a".repeat(64)),
                ("nextest".to_string(), "a".repeat(64)),
            ]),
            profile: InventoryProfile::cargo(),
            project_root: std::fs::File::open(&project_root).expect("open project root"),
            run_dir: descriptor,
            run_dir_device: metadata.dev(),
            run_dir_inode: metadata.ino(),
        }
    }

    #[cfg(unix)]
    fn valid_inventory_document(
        binding: &TestInventoryBinding,
        name: &str,
        outcome: &str,
    ) -> String {
        let test = TestInventoryTest {
            id: format!("suite::{name}"),
            package: "suite".to_string(),
            target: "suite-tests".to_string(),
            target_kind: "test".to_string(),
            name: name.to_string(),
            expected_outcome: Some(outcome.to_string()),
        };
        inventory_document(binding, vec![test])
    }

    #[cfg(unix)]
    fn inventory_document(binding: &TestInventoryBinding, tests: Vec<TestInventoryTest>) -> String {
        let mut inventory = TestInventoryEvidence {
            schema: TEST_INVENTORY_SCHEMA.to_string(),
            runner: "nextest".to_string(),
            runner_fingerprint: binding
                .runner_fingerprints
                .get("nextest")
                .cloned()
                .expect("nextest fingerprint"),
            workspace_fingerprint: binding.workspace_fingerprint.clone(),
            tests,
            inventory_fingerprint: String::new(),
            fallback_reason: None,
        };
        inventory.inventory_fingerprint = homeboy_engine_primitives::content_hash::sha256_hex(
            &canonical_inventory_json(&inventory),
        );
        serde_json::to_string(&inventory).expect("serialize inventory")
    }

    #[test]
    fn status_fails_all_skipped_tests() {
        assert_eq!(
            test_run_status(true, Some(&TestCounts::new(1, 0, 0, 1)), false),
            "failed"
        );
    }

    /// Recorded no-measurement scenarios, fed through the real status function.
    ///
    /// Assert the effect, not the command string (#10685). The post-merge audit
    /// gate passed for weeks because its test asserted the command it invoked
    /// rather than what that command produced, so every case here is a shape
    /// actually observed in a CI run and every assertion is about the verdict
    /// that came out.
    #[test]
    fn no_recorded_unmeasured_shape_renders_green() {
        let unmeasured: &[(&str, Option<TestCounts>)] = &[
            (
                "child killed before writing its results sidecar (#10639/#10644): counts absent \
                 entirely",
                None,
            ),
            (
                "runner exited 0 having executed nothing: `0 passed; 0 failed`",
                Some(TestCounts::new(0, 0, 0, 0)),
            ),
            (
                "every selected test skipped: the runner started and measured no assertion",
                Some(TestCounts::new(12, 0, 0, 12)),
            ),
            (
                "a total was reported but no assertion resolved -- the shape a truncated \
                 summary parse produces",
                Some(TestCounts::new(412, 0, 0, 0)),
            ),
        ];

        for (scenario, counts) in unmeasured {
            assert_eq!(
                test_run_status(true, counts.as_ref(), false),
                "failed",
                "an unmeasured test phase rendered green: {scenario}"
            );
        }
    }

    /// The migration onto the shared predicate is behaviour-preserving.
    ///
    /// Stated as a property over the whole reachable input space rather than as
    /// a handful of examples, because "identical behaviour" is the entire claim
    /// the refactor rests on and examples cannot support it.
    #[test]
    fn test_run_status_matches_the_shared_predicate() {
        for runner_success in [true, false] {
            for no_tests in [true, false] {
                for counts in [
                    None,
                    Some(TestCounts::new(0, 0, 0, 0)),
                    Some(TestCounts::new(1, 0, 0, 1)),
                    Some(TestCounts::new(3, 3, 0, 0)),
                    Some(TestCounts::new(3, 2, 1, 0)),
                    Some(TestCounts::new(3, 0, 3, 0)),
                    Some(TestCounts::new(9, 4, 0, 5)),
                ] {
                    // The rule as it stood before #10685, verbatim.
                    let legacy = if !runner_success {
                        "failed"
                    } else if no_tests {
                        "skipped"
                    } else if counts.as_ref().is_some_and(|counts| {
                        counts.passed + counts.failed > 0 && counts.failed == 0
                    }) {
                        "passed"
                    } else {
                        "failed"
                    };
                    assert_eq!(
                        test_run_status(runner_success, counts.as_ref(), no_tests),
                        legacy,
                        "shared-predicate migration changed behaviour for \
                         runner_success={runner_success} no_tests={no_tests} counts={counts:?}"
                    );
                }
            }
        }
    }

    /// `skipped` is the one exit from the measurement requirement, and it is
    /// reached only through positive, nonce-bound evidence -- never through the
    /// absence of counts.
    #[test]
    fn only_signed_evidence_can_skip_the_measurement_requirement() {
        assert_eq!(test_run_status(true, None, true), "skipped");
        assert_eq!(
            test_run_status(true, None, false),
            "failed",
            "absent counts must never be read as `nothing to test`"
        );
    }

    #[test]
    fn no_test_policy_requires_bound_structured_evidence() {
        let temp = tempfile::tempdir().expect("temp dir");
        let evidence_file = temp.path().join("no-tests-applicable.json");
        std::fs::write(&evidence_file, r#"{"schema":"homeboy/no-tests-applicable/v1","extension_id":"fixture","step":"test","nonce":"nonce","reason":"docs only"}"#).expect("write evidence");
        assert!(no_tests_applicable(
            true,
            &evidence_file,
            "fixture",
            "nonce",
            None
        ));
        std::fs::write(&evidence_file, r#"{"schema":"homeboy/no-tests-applicable/v1","extension_id":"fixture","step":"test","nonce":"wrong","reason":"docs only"}"#).expect("write wrong nonce");
        assert!(!no_tests_applicable(
            true,
            &evidence_file,
            "fixture",
            "nonce",
            None
        ));
        std::fs::write(&evidence_file, r#"{"schema":"homeboy/no-tests-applicable/v1","extension_id":"fixture","step":"lint","nonce":"nonce","reason":"docs only"}"#).expect("write wrong step");
        assert!(!no_tests_applicable(
            true,
            &evidence_file,
            "fixture",
            "nonce",
            None
        ));
        std::fs::write(&evidence_file, "not json").expect("write malformed evidence");
        assert!(!no_tests_applicable(
            true,
            &evidence_file,
            "fixture",
            "nonce",
            None
        ));
    }

    #[test]
    fn declared_result_parser_script_normalizes_provider_json() {
        with_isolated_home(|_| {
            // Use an exec-capable tempdir: these tests write a parser script
            // and execute it, so a `noexec` $TMPDIR (e.g. hardened `/tmp`)
            // would fail with exit 126 regardless of the behavior under test.
            let temp_dir = exec_capable_tempdir();
            let extension_dir = temp_dir.path().join("extension");
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            let parser_script = extension_dir.join("parse-results.sh");
            std::fs::write(
                &parser_script,
                r#"#!/usr/bin/env bash
set -euo pipefail
if [ "${2:-}" != "custom-json" ]; then
    exit 7
fi
if [ ! -f "$1" ]; then
    printf 'expected parser input file to exist: %s\n' "$1" >&2
    exit 8
fi
if ! grep -q 'custom-provider/test-results/v1' "$1"; then
    printf 'expected parser input file to contain provider JSON\n' >&2
    exit 9
fi
source "$HOMEBOY_RUNTIME_WRITE_TEST_RESULTS"
parsed=$(python3 - "$1" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as handle:
    data = json.load(handle)

summary = data.get("summary") if isinstance(data.get("summary"), dict) else {}
total = int(summary.get("total") or 0)
passed = int(summary.get("passed") or 0)
failed = int(summary.get("failed") or 0)
skipped = int(summary.get("skipped") or 0)

if total == 0:
    for suite in data.get("suites") or []:
        if not isinstance(suite, dict):
            continue
        total += int(suite.get("tests") or suite.get("total") or 0)
        passed += int(suite.get("passed") or 0)
        failed += int(suite.get("failed") or 0)
        skipped += int(suite.get("skipped") or 0)

print(f"{total}\t{passed}\t{failed}\t{skipped}")
PY
)
IFS=$'\t' read -r total passed failed skipped <<EOF
$parsed
EOF
homeboy_write_test_results "$total" "$passed" "$failed" "$skipped"
printf '{"total":%s,"passed":%s,"failed":%s,"skipped":%s}\n' "$total" "$passed" "$failed" "$skipped"
"#,
            )
            .expect("parser script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&parser_script, std::fs::Permissions::from_mode(0o755))
                    .expect("parser script permissions");
            }

            let component = Component::new(
                "fixture".to_string(),
                temp_dir.path().to_string_lossy().to_string(),
                "fixture-extension".to_string(),
                None,
            );
            let context = crate::extension_execution::ExtensionExecutionContext {
                component: component.clone(),
                capability: ExtensionCapability::Test,
                extension_id: "fixture-extension".to_string(),
                extension_path: extension_dir,
                script_path: "test.sh".to_string(),
                settings: Vec::new(),
                accepted_setting_keys: Vec::new(),
            };
            let spec = ParseSpec {
                extension_script: Some("parse-results.sh".to_string()),
                adapters: vec!["custom-json".to_string()],
                rules: Vec::new(),
                defaults: std::collections::HashMap::new(),
                derive: Vec::new(),
            };
            let run_dir = RunDir::create().expect("run dir");

            run_declared_result_parser(
                &component,
                &context,
                &spec,
                r#"{
                "schema": "custom-provider/test-results/v1",
                "summary": { "total": 0 },
                "suites": [
                    { "tests": 3, "passed": 2, "failed": 1 },
                    { "total": 2, "passed": 1, "skipped": 1 }
                ]
            }"#,
                &run_dir,
            )
            .expect("declared parser should run");

            let counts = parse_test_results_file_with_spec(
                &run_dir.step_file(run_dir::files::TEST_RESULTS),
                Some(&spec),
            )
            .expect("declared parser should write normalized counts");
            let counts = counts.expect("normalized counts should be present");

            run_dir.cleanup();

            assert_eq!(counts.total, 5);
            assert_eq!(counts.passed, 3);
            assert_eq!(counts.failed, 1);
            assert_eq!(counts.skipped, 1);
        });
    }

    #[test]
    fn declared_result_parser_errors_when_script_is_missing() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let extension_dir = temp_dir.path().join("extension");
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        let component = Component::new(
            "fixture".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
            "fixture-extension".to_string(),
            None,
        );
        let context = crate::extension_execution::ExtensionExecutionContext {
            component: component.clone(),
            capability: ExtensionCapability::Test,
            extension_id: "fixture-extension".to_string(),
            extension_path: extension_dir.clone(),
            script_path: "test.sh".to_string(),
            settings: Vec::new(),
            accepted_setting_keys: Vec::new(),
        };
        let spec = ParseSpec {
            extension_script: Some("missing-parser.sh".to_string()),
            adapters: vec!["fixture-json".to_string()],
            rules: Vec::new(),
            defaults: std::collections::HashMap::new(),
            derive: Vec::new(),
        };
        let run_dir = RunDir::create().expect("run dir");

        let err = run_declared_result_parser(&component, &context, &spec, "{}", &run_dir)
            .expect_err("declared missing parser should fail");
        run_dir.cleanup();

        assert_eq!(err.code, ErrorCode::ConfigInvalidValue);
        assert!(err
            .message
            .contains("Declared test result parser script does not exist"));
        assert!(err.message.contains("missing-parser.sh"));
        assert_eq!(err.details["script_path"], "missing-parser.sh");
        assert_eq!(
            err.details["resolved_script"].as_str(),
            Some(
                extension_dir
                    .join("missing-parser.sh")
                    .to_string_lossy()
                    .as_ref()
            )
        );
    }

    #[test]
    fn declared_result_parser_errors_with_context_on_non_zero_exit() {
        // This test executes a capability script, which builds an env derived
        // from HOME / homeboy paths. Run it under the shared `home_lock()` so it
        // is globally serialized against env-mutating tests instead of racing
        // them under default parallelism (#6760, #6804).
        with_isolated_home(|_| {
            // Use an exec-capable tempdir: these tests write a parser script
            // and execute it, so a `noexec` $TMPDIR (e.g. hardened `/tmp`)
            // would fail with exit 126 regardless of the behavior under test.
            let temp_dir = exec_capable_tempdir();
            let extension_dir = temp_dir.path().join("extension");
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            let parser_script = extension_dir.join("parse-results.sh");
            std::fs::write(
                &parser_script,
                r#"#!/usr/bin/env bash
printf 'parser stdout detail\n'
printf 'parser stderr detail\n' >&2
exit 23
"#,
            )
            .expect("parser script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&parser_script, std::fs::Permissions::from_mode(0o755))
                    .expect("parser script permissions");
            }

            let component = Component::new(
                "fixture".to_string(),
                temp_dir.path().to_string_lossy().to_string(),
                "fixture-extension".to_string(),
                None,
            );
            let context = crate::extension_execution::ExtensionExecutionContext {
                component: component.clone(),
                capability: ExtensionCapability::Test,
                extension_id: "fixture-extension".to_string(),
                extension_path: extension_dir,
                script_path: "test.sh".to_string(),
                settings: Vec::new(),
                accepted_setting_keys: Vec::new(),
            };
            let spec = ParseSpec {
                extension_script: Some("parse-results.sh".to_string()),
                adapters: vec!["fixture-json".to_string()],
                rules: Vec::new(),
                defaults: std::collections::HashMap::new(),
                derive: Vec::new(),
            };
            let run_dir = RunDir::create().expect("run dir");

            let err = run_declared_result_parser(&component, &context, &spec, "{}", &run_dir)
                .expect_err("declared parser non-zero exit should fail");
            run_dir.cleanup();

            assert_eq!(err.code, ErrorCode::ConfigInvalidValue);
            assert!(err.message.contains("exit code 23"));
            assert_eq!(err.details["script_path"], "parse-results.sh");
            assert_eq!(err.details["exit_code"], 23);
            assert!(err.details["command"]
                .as_str()
                .unwrap_or_default()
                .contains("parse-results.sh"));
            assert!(err.details["stdout_tail"]
                .as_str()
                .unwrap_or_default()
                .contains("parser stdout detail"));
            assert!(err.details["stderr_tail"]
                .as_str()
                .unwrap_or_default()
                .contains("parser stderr detail"));
        });
    }

    #[test]
    fn declared_result_parser_accepts_flat_count_stdout_json() {
        with_isolated_home(|_| {
            // Use an exec-capable tempdir: these tests write a parser script
            // and execute it, so a `noexec` $TMPDIR (e.g. hardened `/tmp`)
            // would fail with exit 126 regardless of the behavior under test.
            let temp_dir = exec_capable_tempdir();
            let extension_dir = temp_dir.path().join("extension");
            std::fs::create_dir_all(&extension_dir).expect("extension dir");
            let parser_script = extension_dir.join("parse-results.sh");
            std::fs::write(
                &parser_script,
                r#"#!/usr/bin/env bash
set -euo pipefail
printf '{"total":5,"passed":3,"failed":1,"skipped":1}\n'
"#,
            )
            .expect("parser script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&parser_script, std::fs::Permissions::from_mode(0o755))
                    .expect("parser script permissions");
            }

            let component = Component::new(
                "fixture".to_string(),
                temp_dir.path().to_string_lossy().to_string(),
                "fixture-extension".to_string(),
                None,
            );
            let context = crate::extension_execution::ExtensionExecutionContext {
                component: component.clone(),
                capability: ExtensionCapability::Test,
                extension_id: "fixture-extension".to_string(),
                extension_path: extension_dir,
                script_path: "test.sh".to_string(),
                settings: Vec::new(),
                accepted_setting_keys: Vec::new(),
            };
            let spec = ParseSpec {
                extension_script: Some("parse-results.sh".to_string()),
                adapters: vec!["fixture-json".to_string()],
                rules: Vec::new(),
                defaults: std::collections::HashMap::new(),
                derive: Vec::new(),
            };
            let run_dir = RunDir::create().expect("run dir");

            run_declared_result_parser(&component, &context, &spec, "runner output", &run_dir)
                .expect("declared parser stdout should run");

            let counts = parse_test_results_file_with_spec(
                &run_dir.step_file(run_dir::files::TEST_RESULTS),
                Some(&spec),
            )
            .expect("parser stdout JSON should be normalized to test-results.json");
            let counts = counts.expect("normalized counts should be present");

            run_dir.cleanup();

            assert_eq!(counts.total, 5);
            assert_eq!(counts.passed, 3);
            assert_eq!(counts.failed, 1);
            assert_eq!(counts.skipped, 1);
        });
    }

    #[test]
    fn declared_result_parser_rejects_malformed_successful_stdout_json() {
        // Exec-capable tempdir: this test runs the parser script, so a
        // `noexec` $TMPDIR would fail with exit 126 before reaching the
        // malformed-JSON assertion under test.
        let temp_dir = exec_capable_tempdir();
        let extension_dir = temp_dir.path().join("extension");
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        let parser_script = extension_dir.join("parse-results.sh");
        std::fs::write(
            &parser_script,
            r#"#!/usr/bin/env bash
set -euo pipefail
printf 'not json\n'
"#,
        )
        .expect("parser script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&parser_script, std::fs::Permissions::from_mode(0o755))
                .expect("parser script permissions");
        }

        let component = Component::new(
            "fixture".to_string(),
            temp_dir.path().to_string_lossy().to_string(),
            "fixture-extension".to_string(),
            None,
        );
        let context = crate::extension_execution::ExtensionExecutionContext {
            component: component.clone(),
            capability: ExtensionCapability::Test,
            extension_id: "fixture-extension".to_string(),
            extension_path: extension_dir,
            script_path: "test.sh".to_string(),
            settings: Vec::new(),
            accepted_setting_keys: Vec::new(),
        };
        let spec = ParseSpec {
            extension_script: Some("parse-results.sh".to_string()),
            adapters: vec!["fixture-json".to_string()],
            rules: Vec::new(),
            defaults: std::collections::HashMap::new(),
            derive: Vec::new(),
        };
        let run_dir = RunDir::create().expect("run dir");

        let error =
            run_declared_result_parser(&component, &context, &spec, "runner output", &run_dir)
                .expect_err("malformed parser stdout should fail");

        run_dir.cleanup();

        assert!(error.message.contains("Invalid JSON"));
        assert_eq!(error.code.as_str(), "validation.invalid_json");
    }

    #[test]
    fn test_run_self_check_test_workflow() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("test.sh"), "printf test-ok\n")
            .expect("script should be written");

        let mut component = Component::new(
            "fixture".to_string(),
            dir.path().to_string_lossy().to_string(),
            "".to_string(),
            None,
        );
        component.scripts = Some(ComponentScriptsConfig {
            lint: Vec::new(),
            test: vec!["sh test.sh".to_string()],
            build: Vec::new(),
            bench: Vec::new(),
            fuzz: Vec::new(),
            trace: Vec::new(),
            deps: Vec::new(),
        });

        let result =
            run_self_check_test_workflow(&component, dir.path(), "fixture".to_string(), true)
                .expect("test self-check should run");

        assert_eq!(result.status, "passed");
        assert_eq!(result.exit_code, 0);
        assert_eq!(result.component, "fixture");
        assert!(result.summary.is_some());
    }
}

#[cfg(all(test, unix))]
mod inventory_profile_tests {
    use super::*;
    use crate::extension::{TestInventoryConfig, TestInventoryRunner};

    fn wordpress_config() -> TestInventoryConfig {
        TestInventoryConfig {
            root_markers: vec!["composer.json".to_string()],
            fingerprint_names: vec!["composer.json".to_string(), "composer.lock".to_string()],
            fingerprint_extensions: vec!["php".to_string()],
            fingerprint_skip_dirs: vec![".git".to_string(), "vendor".to_string()],
            runners: vec![TestInventoryRunner {
                id: "wordpress".to_string(),
                version_command: vec!["php".to_string(), "--version".to_string()],
            }],
        }
    }

    /// An extension that declares nothing keeps the exact Cargo-derived
    /// behaviour, so Rust needs no manifest change and cannot regress. (#12394)
    #[test]
    fn absent_config_resolves_to_the_cargo_profile() {
        let profile = InventoryProfile::resolve(None).expect("cargo default");
        assert_eq!(profile, InventoryProfile::cargo());
        assert!(
            profile.root_markers.is_empty(),
            "empty markers is what routes root resolution back through cargo metadata"
        );
        assert!(profile.selects(Path::new("crates/foo/src/lib.rs")));
        assert!(profile.selects(Path::new("Cargo.toml")));
        assert!(profile.selects(Path::new("Cargo.lock")));
        assert!(!profile.selects(Path::new("README.md")));
        assert!(profile.skips_dir(std::ffi::OsStr::new("target")));
        assert!(profile.skips_dir(std::ffi::OsStr::new(".git")));
        assert_eq!(
            profile.runner_commands.keys().collect::<Vec<_>>(),
            vec!["cargo", "nextest"]
        );
    }

    /// A declared config drives selection, and a PHP workspace is fingerprinted
    /// by its own files rather than by Rust ones it does not have.
    #[test]
    fn declared_config_selects_its_own_files() {
        let profile = InventoryProfile::resolve(Some(&wordpress_config())).expect("profile");
        assert!(profile.selects(Path::new("inc/Abilities/SystemAbilities.php")));
        assert!(profile.selects(Path::new("composer.lock")));
        assert!(!profile.selects(Path::new("src/lib.rs")));
        assert!(profile.skips_dir(std::ffi::OsStr::new("vendor")));
        assert!(!profile.skips_dir(std::ffi::OsStr::new("inc")));
    }

    /// A fingerprint over zero files hashes the empty string for every
    /// workspace, making unrelated checkouts compare equal. Refuse to bind
    /// rather than bind to a constant.
    #[test]
    fn config_selecting_no_files_is_refused() {
        let mut config = wordpress_config();
        config.fingerprint_names.clear();
        config.fingerprint_extensions.clear();
        assert!(InventoryProfile::resolve(Some(&config)).is_none());
    }

    /// A runner with no executable cannot be fingerprinted, so a config that
    /// declares only such runners cannot bind an inventory.
    #[test]
    fn config_without_a_usable_runner_is_refused() {
        let mut config = wordpress_config();
        config.runners = vec![TestInventoryRunner {
            id: "broken".to_string(),
            version_command: vec![],
        }];
        assert!(InventoryProfile::resolve(Some(&config)).is_none());
    }

    /// Root resolution walks upward to the marker, so a component nested inside
    /// a larger checkout still binds to its own root without `cargo metadata`.
    #[test]
    fn declared_markers_resolve_the_root_without_cargo() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().canonicalize().expect("canonical");
        let nested = root.join("inc/Core/Bootstrap");
        std::fs::create_dir_all(&nested).expect("nested dirs");
        std::fs::write(root.join("composer.json"), "{}").expect("marker");

        let profile = InventoryProfile::resolve(Some(&wordpress_config())).expect("profile");
        assert_eq!(
            inventory_workspace_root(&nested, &profile),
            Some(root.clone())
        );
    }

    /// Without a marker anywhere above it, the component path is its own root
    /// rather than an unrelated ancestor.
    #[test]
    fn missing_marker_falls_back_to_the_component_path() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().canonicalize().expect("canonical");
        let nested = root.join("inc");
        std::fs::create_dir_all(&nested).expect("nested dirs");

        let mut config = wordpress_config();
        config.root_markers = vec!["definitely-absent-marker".to_string()];
        let profile = InventoryProfile::resolve(Some(&config)).expect("profile");
        assert_eq!(inventory_workspace_root(&nested, &profile), Some(nested));
    }

    /// The fingerprint must actually respond to the declared file set: two
    /// checkouts differing only in a selected file must not compare equal.
    #[test]
    fn declared_fingerprint_covers_the_declared_files() {
        let profile = InventoryProfile::resolve(Some(&wordpress_config())).expect("profile");
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().canonicalize().expect("canonical");
        std::fs::create_dir_all(root.join("inc")).expect("inc");
        std::fs::create_dir_all(root.join("vendor")).expect("vendor");
        std::fs::write(root.join("inc/Plugin.php"), "<?php\necho 1;\n").expect("php");
        std::fs::write(root.join("composer.json"), "{}").expect("composer");

        let before = workspace_fingerprint(&root, &profile).expect("fingerprint");

        // A skipped directory must not move the fingerprint.
        std::fs::write(root.join("vendor/Ignored.php"), "<?php\necho 2;\n").expect("vendor php");
        assert_eq!(
            workspace_fingerprint(&root, &profile),
            Some(before.clone()),
            "vendor/ is skipped, so it cannot change the fingerprint"
        );

        // An unselected extension must not move it either.
        std::fs::write(root.join("inc/notes.md"), "notes\n").expect("md");
        assert_eq!(
            workspace_fingerprint(&root, &profile),
            Some(before.clone()),
            "markdown is not a declared fingerprint input"
        );

        // A selected file must.
        std::fs::write(root.join("inc/Plugin.php"), "<?php\necho 3;\n").expect("php edit");
        assert_ne!(
            workspace_fingerprint(&root, &profile),
            Some(before),
            "a declared PHP source file must be covered by the fingerprint"
        );
    }

    /// A runner the binding fingerprinted is accepted even when it is not a
    /// Rust one. #12396 made root resolution, fingerprinting, and runner
    /// identity extension-driven, but the payload validator still carried a
    /// `"cargo" | "nextest"` name allowlist, which refused any declared runner
    /// and left the contract Rust-only in practice. (#12394)
    #[test]
    fn a_declared_non_rust_runner_is_accepted() {
        let run = RunDir::create().expect("run dir");
        let mut binding = super::tests::test_inventory_binding_for_test(run.path());
        binding
            .runner_fingerprints
            .insert("wordpress".to_string(), "d".repeat(64));

        let mut inventory = TestInventoryEvidence {
            schema: TEST_INVENTORY_SCHEMA.to_string(),
            runner: "wordpress".to_string(),
            runner_fingerprint: "d".repeat(64),
            workspace_fingerprint: binding.workspace_fingerprint.clone(),
            tests: vec![TestInventoryTest {
                id: "tests/plugin-smoke.php".to_string(),
                package: "data-machine".to_string(),
                target: "tests".to_string(),
                target_kind: "smoke".to_string(),
                name: "plugin-smoke".to_string(),
                expected_outcome: Some("executed".to_string()),
            }],
            inventory_fingerprint: String::new(),
            fallback_reason: None,
        };
        inventory.inventory_fingerprint = homeboy_engine_primitives::content_hash::sha256_hex(
            &canonical_inventory_json(&inventory),
        );

        let accepted = valid_test_inventory_payload(&inventory, &binding)
            .expect("a declared runner must be accepted");
        assert_eq!(accepted.runner, "wordpress");
        assert_eq!(accepted.test_count, 1);
    }

    /// Dropping the name allowlist must not weaken the check: a runner the
    /// binding never fingerprinted is still refused, because
    /// `expected_runner_fingerprint` has no entry to match against.
    #[test]
    fn an_undeclared_runner_is_still_refused_without_the_name_allowlist() {
        let run = RunDir::create().expect("run dir");
        let binding = super::tests::test_inventory_binding_for_test(run.path());

        let mut inventory = TestInventoryEvidence {
            schema: TEST_INVENTORY_SCHEMA.to_string(),
            runner: "totally-undeclared".to_string(),
            runner_fingerprint: "e".repeat(64),
            workspace_fingerprint: binding.workspace_fingerprint.clone(),
            tests: vec![TestInventoryTest {
                id: "suite::one".to_string(),
                package: "pkg".to_string(),
                target: "target".to_string(),
                target_kind: "test".to_string(),
                name: "one".to_string(),
                expected_outcome: Some("executed".to_string()),
            }],
            inventory_fingerprint: String::new(),
            fallback_reason: None,
        };
        inventory.inventory_fingerprint = homeboy_engine_primitives::content_hash::sha256_hex(
            &canonical_inventory_json(&inventory),
        );

        assert!(valid_test_inventory_payload(&inventory, &binding).is_err());
    }

    /// An empty runner name carries no identity at all.
    #[test]
    fn an_empty_runner_name_is_refused() {
        let run = RunDir::create().expect("run dir");
        let binding = super::tests::test_inventory_binding_for_test(run.path());

        let mut inventory = TestInventoryEvidence {
            schema: TEST_INVENTORY_SCHEMA.to_string(),
            runner: "   ".to_string(),
            runner_fingerprint: "f".repeat(64),
            workspace_fingerprint: binding.workspace_fingerprint.clone(),
            tests: vec![TestInventoryTest {
                id: "suite::one".to_string(),
                package: "pkg".to_string(),
                target: "target".to_string(),
                target_kind: "test".to_string(),
                name: "one".to_string(),
                expected_outcome: Some("executed".to_string()),
            }],
            inventory_fingerprint: String::new(),
            fallback_reason: None,
        };
        inventory.inventory_fingerprint = homeboy_engine_primitives::content_hash::sha256_hex(
            &canonical_inventory_json(&inventory),
        );

        assert!(valid_test_inventory_payload(&inventory, &binding).is_err());
    }

    /// An inventory may only claim a runner the binding fingerprinted.
    #[test]
    fn expected_runner_fingerprint_rejects_an_undeclared_runner() {
        let run = RunDir::create().expect("run dir");
        let binding = super::tests::test_inventory_binding_for_test(run.path());
        assert!(expected_runner_fingerprint(&binding, "cargo").is_some());
        assert!(expected_runner_fingerprint(&binding, "nextest").is_some());
        assert!(expected_runner_fingerprint(&binding, "wordpress").is_none());
        assert!(expected_runner_fingerprint(&binding, "").is_none());
    }
}
