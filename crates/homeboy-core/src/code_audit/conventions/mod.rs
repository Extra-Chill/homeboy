//! Convention discovery — detect structural patterns across similar files.
//!
//! Scans files matched by glob patterns, extracts structural fingerprints
//! (method names, registration calls, naming patterns), then groups them
//! to discover conventions and outliers.

use std::collections::HashMap;
use std::path::Path;

use super::convention_membership::{
    declared_trait_name, declares_type_subject, is_convention_exception, is_utility_like_file,
    member_requirement_deviation,
};
use super::fingerprint::FileFingerprint;
use super::import_matching::has_import_with_context;
use super::naming::{detect_naming_suffix, suffix_matches};
use super::signatures::{compute_signature_skeleton, tokenize_signature};
use homeboy_audit_contract::AuditConfig;

/// `Language` now lives in `homeboy_engine_primitives::language` — the foundation layer,
/// since it is a source-file primitive with no audit dependencies. Re-exported
/// here so existing `code_audit::conventions::Language` paths keep resolving.
pub use homeboy_engine_primitives::language::Language;

/// Generic, framework-agnostic tracker-reference regex defaults shipped with
/// Homeboy core. These match issue/PR/ticket URL shapes that are not tied to
/// any single ecosystem (a generic code-host issue/PR URL and an `@see <url>`
/// provenance reference). Ecosystem-specific tracker hosts (e.g. a particular
/// framework's bug tracker) are not hardcoded in core — they ship in the
/// extension-provided defaults asset and are merged in when a component opts
/// into builtin profile defaults, keeping core free of framework literals
/// (#2240).
pub fn builtin_tracker_reference_regexes() -> &'static [&'static str] {
    &[
        r"https?://github\.com/[\w\-.]+/[\w\-.]+/(?:issues|pull)/\d+",
        r"@see\s+https?://[^\s)]+",
    ]
}

/// A discovered convention: a pattern that most files in a group follow.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Convention {
    /// Human-readable name (auto-generated or from config).
    pub name: String,
    /// The glob pattern that groups these files.
    pub glob: String,
    /// The expected methods/functions that define the convention.
    pub expected_methods: Vec<String>,
    /// The expected registration calls.
    pub expected_registrations: Vec<String>,
    /// The expected interfaces/traits that files should implement.
    pub expected_interfaces: Vec<String>,
    /// The expected namespace pattern (if consistent across files).
    pub expected_namespace: Option<String>,
    /// The expected import/use statements.
    pub expected_imports: Vec<String>,
    /// Files that follow the convention.
    pub conforming: Vec<String>,
    /// Files that deviate from the convention.
    pub outliers: Vec<Outlier>,
    /// How many files were analyzed.
    pub total_files: usize,
    /// Confidence score (0.0 - 1.0).
    pub confidence: f32,
}

/// A file that deviates from a convention.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Outlier {
    /// Relative file path.
    pub file: String,
    /// Whether this outlier appears to be helper/utility drift rather than a real member.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub noisy: bool,
    /// What's missing or different.
    pub deviations: Vec<Deviation>,
}

/// A specific deviation from the convention.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Deviation {
    /// What kind of deviation.
    pub kind: AuditFinding,
    /// Human-readable description.
    pub description: String,
    /// Suggested fix.
    pub suggestion: String,
}

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum AuditFinding {
    MissingMethod,
    ExtraMethod,
    MissingRegistration,
    DifferentRegistration,
    MissingInterface,
    NamingMismatch,
    SignatureMismatch,
    NamespaceMismatch,
    MissingImport,
    /// File exceeds line count threshold.
    GodFile,
    /// File has too many top-level items.
    HighItemCount,
    /// Directory has too many source files in a flat namespace.
    DirectorySprawl,
    /// Function body is duplicated across files.
    DuplicateFunction,
    /// Function has identical structure but different identifiers/literals.
    NearDuplicate,
    /// Function parameter is declared but never used in the function body.
    /// When call-site data is available, this means no callers pass a value
    /// for this position — truly dead, safe to remove.
    UnusedParameter,
    /// Function parameter is received but ignored — callers ARE passing values
    /// for this position, but the function doesn't use them. Higher severity
    /// than UnusedParameter: likely a bug or stale param from a refactor.
    IgnoredParameter,
    /// Developer has marked code with a dead code suppression attribute.
    DeadCodeMarker,
    /// Public function/method is never imported or called by any other file.
    UnreferencedExport,
    /// Private/internal function is never called within the same file.
    OrphanedInternal,
    /// Source file has no corresponding test file.
    MissingTestFile,
    /// Source method/function has no corresponding test method.
    MissingTestMethod,
    /// Test file or test method has no corresponding source file/method.
    OrphanedTest,
    /// Test method has a placeholder/no-op body that does not exercise product code.
    VacuousTest,
    /// Comment starts with TODO/FIXME/HACK/XXX marker.
    TodoMarker,
    /// Comment starts with stale or legacy phrasing.
    LegacyComment,
    /// File violates a configured architecture/layer ownership rule.
    LayerOwnershipViolation,
    /// Inline test modules are present in source files instead of centralized tests.
    InlineTestModule,
    /// Test files are placed under source directories instead of the central tests tree.
    ScatteredTestFile,
    /// Duplicated code block found within the same method/function body.
    IntraMethodDuplicate,
    /// Two functions in different files follow the same call pattern —
    /// they invoke a parallel sequence of helpers, suggesting the shared
    /// workflow should be abstracted into a single parameterized function.
    ParallelImplementation,
    /// Documentation references a file, directory, or class that no longer exists.
    BrokenDocReference,
    /// Source feature (struct, trait, function, hook) has no mention in any docs.
    UndocumentedFeature,
    /// Documentation exists but references stale paths that have moved.
    StaleDocReference,
    /// Compiler warning (dead code, unused import, unused variable, etc).
    /// Detected by running an extension-owned language compiler/checker script.
    CompilerWarning,
    /// Wrapper file is missing an explicit declaration of what it wraps.
    /// Detected by tracing calls in the wrapper to infer the implementation target.
    MissingWrapperDeclaration,
    /// Two directories contain overlapping file names with high content similarity.
    /// Indicates a copy-paste module that was never consolidated.
    ShadowModule,
    /// Inline array/object literal shape (ordered keys + value kinds) appears
    /// many times across the codebase — candidate for extraction into a helper
    /// constructor (e.g. `error_envelope($error, $message)`).
    RepeatedLiteralShape,
    /// Docblock `@deprecated X.Y.Z` tag is older than the configured age
    /// threshold relative to the component's current version.
    DeprecationAge,
    /// `function_exists` / `class_exists` / `defined` guard on a symbol that is
    /// guaranteed to exist given plugin requirements, explicit bootstrap
    /// `require`s, or the WordPress core version baseline.
    DeadGuard,
    /// Code that exists because of a tracked upstream bug — workaround/polyfill/
    /// shim/hack comments paired with an issue/PR/Trac reference, or
    /// `version_compare(...) <` guards against known constants.
    ///
    /// Distinct from `LegacyComment`: `LegacyComment` flags any stale phrasing
    /// regardless of whether a tracker exists. `UpstreamWorkaround` requires
    /// BOTH a workaround marker AND a concrete reference (URL or ticket), so
    /// findings are actionable: check the linked issue, see if the upstream
    /// fix has shipped, then remove the local workaround. Per the
    /// fix-upstream-first rule, workarounds should never outlive their cause.
    ///
    /// Severity scales by tier:
    /// - Marker + reference (Tier A) → `Severity::Warning`
    /// - `version_compare` guard (Tier B) → `Severity::Info`
    UpstreamWorkaround,
    /// A group of classes in the same directory subtree share the same overall
    /// method-shape (same method names + visibilities + order) and have high
    /// per-method body similarity — candidates for a shared base class.
    SharedScaffolding,
    /// Class whose public methods are mostly single-expression delegates to an
    /// internal member — usually a split-then-rejoin facade or legacy wrapper.
    FacadePassthrough,
    /// SQL uses LIKE to match exact JSON key/value semantics in a blob column
    /// such as metadata, engine_data, config, or payload.
    JsonLikeExactMatch,
    /// String literal duplicates a slug value that is already centralized in a
    /// class constant, making drift possible despite the constant existing.
    ConstantBackedSlugLiteral,
    /// Comments/docblocks promise network/site-option storage while nearby code
    /// uses single-site get_option/update_option calls.
    OptionScopeDrift,
    /// Docs/schema claim a scoped internal proxy while implementation forwards
    /// request-controlled targets without an explicit allowlist/prefix marker.
    ProxyScopeDrift,
    /// Tests mutate process-global environment variables without using the
    /// shared guard for that variable.
    GlobalEnvMutationGuard,
    /// Nested Rust test file is not wired into Cargo via a source-module
    /// `#[path = "..."] mod ...;` declaration.
    UnwiredNestedRustTest,
    /// Command-family files independently assemble the same generic execution
    /// contract phases and contract-call shape.
    ParallelRunnerSetup,
    /// Remote execution dispatch lacks an explicit preflight for path/artifact
    /// translation before handing arguments to a remote runtime.
    RemoteExecutionPreflight,
    /// Repeated exhaustive match blocks over the same enum duplicate a
    /// label/getter/policy contract that should likely live on the enum.
    RepeatedEnumDispatchContract,
    /// Direct aggregate/struct literals are repeated even though a canonical
    /// construction seam exists for the same type.
    DirectAggregateConstruction,
    /// Configured key has write/migration/accessor evidence but no non-test read.
    WriteOnlyConfigKey,
    /// Configured ecosystem/language/framework term appears in core-owned source.
    CoreBoundaryLeak,
    /// Configured source policy term appears in a disallowed source scope.
    SourcePolicyViolation,
    /// Configured mutating handler/resource-id path lacks a direct ownership or
    /// access check, or a trusted delegation marker known to enforce one.
    MutatingResourceAccess,
    /// Config/schema key appears in one side of a round-trip path but not the other.
    ConfigRoundtripAsymmetry,
    /// Public endpoint exposes registry/config metadata through a raw getter
    /// while a permission-aware resolver/helper exists nearby.
    PublicRegistryExposure,
    /// Request-derived redirect destination reaches a configured redirect sink
    /// before configured URL validation dominates the sink path.
    RedirectValidation,
    /// Persisted artifact evidence points at a runtime-local temp path instead
    /// of a durable artifact-store path or portable artifact token.
    NonPortableArtifactPath,
    /// Command output capture retains stdout/stderr or repeated details without
    /// an explicit size bound and truncation metadata.
    UnboundedOutputCapture,
    /// Declared command scenario output differs from its expected status contract.
    CommandStatusContractViolation,
    /// A declared command-status scenario references a golden fixture file that
    /// is missing or unreadable. This is test-data hygiene (write or remove the
    /// fixture), distinct from an actual status-contract violation.
    CommandStatusFixtureMissing,
    /// A command-layer module accumulates orchestration/business logic that
    /// should live in a core service. Command modules are expected to stay thin
    /// adapters (argument parsing, typed request construction, output
    /// formatting); orchestration density beyond the configured threshold is a
    /// boundary violation.
    ThinCommandAdapterViolation,
}

pub(crate) fn unwired_test_file_finding() -> AuditFinding {
    AuditFinding::UnwiredNestedRustTest
}

impl AuditFinding {
    /// All known variant names in snake_case, for CLI help and error messages.
    pub fn all_names() -> &'static [&'static str] {
        &[
            "missing_method",
            "extra_method",
            "missing_registration",
            "different_registration",
            "missing_interface",
            "naming_mismatch",
            "signature_mismatch",
            "namespace_mismatch",
            "missing_import",
            "god_file",
            "high_item_count",
            "directory_sprawl",
            "duplicate_function",
            "near_duplicate",
            "unused_parameter",
            "ignored_parameter",
            "dead_code_marker",
            "unreferenced_export",
            "orphaned_internal",
            "missing_test_file",
            "missing_test_method",
            "orphaned_test",
            "vacuous_test",
            "todo_marker",
            "legacy_comment",
            "layer_ownership_violation",
            "inline_test_module",
            "scattered_test_file",
            "intra_method_duplicate",
            "parallel_implementation",
            "broken_doc_reference",
            "undocumented_feature",
            "stale_doc_reference",
            "compiler_warning",
            "missing_wrapper_declaration",
            "shadow_module",
            "repeated_field_pattern",
            "repeated_literal_shape",
            "deprecation_age",
            "dead_guard",
            "upstream_workaround",
            "shared_scaffolding",
            "facade_passthrough",
            "json_like_exact_match",
            "constant_backed_slug_literal",
            "option_scope_drift",
            "proxy_scope_drift",
            "global_env_mutation_guard",
            "unwired_nested_rust_test",
            "parallel_runner_setup",
            "repeated_enum_dispatch_contract",
            "direct_aggregate_construction",
            "write_only_config_key",
            "core_boundary_leak",
            "source_policy_violation",
            "mutating_resource_access",
            "config_roundtrip_asymmetry",
            "public_registry_exposure",
            "redirect_validation",
            "non_portable_artifact_path",
            "unbounded_output_capture",
            "command_status_contract_violation",
            "command_status_fixture_missing",
            "thin_command_adapter_violation",
        ]
    }
}

impl std::str::FromStr for AuditFinding {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
        let json = format!("\"{}\"", normalized);
        serde_json::from_str(&json).map_err(|_| {
            format!(
                "unknown finding kind '{}'. Valid kinds: {}",
                value,
                Self::all_names().join(", ")
            )
        })
    }
}

// ============================================================================
// Import Matching
// ============================================================================

// ============================================================================
// Fingerprinting — Extension-powered
// ============================================================================

// ============================================================================
// Convention Discovery
// ============================================================================

/// Discover conventions from a set of fingerprints that share a common grouping.
///
/// The algorithm:
/// 1. Find methods that appear in ≥ 60% of files (the "convention")
/// 2. Find files that are missing any of those methods (the "outliers")
pub fn discover_conventions_with_config(
    group_name: &str,
    glob_pattern: &str,
    fingerprints: &[FileFingerprint],
    audit_config: &AuditConfig,
) -> Option<Convention> {
    if fingerprints.len() < 2 {
        return None; // Need at least 2 files to detect a pattern
    }

    let total = fingerprints.len();
    let threshold = (total as f32 * 0.6).ceil() as usize;
    let typed_count = fingerprints
        .iter()
        .filter(|fp| declares_type_subject(fp))
        .count();
    let typed_subject_convention = typed_count >= threshold;

    // Count method frequency
    let mut method_counts: HashMap<String, usize> = HashMap::new();
    for fp in fingerprints {
        for method in &fp.methods {
            *method_counts.entry(method.clone()).or_insert(0) += 1;
        }
    }

    // Methods appearing in ≥ threshold files are "expected".
    let is_test_group = super::walker::is_test_path(glob_pattern);
    let expected_methods: Vec<String> = if is_test_group {
        // Test-file helpers (`run`, `init_repo`, fixture builders, etc.) are local
        // scaffolding, not production API conventions every sibling test must carry.
        Vec::new()
    } else {
        method_counts
            .iter()
            .filter(|(_, count)| **count >= threshold)
            .map(|(name, _)| name.clone())
            .collect()
    };

    if expected_methods.is_empty() {
        return None; // No convention found
    }

    // Count registration frequency
    let mut reg_counts: HashMap<String, usize> = HashMap::new();
    for fp in fingerprints {
        for reg in &fp.registrations {
            *reg_counts.entry(reg.clone()).or_insert(0) += 1;
        }
    }

    let expected_registrations: Vec<String> = reg_counts
        .iter()
        .filter(|(_, count)| **count >= threshold)
        .map(|(name, _)| name.clone())
        .collect();

    // Count interface/trait frequency
    let mut interface_counts: HashMap<String, usize> = HashMap::new();
    for fp in fingerprints {
        for iface in &fp.implements {
            *interface_counts.entry(iface.clone()).or_insert(0) += 1;
        }
    }

    let declared_traits: Vec<String> = fingerprints
        .iter()
        .filter_map(declared_trait_name)
        .collect();

    let expected_interfaces: Vec<String> = interface_counts
        .iter()
        .filter(|(_, count)| **count >= threshold)
        .filter(|(name, _)| !declared_traits.contains(name))
        .map(|(name, _)| name.clone())
        .collect();

    // Discover namespace convention (most common namespace)
    let mut ns_counts: HashMap<String, usize> = HashMap::new();
    for fp in fingerprints {
        if let Some(ns) = &fp.namespace {
            *ns_counts.entry(ns.clone()).or_insert(0) += 1;
        }
    }
    let expected_namespace = ns_counts
        .iter()
        .filter(|(_, count)| **count >= threshold)
        .max_by_key(|(_, count)| *count)
        .map(|(ns, _)| ns.clone());

    // Discover import conventions (imports appearing in ≥ threshold files)
    let mut import_counts: HashMap<String, usize> = HashMap::new();
    for fp in fingerprints {
        for imp in &fp.imports {
            *import_counts.entry(imp.clone()).or_insert(0) += 1;
        }
    }
    let expected_imports: Vec<String> = import_counts
        .iter()
        .filter(|(_, count)| **count >= threshold)
        .map(|(name, _)| name.clone())
        .collect();

    // Use primary type_name (one per file) for suffix detection so multi-type
    // files don't dilute the convention signal. The full type_names list is only
    // used below for the per-file conformance check.
    let primary_type_names: Vec<String> = fingerprints
        .iter()
        .filter_map(|fp| fp.type_name.clone())
        .collect();

    let naming_suffix = detect_naming_suffix(&primary_type_names);

    // Classify files
    let mut conforming = Vec::new();
    let mut outliers = Vec::new();

    for fp in fingerprints {
        if typed_subject_convention && !declares_type_subject(fp) {
            continue;
        }

        // A file is "helper-like" only if NONE of its types match the convention suffix.
        // This prevents false positives where the primary type_name doesn't match but
        // the file contains another type that does (e.g., VersionOutput + VersionArgs).
        let helper_like = naming_suffix.as_ref().is_some_and(|suffix| {
            let names_to_check: Vec<&str> = if !fp.type_names.is_empty() {
                fp.type_names.iter().map(|s| s.as_str()).collect()
            } else {
                fp.type_name.as_deref().into_iter().collect()
            };
            !names_to_check.is_empty()
                && names_to_check
                    .iter()
                    .all(|name| !suffix_matches(name, suffix))
        });
        let utility_like = helper_like && is_utility_like_file(fp, audit_config);
        let convention_exempt = is_convention_exception(fp, audit_config);
        let skip_member_requirements = helper_like || convention_exempt;

        let mut deviations = Vec::new();

        if helper_like && !utility_like && !convention_exempt {
            let suffix = naming_suffix.as_deref().unwrap_or("member");
            deviations.push(Deviation {
                kind: AuditFinding::NamingMismatch,
                description: format!(
                    "Helper-like name does not match convention suffix '{}': {}",
                    suffix,
                    fp.type_name
                        .clone()
                        .unwrap_or_else(|| fp.relative_path.clone())
                ),
                suggestion: format!(
                    "Treat this as a utility/helper or rename it to match the '{}' convention",
                    suffix
                ),
            });
        }

        // Check missing methods
        for expected in &expected_methods {
            if skip_member_requirements {
                continue;
            }
            if !fp.methods.contains(expected) {
                deviations.push(member_requirement_deviation(
                    AuditFinding::MissingMethod,
                    "Missing method",
                    "Add",
                    expected,
                    "()",
                    group_name,
                ));
            }
        }

        // Check missing registrations
        for expected in &expected_registrations {
            if skip_member_requirements {
                continue;
            }
            if !fp.registrations.contains(expected) {
                deviations.push(member_requirement_deviation(
                    AuditFinding::MissingRegistration,
                    "Missing registration",
                    "Add",
                    expected,
                    " call",
                    group_name,
                ));
            }
        }

        // Check missing interfaces/traits
        for expected in &expected_interfaces {
            if skip_member_requirements {
                continue;
            }
            if !fp.implements.contains(expected) {
                deviations.push(member_requirement_deviation(
                    AuditFinding::MissingInterface,
                    "Missing interface",
                    "Implement",
                    expected,
                    "",
                    group_name,
                ));
            }
        }

        // Check namespace mismatch
        if let Some(expected_ns) = &expected_namespace {
            if let Some(actual_ns) = &fp.namespace {
                if actual_ns != expected_ns {
                    deviations.push(Deviation {
                        kind: AuditFinding::NamespaceMismatch,
                        description: format!(
                            "Namespace mismatch: expected `{}`, found `{}`",
                            expected_ns, actual_ns
                        ),
                        suggestion: format!("Change namespace to `{}`", expected_ns),
                    });
                }
            }
            // Missing namespace when others have one is also a deviation
            if fp.namespace.is_none() {
                deviations.push(Deviation {
                    kind: AuditFinding::NamespaceMismatch,
                    description: format!(
                        "Missing namespace declaration (expected `{}`)",
                        expected_ns
                    ),
                    suggestion: format!("Add `namespace {};`", expected_ns),
                });
            }
        }

        // Check missing imports (aware of grouped imports, path equivalence, usage,
        // self-imports, and same-namespace references).
        for expected_imp in &expected_imports {
            if !has_import_with_context(
                expected_imp,
                &fp.imports,
                &fp.content,
                fp.namespace.as_deref(),
                fp.type_name.as_deref(),
                &fp.type_names,
            ) {
                deviations.push(Deviation {
                    kind: AuditFinding::MissingImport,
                    description: format!("Missing import: {}", expected_imp),
                    suggestion: format!(
                        "Add `use {};` to match the convention in {}",
                        expected_imp, group_name
                    ),
                });
            }
        }

        if deviations.is_empty() {
            conforming.push(fp.relative_path.clone());
        } else {
            outliers.push(Outlier {
                file: fp.relative_path.clone(),
                noisy: helper_like,
                deviations,
            });
        }
    }

    let conforming_count = conforming.len();
    let confidence = conforming_count as f32 / total as f32;

    log_status!(
        "audit",
        "Convention '{}': {}/{} files conform (confidence: {:.0}%)",
        group_name,
        conforming_count,
        total,
        confidence * 100.0
    );

    Some(Convention {
        name: group_name.to_string(),
        glob: glob_pattern.to_string(),
        expected_methods,
        expected_registrations,
        expected_interfaces,
        expected_namespace,
        expected_imports,
        conforming,
        outliers,
        total_files: total,
        confidence,
    })
}

// ============================================================================
// Signature Consistency
// ============================================================================

/// Check method signatures across all files in a convention for consistency.
///
/// Uses structural comparison: signatures are tokenized and compared
/// position-by-position. Positions where tokens vary across files are treated
/// as "type parameters" (expected to differ). Only structural differences
/// (different token count, different constant tokens) are flagged.
pub fn check_signature_consistency(
    conventions: &mut [Convention],
    root: &Path,
    audit_config: &AuditConfig,
) {
    for conv in conventions.iter_mut() {
        if conv.expected_methods.is_empty() {
            continue;
        }

        // Detect language from the glob pattern
        let lang = if conv.glob.ends_with(".php") || conv.glob.ends_with("/*") {
            // Check first conforming file extension
            conv.conforming
                .first()
                .and_then(|f| f.rsplit('.').next())
                .map(Language::from_extension)
                .unwrap_or(Language::Unknown)
        } else {
            Language::Unknown
        };

        if lang == Language::Unknown {
            continue;
        }

        // Collect signatures for each method across ALL files (conforming + outliers)
        let all_files: Vec<String> = conv
            .conforming
            .iter()
            .chain(conv.outliers.iter().map(|o| &o.file))
            .filter(|file| {
                !audit_config
                    .convention_exception_globs
                    .iter()
                    .any(|pattern| glob_match::glob_match(pattern, file))
            })
            .cloned()
            .collect();

        // method_name -> [(file, raw_signature)]
        let mut method_sigs: HashMap<String, Vec<(String, String)>> = HashMap::new();

        for file in &all_files {
            let full_path = root.join(file);
            let content = match std::fs::read_to_string(&full_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let sigs = crate::refactor::plan::generate::extract_signatures(&content, &lang);
            for sig in &sigs {
                if conv.expected_methods.contains(&sig.name) {
                    method_sigs
                        .entry(sig.name.clone())
                        .or_default()
                        .push((file.clone(), sig.signature.clone()));
                }
            }
        }

        // For each method, compute the structural skeleton and find mismatches
        let mut new_outlier_deviations: HashMap<String, Vec<Deviation>> = HashMap::new();

        for (method, file_sigs) in &method_sigs {
            if file_sigs.len() < 2 {
                continue;
            }

            let tokenized: Vec<Vec<String>> = file_sigs
                .iter()
                .map(|(_, sig)| tokenize_signature(sig))
                .collect();

            match compute_signature_skeleton(&tokenized) {
                Some(skeleton) => {
                    // Skeleton computed — all signatures have the same structure.
                    // Check each file against the skeleton's constant positions.
                    for (i, (file, sig)) in file_sigs.iter().enumerate() {
                        let tokens = &tokenized[i];
                        let mut mismatches = Vec::new();
                        for (j, expected) in skeleton.iter().enumerate() {
                            if let Some(expected_token) = expected {
                                if j < tokens.len() && &tokens[j] != expected_token {
                                    mismatches.push((expected_token.clone(), tokens[j].clone()));
                                }
                            }
                        }
                        if !mismatches.is_empty() {
                            // This file's constant tokens differ — real mismatch
                            let canonical_sig = skeleton
                                .iter()
                                .map(|s| s.as_deref().unwrap_or("<_>"))
                                .collect::<Vec<_>>()
                                .join(" ");
                            new_outlier_deviations
                                .entry(file.clone())
                                .or_default()
                                .push(Deviation {
                                    kind: AuditFinding::SignatureMismatch,
                                    description: format!(
                                        "Signature mismatch for {}: expected structure `{}`, found `{}`",
                                        method, canonical_sig, sig
                                    ),
                                    suggestion: format!(
                                        "Update {}() to match the structural pattern: `{}`",
                                        method, canonical_sig
                                    ),
                                });
                        }
                    }
                }
                None => {
                    // Different token counts — possible structural mismatch.
                    // Group signatures by token count to identify signature families.
                    // A token count shared by 2+ files is an intentional variant (e.g.,
                    // different handler types with the same method name but different
                    // parameter lists). Only flag truly isolated signatures — those
                    // with a token count that appears exactly once (#691).
                    let mut len_counts: HashMap<usize, usize> = HashMap::new();
                    for t in &tokenized {
                        *len_counts.entry(t.len()).or_insert(0) += 1;
                    }
                    let max_family_size = len_counts.values().copied().max().unwrap_or(0);
                    if max_family_size < 2 {
                        continue;
                    }

                    let majority_lens: Vec<usize> = len_counts
                        .iter()
                        .filter(|(_, count)| **count == max_family_size)
                        .map(|(len, _)| *len)
                        .collect();
                    if majority_lens.len() != 1 {
                        continue;
                    }

                    let majority_len = majority_lens[0];

                    // Build canonical from majority-length sigs
                    let majority_sigs: Vec<&Vec<String>> = tokenized
                        .iter()
                        .filter(|t| t.len() == majority_len)
                        .collect();

                    let canonical_display = if let Some(first) = majority_sigs.first() {
                        first.join(" ")
                    } else {
                        continue;
                    };

                    for (i, (file, sig)) in file_sigs.iter().enumerate() {
                        let this_len = tokenized[i].len();
                        if this_len == majority_len {
                            continue;
                        }
                        // Only flag if this token count is truly isolated (count == 1).
                        // Multiple files sharing the same non-majority signature
                        // indicates an intentional variant, not a mismatch.
                        let family_size = len_counts.get(&this_len).copied().unwrap_or(0);
                        if family_size >= 2 {
                            continue;
                        }
                        new_outlier_deviations
                            .entry(file.clone())
                            .or_default()
                            .push(Deviation {
                                kind: AuditFinding::SignatureMismatch,
                                description: format!(
                                    "Signature mismatch for {}: different structure — expected {} tokens, found {}. Example: `{}`",
                                    method, majority_len, tokenized[i].len(), sig
                                ),
                                suggestion: format!(
                                    "Update {}() to match the structural pattern: `{}`",
                                    method, canonical_display
                                ),
                            });
                    }
                }
            }
        }

        if new_outlier_deviations.is_empty() {
            continue;
        }

        // Move conforming files with mismatches to outliers
        let mut moved_files = Vec::new();
        for file in &conv.conforming {
            if let Some(devs) = new_outlier_deviations.remove(file) {
                moved_files.push(file.clone());
                conv.outliers.push(Outlier {
                    file: file.clone(),
                    noisy: false,
                    deviations: devs,
                });
            }
        }
        conv.conforming.retain(|f| !moved_files.contains(f));

        // Add deviations to existing outliers
        for outlier in &mut conv.outliers {
            if let Some(devs) = new_outlier_deviations.remove(&outlier.file) {
                outlier.deviations.extend(devs);
            }
        }

        // Recalculate confidence
        conv.confidence = conv.conforming.len() as f32 / conv.total_files as f32;
    }
}

// ============================================================================
// Auto-Discovery
// ============================================================================

// ============================================================================
// Cross-Directory Discovery
// ============================================================================

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests;
