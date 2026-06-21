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
use crate::core::component::AuditConfig;

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Php,
    Rust,
    JavaScript,
    TypeScript,
    #[default]
    Unknown,
}

impl Language {
    pub fn from_extension(ext: &str) -> Self {
        match ext {
            "php" => Language::Php,
            "rs" => Language::Rust,
            "js" | "jsx" | "mjs" => Language::JavaScript,
            "ts" | "tsx" => Language::TypeScript,
            _ => Language::Unknown,
        }
    }

    pub fn from_path(path: &std::path::Path) -> Self {
        path.extension()
            .and_then(|e| e.to_str())
            .map(Self::from_extension)
            .unwrap_or(Self::Unknown)
    }

    /// Resolve a configured language/ecosystem token to a [`Language`].
    ///
    /// Accepts both file-extension tokens (`rs`, `js`) and ecosystem names
    /// (`rust`, `javascript`). This is the single, language-aware home for the
    /// token→language mapping so detector implementations under
    /// `code_audit::detectors` can stay free of hardcoded ecosystem literals:
    /// they declare which tokens a component opted into (via config) and ask
    /// this helper whether a fingerprint's language is one of them.
    pub fn from_token(token: &str) -> Self {
        match token.trim().to_ascii_lowercase().as_str() {
            "php" => Language::Php,
            "rust" | "rs" => Language::Rust,
            "javascript" | "js" | "jsx" | "mjs" => Language::JavaScript,
            "typescript" | "ts" | "tsx" => Language::TypeScript,
            _ => Language::Unknown,
        }
    }

    /// Whether this language is the one named by `token` (extension or
    /// ecosystem name). `Unknown` never matches.
    pub fn matches_token(&self, token: &str) -> bool {
        let resolved = Self::from_token(token);
        resolved != Language::Unknown && resolved == *self
    }

    /// Whether any token in `tokens` names this language.
    pub fn matches_any_token<I, S>(&self, tokens: I) -> bool
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        tokens
            .into_iter()
            .any(|token| self.matches_token(token.as_ref()))
    }

    /// The canonical file-extension tokens for every language Homeboy can
    /// classify. This is the agnostic home for the default scan/apply token set
    /// used by detectors when a component opts into builtin profile defaults —
    /// it keeps the concrete extension literals out of the detector
    /// implementations under `code_audit::detectors`.
    pub fn builtin_extension_tokens() -> &'static [&'static str] {
        &["rs", "php", "ts", "js", "go"]
    }

    /// The ecosystem tokens whose builtin version-compare guard defaults ship
    /// with Homeboy. Version-compatibility guard syntax (`version_compare(...)`)
    /// is ecosystem-specific, so the concrete token set lives here in the
    /// agnostic conventions home rather than hardcoded inside a detector under
    /// `code_audit::detectors`. Components that opt into builtin defaults
    /// inherit these; others declare their own via config.
    pub fn builtin_version_guard_tokens() -> &'static [&'static str] {
        &["php"]
    }

    /// Extension tokens whose source files embed unit tests inline in the same
    /// file (e.g. Rust's `#[cfg(test)] mod tests { ... }`). Detectors that parse
    /// production structure must strip these inline test modules first so test
    /// fixtures are never mistaken for production declarations. Components that
    /// opt into builtin defaults inherit this set; others declare their own.
    pub fn builtin_inline_test_strip_tokens() -> &'static [&'static str] {
        &["rs"]
    }

    /// File-name suffixes that mark a whole file as test-only across the
    /// languages Homeboy can classify. Detectors skip these entirely so their
    /// fixtures and assertions never count as production structure. Components
    /// that opt into builtin defaults inherit this set; others declare theirs.
    pub fn builtin_test_file_suffixes() -> &'static [&'static str] {
        &["_test.rs", "_test.php", ".test.ts", ".test.js", ".test.tsx"]
    }
}

/// Builtin tracker-reference regex defaults shipped with Homeboy. These match
/// the concrete issue/PR/ticket URL shapes used by common upstream trackers.
/// The ecosystem-specific URL literals (e.g. project-tracker hosts) live here
/// in the agnostic conventions home so detector implementations under
/// `code_audit::detectors` stay free of hardcoded ecosystem literals — they
/// pull this default set when a component opts into builtin profile defaults.
pub fn builtin_tracker_reference_regexes() -> &'static [&'static str] {
    &[
        r"https?://github\.com/[\w\-.]+/[\w\-.]+/(?:issues|pull)/\d+",
        r"core\.trac\.wordpress\.org/ticket/\d+",
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
    /// Multiple structs define the same field group — candidates for extraction
    /// into a shared type and flattening/embedding.
    RepeatedFieldPattern,
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
    /// Config-like key/value is written, migrated, or surfaced by an accessor
    /// without a corresponding production read/consumer.
    ConfigKeyWriteOnly,
    /// Config/schema key appears in one side of a round-trip path but not the other.
    ConfigRoundtripAsymmetry,
    /// Public metadata endpoint reads a raw registry/config source without the
    /// project-local resolver/policy companion.
    PublicRegistryResolverBypass,
    /// Mutating resource handler lacks the configured ownership/access companion.
    MutatingResourceOwnershipMissing,
    /// Request-derived redirect destination lacks a dominating validation companion.
    UndominatedRedirectParam,
    /// Direct construction bypasses a configured factory/filter/resolver seam.
    FactorySeamBypass,
    /// Internal REST/API proxy call lacks a configured namespace or scope guard.
    InternalProxyScopeMissing,
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
            "config_key_write_only",
            "config_roundtrip_asymmetry",
            "public_registry_resolver_bypass",
            "mutating_resource_ownership_missing",
            "undominated_redirect_param",
            "factory_seam_bypass",
            "internal_proxy_scope_missing",
            "public_registry_exposure",
            "redirect_validation",
            "non_portable_artifact_path",
            "unbounded_output_capture",
            "command_status_contract_violation",
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

            let sigs = crate::core::refactor::plan::generate::extract_signatures(&content, &lang);
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
mod tests {
    use super::*;

    fn discover_conventions(
        group_name: &str,
        glob_pattern: &str,
        fingerprints: &[FileFingerprint],
    ) -> Option<Convention> {
        discover_conventions_with_config(
            group_name,
            glob_pattern,
            fingerprints,
            &AuditConfig::default(),
        )
    }

    fn framework_like_audit_config() -> AuditConfig {
        AuditConfig {
            utility_suffixes: vec![
                "Helper".to_string(),
                "Helpers".to_string(),
                "Constants".to_string(),
                "Categories".to_string(),
                "Sanitizer".to_string(),
                "Renderer".to_string(),
                "Validator".to_string(),
                "Verifier".to_string(),
                "Resolver".to_string(),
                "Factory".to_string(),
                "Builder".to_string(),
                "Result".to_string(),
                "Scheduling".to_string(),
            ],
            ..Default::default()
        }
    }

    /// Return `true` only when the Rust grammar is discoverable via the
    /// extension registry.
    ///
    /// `check_signature_consistency` → `extract_signatures_from_items` →
    /// `load_grammar_for_ext("rs")` depends on the `rust` extension being
    /// installed under `~/.config/homeboy/extensions/`. In CI that's
    /// guaranteed, but on developer machines (or minimal dev setups that
    /// only have the `wordpress` extension) it may be absent — without
    /// this guard the signature-consistency tests fail with a confusing
    /// assertion instead of a clear skip.
    ///
    /// Tests that parse real Rust source via the grammar call this helper
    /// and early-return when it reports `false`. `eprintln!` surfaces the
    /// skip in test output so the gap is visible rather than silent.
    fn rust_grammar_available() -> bool {
        crate::core::code_audit::core_fingerprint::load_grammar_for_ext("rs").is_some()
    }

    /// Short-circuit the calling test when the Rust grammar isn't
    /// available, emitting a notice to stderr so CI output still records
    /// the skip.
    macro_rules! require_rust_grammar {
        ($test_name:expr) => {
            if !rust_grammar_available() {
                eprintln!(
                    "skip: {} requires the `rust` extension/grammar to be installed",
                    $test_name
                );
                return;
            }
        };
    }

    #[test]
    fn convention_needs_minimum_two_files() {
        let fingerprints = vec![FileFingerprint {
            relative_path: "single.php".to_string(),
            language: Language::Php,
            methods: vec!["run".to_string()],
            ..Default::default()
        }];

        assert!(discover_conventions("Single", "*.php", &fingerprints).is_none());
    }

    #[test]
    fn language_from_extension() {
        assert_eq!(Language::from_extension("php"), Language::Php);
        assert_eq!(Language::from_extension("rs"), Language::Rust);
        assert_eq!(Language::from_extension("ts"), Language::TypeScript);
        assert_eq!(Language::from_extension("jsx"), Language::JavaScript);
        assert_eq!(Language::from_extension("txt"), Language::Unknown);
    }

    #[test]
    fn test_from_path() {
        assert_eq!(Language::from_path(Path::new("src/lib.rs")), Language::Rust);
        assert_eq!(
            Language::from_path(Path::new("src/app.tsx")),
            Language::TypeScript
        );
        assert_eq!(Language::from_path(Path::new("README")), Language::Unknown);
    }

    #[test]
    fn test_all_names() {
        assert!(AuditFinding::all_names().contains(&"write_only_config_key"));
        assert!(AuditFinding::all_names().contains(&"core_boundary_leak"));
    }

    #[test]
    fn test_discover_conventions_with_config() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/CreateAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("CreateAbility".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/UpdateAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("UpdateAbility".to_string()),
                ..Default::default()
            },
        ];

        let convention = discover_conventions_with_config(
            "Abilities",
            "abilities/*.php",
            &fingerprints,
            &AuditConfig::default(),
        )
        .unwrap();

        let mut expected_methods = convention.expected_methods.clone();
        expected_methods.sort();
        assert_eq!(expected_methods, vec!["execute", "register"]);
        assert_eq!(convention.conforming.len(), 2);
        assert!(convention.outliers.is_empty());
    }

    #[test]
    fn test_check_signature_consistency() {
        let mut conventions = vec![Convention {
            name: "No Methods".to_string(),
            glob: "src/*.rs".to_string(),
            expected_methods: Vec::new(),
            expected_registrations: Vec::new(),
            expected_interfaces: Vec::new(),
            expected_namespace: None,
            expected_imports: Vec::new(),
            conforming: vec!["src/a.rs".to_string()],
            outliers: Vec::new(),
            total_files: 1,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, Path::new("."), &AuditConfig::default());

        assert!(conventions[0].outliers.is_empty());
    }

    #[test]
    fn test_member_requirement_deviation() {
        let deviation = member_requirement_deviation(
            AuditFinding::MissingMethod,
            "Missing method",
            "Add",
            "execute",
            "()",
            "Abilities",
        );

        assert_eq!(deviation.kind, AuditFinding::MissingMethod);
        assert_eq!(deviation.description, "Missing method: execute");
        assert_eq!(
            deviation.suggestion,
            "Add execute() to match the convention in Abilities"
        );
    }

    #[test]
    fn utility_like_outlier_is_not_promoted_to_naming_mismatch() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/CreateAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("CreateAbility".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/UpdateAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("UpdateAbility".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/FlowHelpers.php".to_string(),
                language: Language::Php,
                methods: vec!["formatFlow".to_string()],
                type_name: Some("FlowHelpers".to_string()),
                ..Default::default()
            },
        ];

        let convention = discover_conventions_with_config(
            "Abilities",
            "abilities/*.php",
            &fingerprints,
            &framework_like_audit_config(),
        )
        .unwrap();

        assert!(
            convention.outliers.is_empty(),
            "recognized helper files are intentional utilities, got: {:?}",
            convention.outliers
        );
    }

    #[test]
    fn generic_scaffolding_suffixes_are_not_promoted_to_naming_mismatch() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/CreateAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("CreateAbility".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/UpdateAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("UpdateAbility".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/DeleteAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("DeleteAbility".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/WikiAbilityBase.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("WikiAbilityBase".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/WikiActionHandlers.php".to_string(),
                language: Language::Php,
                methods: vec!["register".to_string()],
                type_name: Some("WikiActionHandlers".to_string()),
                ..Default::default()
            },
        ];

        let convention =
            discover_conventions("Abilities", "abilities/*.php", &fingerprints).unwrap();

        assert!(
            convention.outliers.is_empty(),
            "generic scaffolding suffixes are utility-like, got: {:?}",
            convention.outliers
        );

        let projector = FileFingerprint {
            relative_path: "handlers/McpPacketProjector.php".to_string(),
            language: Language::Php,
            methods: vec!["project".to_string()],
            type_name: Some("McpPacketProjector".to_string()),
            ..Default::default()
        };

        assert!(is_utility_like_file(&projector, &AuditConfig::default()));
    }

    #[test]
    fn non_utility_helper_like_outlier_still_reports_naming_mismatch() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/CreateAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("CreateAbility".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/UpdateAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                type_name: Some("UpdateAbility".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/FlowThing.php".to_string(),
                language: Language::Php,
                methods: vec!["formatFlow".to_string()],
                type_name: Some("FlowThing".to_string()),
                ..Default::default()
            },
        ];

        let convention =
            discover_conventions("Abilities", "abilities/*.php", &fingerprints).unwrap();

        assert_eq!(convention.outliers.len(), 1);
        assert!(matches!(
            convention.outliers[0].deviations[0].kind,
            AuditFinding::NamingMismatch
        ));
    }

    #[test]
    fn opaque_convention_exception_globs_suppress_group_findings() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "src/items/one.item".to_string(),
                language: Language::Unknown,
                methods: vec!["run".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "src/items/two.item".to_string(),
                language: Language::Unknown,
                methods: vec!["run".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "src/generated/fixture.item".to_string(),
                language: Language::Unknown,
                methods: vec![],
                ..Default::default()
            },
        ];
        let audit_config = AuditConfig {
            convention_exception_globs: vec!["src/generated/*".to_string()],
            ..Default::default()
        };

        let convention =
            discover_conventions_with_config("Items", "src/items/*", &fingerprints, &audit_config)
                .unwrap();

        assert!(
            convention.outliers.is_empty(),
            "configured exception globs are opaque to core and should suppress convention deviations"
        );
    }

    #[test]
    fn declared_traits_do_not_become_missing_interfaces() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "chat/ListChatSessionsAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                type_name: Some("ListChatSessionsAbility".to_string()),
                implements: vec!["ChatSessionHelpers".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "chat/DeleteChatSessionAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                type_name: Some("DeleteChatSessionAbility".to_string()),
                implements: vec!["ChatSessionHelpers".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "chat/ChatSessionHelpers.php".to_string(),
                language: Language::Php,
                methods: vec!["verifySessionOwnership".to_string()],
                type_name: Some("ChatSessionHelpers".to_string()),
                content: "<?php\ntrait ChatSessionHelpers {}".to_string(),
                ..Default::default()
            },
        ];

        let convention = discover_conventions_with_config(
            "Chat",
            "chat/*.php",
            &fingerprints,
            &framework_like_audit_config(),
        )
        .unwrap();
        assert!(
            convention.expected_interfaces.is_empty(),
            "traits should not be treated as interfaces: {:?}",
            convention.expected_interfaces
        );
        assert!(
            convention
                .outliers
                .iter()
                .flat_map(|o| &o.deviations)
                .all(|d| d.kind != AuditFinding::MissingInterface),
            "declared trait should not produce MissingInterface deviations"
        );
    }

    #[test]
    fn utility_classes_do_not_need_dispatch_registration() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "endpoints/PostsEndpoint.php".to_string(),
                language: Language::Php,
                methods: vec!["register".to_string()],
                registrations: vec!["runtime_dispatch".to_string()],
                type_name: Some("PostsEndpoint".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "endpoints/PagesEndpoint.php".to_string(),
                language: Language::Php,
                methods: vec!["register".to_string()],
                registrations: vec!["runtime_dispatch".to_string()],
                type_name: Some("PagesEndpoint".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "endpoints/SignatureVerifier.php".to_string(),
                language: Language::Php,
                methods: vec!["verify".to_string()],
                type_name: Some("SignatureVerifier".to_string()),
                ..Default::default()
            },
        ];

        let convention = discover_conventions_with_config(
            "Api",
            "endpoints/*.php",
            &fingerprints,
            &framework_like_audit_config(),
        )
        .unwrap();
        assert!(
            convention
                .outliers
                .iter()
                .flat_map(|o| &o.deviations)
                .all(|d| d.kind != AuditFinding::MissingRegistration
                    && d.kind != AuditFinding::MissingMethod),
            "utility classes should not be treated as runtime endpoint registrants"
        );
    }

    #[test]
    fn typeless_files_do_not_need_type_subject_methods_or_registration() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/CreateAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                registrations: vec!["runtime_dispatch".to_string()],
                type_name: Some("CreateAbility".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/UpdateAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                registrations: vec!["runtime_dispatch".to_string()],
                type_name: Some("UpdateAbility".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/DeleteAbility.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string(), "register".to_string()],
                registrations: vec!["runtime_dispatch".to_string()],
                type_name: Some("DeleteAbility".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/bootstrap.php".to_string(),
                language: Language::Php,
                methods: vec![],
                type_name: None,
                type_names: vec![],
                ..Default::default()
            },
        ];

        let convention = discover_conventions_with_config(
            "Abilities",
            "abilities/*.php",
            &fingerprints,
            &AuditConfig::default(),
        )
        .unwrap();

        assert!(
            convention.outliers.is_empty(),
            "typeless composition files should not inherit type-subject conventions: {:?}",
            convention.outliers
        );
        assert!(
            !convention
                .conforming
                .contains(&"abilities/bootstrap.php".to_string()),
            "typeless composition files are skipped rather than counted as convention members"
        );
    }

    #[test]
    fn factories_do_not_need_methods_of_created_type() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "chat/DatabaseConversationStore.php".to_string(),
                language: Language::Php,
                methods: vec!["update_title".to_string(), "delete_session".to_string()],
                type_name: Some("DatabaseConversationStore".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "chat/MemoryConversationStore.php".to_string(),
                language: Language::Php,
                methods: vec!["update_title".to_string(), "delete_session".to_string()],
                type_name: Some("MemoryConversationStore".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "chat/ConversationStoreFactory.php".to_string(),
                language: Language::Php,
                methods: vec!["get".to_string()],
                type_name: Some("ConversationStoreFactory".to_string()),
                ..Default::default()
            },
        ];

        let convention = discover_conventions_with_config(
            "Chat",
            "chat/*.php",
            &fingerprints,
            &framework_like_audit_config(),
        )
        .unwrap();
        assert!(
            convention
                .outliers
                .iter()
                .flat_map(|o| &o.deviations)
                .all(|d| d.kind != AuditFinding::MissingMethod),
            "factories produce stores; they should not implement store methods"
        );
    }

    #[test]
    fn test_file_helper_functions_do_not_create_missing_method_conventions() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "tests/core/stack/apply_test.rs".to_string(),
                language: Language::Rust,
                methods: vec![
                    "run".to_string(),
                    "init_repo".to_string(),
                    "write_and_commit".to_string(),
                ],
                type_name: None,
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "tests/core/stack/status_test.rs".to_string(),
                language: Language::Rust,
                methods: vec![
                    "run".to_string(),
                    "init_repo".to_string(),
                    "write_and_commit".to_string(),
                ],
                type_name: None,
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "tests/core/stack/spec_test.rs".to_string(),
                language: Language::Rust,
                methods: vec!["spec_round_trips".to_string()],
                type_name: None,
                ..Default::default()
            },
        ];

        let convention = discover_conventions_with_config(
            "Stack (Tests)",
            "tests/core/stack/*_test.rs",
            &fingerprints,
            &AuditConfig::default(),
        );

        assert!(
            convention.is_none(),
            "test helper functions are local scaffolding, not methods every sibling test file must define"
        );
    }

    #[test]
    fn no_interface_convention_when_none_shared() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "a.php".to_string(),
                language: Language::Php,
                methods: vec!["run".to_string()],
                implements: vec!["FooInterface".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "b.php".to_string(),
                language: Language::Php,
                methods: vec!["run".to_string()],
                implements: vec!["BarInterface".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "c.php".to_string(),
                language: Language::Php,
                methods: vec!["run".to_string()],
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Mixed", "*.php", &fingerprints).unwrap();

        // No interface appears in ≥60% of files
        assert!(convention.expected_interfaces.is_empty());
    }

    // ========================================================================
    // Signature consistency tests
    // ========================================================================

    #[test]
    fn signature_check_detects_mismatch() {
        let _audit_guard = crate::test_support::AuditGuard::new();
        // Uses Rust files so the test works in CI (only rust extension/grammar installed).
        // When the grammar isn't discoverable (e.g. dev machine without the rust
        // extension installed), skip instead of failing the assertion downstream.
        require_rust_grammar!("signature_check_detects_mismatch");
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("handlers")).unwrap();

        // Two conforming files with matching signatures
        std::fs::write(
            dir.join("handlers/chat.rs"),
            "pub fn execute(config: &Config, context: &Context) -> Result<()> { Ok(()) }\npub fn register() {}\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("handlers/webhook.rs"),
            "pub fn execute(config: &Config, context: &Context) -> Result<()> { Ok(()) }\npub fn register() {}\n",
        )
        .unwrap();

        // One file with structurally different signature (different param count)
        std::fs::write(
            dir.join("handlers/ping.rs"),
            "pub fn execute(config: &Config) -> Result<()> { Ok(()) }\npub fn register() {}\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Handlers".to_string(),
            glob: "handlers/*".to_string(),
            expected_methods: vec!["execute".to_string(), "register".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec![
                "handlers/chat.rs".to_string(),
                "handlers/webhook.rs".to_string(),
                "handlers/ping.rs".to_string(),
            ],
            outliers: vec![],
            total_files: 3,
            confidence: 1.0,
        }];

        for _ in 0..5 {
            check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());
            if conventions[0]
                .outliers
                .iter()
                .flat_map(|outlier| outlier.deviations.iter())
                .any(|d| d.kind == AuditFinding::SignatureMismatch)
            {
                break;
            }
            std::thread::yield_now();
        }

        let conv = &conventions[0];
        // ping.rs should be moved to outliers
        assert_eq!(conv.conforming.len(), 2);
        assert_eq!(conv.outliers.len(), 1);
        assert_eq!(conv.outliers[0].file, "handlers/ping.rs");
        assert!(conv.outliers[0].deviations.iter().any(|d| {
            d.kind == AuditFinding::SignatureMismatch && d.description.contains("execute")
        }));
    }

    #[test]
    fn signature_check_adds_to_existing_outliers() {
        let _audit_guard = crate::test_support::AuditGuard::new();
        // Uses Rust files so the test works in CI (only rust extension/grammar installed).
        require_rust_grammar!("signature_check_adds_to_existing_outliers");
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("handlers")).unwrap();

        std::fs::write(
            dir.join("handlers/chat.rs"),
            "pub fn execute(config: &Config, context: &Context) -> Result<()> { Ok(()) }\npub fn register() {}\n",
        ).unwrap();

        std::fs::write(
            dir.join("handlers/webhook.rs"),
            "pub fn execute(config: &Config, context: &Context) -> Result<()> { Ok(()) }\npub fn register() {}\n",
        ).unwrap();

        // File already an outlier (missing register) AND has structurally different execute (1 param vs 2)
        std::fs::write(
            dir.join("handlers/bad.rs"),
            "pub fn execute(config: &Config) -> Result<()> { Ok(()) }\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Handlers".to_string(),
            glob: "handlers/*".to_string(),
            expected_methods: vec!["execute".to_string(), "register".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec![
                "handlers/chat.rs".to_string(),
                "handlers/webhook.rs".to_string(),
            ],
            outliers: vec![Outlier {
                file: "handlers/bad.rs".to_string(),
                noisy: false,
                deviations: vec![Deviation {
                    kind: AuditFinding::MissingMethod,
                    description: "Missing method: register".to_string(),
                    suggestion: "Add register()".to_string(),
                }],
            }],
            total_files: 3,
            confidence: 0.67,
        }];

        for _ in 0..5 {
            check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());
            if conventions[0].outliers[0]
                .deviations
                .iter()
                .any(|d| d.kind == AuditFinding::SignatureMismatch)
            {
                break;
            }
            std::thread::yield_now();
        }

        let conv = &conventions[0];
        assert_eq!(conv.conforming.len(), 2);
        assert_eq!(conv.outliers.len(), 1);
        // Should have BOTH the original MissingMethod AND the new SignatureMismatch
        assert!(conv.outliers[0].deviations.len() >= 2);
        assert!(conv.outliers[0]
            .deviations
            .iter()
            .any(|d| d.kind == AuditFinding::MissingMethod));
        assert!(conv.outliers[0]
            .deviations
            .iter()
            .any(|d| d.kind == AuditFinding::SignatureMismatch));
    }

    #[test]
    fn signature_check_no_change_when_all_match() {
        let _audit_guard = crate::test_support::AuditGuard::new();
        // Uses Rust files so the test works in CI (only rust extension/grammar installed).
        require_rust_grammar!("signature_check_no_change_when_all_match");
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("handlers")).unwrap();

        std::fs::write(
            dir.join("handlers/a.rs"),
            "pub fn execute(config: &Config) -> Vec<Item> { vec![] }\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("handlers/b.rs"),
            "pub fn execute(config: &Config) -> Vec<Item> { vec![] }\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Handlers".to_string(),
            glob: "handlers/*".to_string(),
            expected_methods: vec!["execute".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec!["handlers/a.rs".to_string(), "handlers/b.rs".to_string()],
            outliers: vec![],
            total_files: 2,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());

        let conv = &conventions[0];
        assert_eq!(conv.conforming.len(), 2);
        assert!(conv.outliers.is_empty());
        assert!((conv.confidence - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn signature_check_skips_convention_exception_files() {
        let _audit_guard = crate::test_support::AuditGuard::new();
        require_rust_grammar!("signature_check_skips_convention_exception_files");
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("handlers")).unwrap();

        std::fs::write(
            dir.join("handlers/a.rs"),
            "pub fn execute(config: &Config) -> Vec<Item> { vec![] }\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("handlers/b.rs"),
            "pub fn execute(config: &Config) -> Vec<Item> { vec![] }\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("handlers/register.rs"),
            "pub fn execute(config: &Config, context: &Context) -> Vec<Item> { vec![] }\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Handlers".to_string(),
            glob: "handlers/*".to_string(),
            expected_methods: vec!["execute".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec![
                "handlers/a.rs".to_string(),
                "handlers/b.rs".to_string(),
                "handlers/register.rs".to_string(),
            ],
            outliers: vec![],
            total_files: 3,
            confidence: 1.0,
        }];
        let audit_config = AuditConfig {
            convention_exception_globs: vec!["**/register.rs".to_string()],
            ..Default::default()
        };

        check_signature_consistency(&mut conventions, &dir, &audit_config);

        assert!(conventions[0].outliers.is_empty());
    }

    #[test]
    fn signature_check_skips_unknown_language() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("data")).unwrap();

        std::fs::write(dir.join("data/a.txt"), "some text\n").unwrap();
        std::fs::write(dir.join("data/b.txt"), "some text\n").unwrap();

        let mut conventions = vec![Convention {
            name: "Data".to_string(),
            glob: "data/*".to_string(),
            expected_methods: vec!["process".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec!["data/a.txt".to_string(), "data/b.txt".to_string()],
            outliers: vec![],
            total_files: 2,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());

        // Should not change anything for unknown language
        assert_eq!(conventions[0].conforming.len(), 2);
        assert!(conventions[0].outliers.is_empty());
    }

    #[test]
    fn signature_check_majority_wins() {
        let _audit_guard = crate::test_support::AuditGuard::new();
        // Uses Rust files so the test works in CI (only rust extension/grammar installed).
        // 2 files have one signature (2 params), 1 file has another (1 param) — the 2-file version is canonical
        require_rust_grammar!("signature_check_majority_wins");
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("handlers")).unwrap();

        std::fs::write(
            dir.join("handlers/a.rs"),
            "pub fn run(input: &Input, context: &Context) -> bool { true }\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("handlers/b.rs"),
            "pub fn run(input: &Input, context: &Context) -> bool { true }\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("handlers/c.rs"),
            "pub fn run(input: &Input) -> bool { true }\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Handlers".to_string(),
            glob: "handlers/*".to_string(),
            expected_methods: vec!["run".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec![
                "handlers/a.rs".to_string(),
                "handlers/b.rs".to_string(),
                "handlers/c.rs".to_string(),
            ],
            outliers: vec![],
            total_files: 3,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());

        let conv = &conventions[0];
        assert_eq!(conv.conforming.len(), 2);
        assert_eq!(conv.outliers.len(), 1);
        assert_eq!(conv.outliers[0].file, "handlers/c.rs");
    }

    #[test]
    fn signature_check_skips_ambiguous_tie() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("undo")).unwrap();

        std::fs::write(
            dir.join("undo/snapshot.rs"),
            "pub fn new(root: &Path, label: &str) -> Self { Self {} }\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("undo/rollback.rs"),
            "pub fn new() -> Self { Self {} }\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Undo".to_string(),
            glob: "undo/*".to_string(),
            expected_methods: vec!["new".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec![
                "undo/snapshot.rs".to_string(),
                "undo/rollback.rs".to_string(),
            ],
            outliers: vec![],
            total_files: 2,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());

        let conv = &conventions[0];
        assert_eq!(conv.conforming.len(), 2);
        assert!(conv.outliers.is_empty());
    }

    #[test]
    fn return_type_difference_not_a_mismatch() {
        // Files with and without return types should NOT produce a SignatureMismatch.
        // Uses Rust files so the test works in CI.
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::create_dir_all(dir.join("api")).unwrap();

        std::fs::write(
            dir.join("api/users.rs"),
            "pub fn register() -> Result<()> { Ok(()) }\npub fn check(request: &Request) {}\n",
        )
        .unwrap();

        std::fs::write(
            dir.join("api/posts.rs"),
            "pub fn register() {}\npub fn check(request: &Request) {}\n",
        )
        .unwrap();

        let mut conventions = vec![Convention {
            name: "Api".to_string(),
            glob: "api/*".to_string(),
            expected_methods: vec!["register".to_string(), "check".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec!["api/users.rs".to_string(), "api/posts.rs".to_string()],
            outliers: vec![],
            total_files: 2,
            confidence: 1.0,
        }];

        check_signature_consistency(&mut conventions, &dir, &AuditConfig::default());

        let conv = &conventions[0];
        // Both files should remain conforming — return type is not structural
        assert_eq!(
            conv.conforming.len(),
            2,
            "Return type difference should not cause mismatch"
        );
        assert!(
            conv.outliers.is_empty(),
            "No outliers expected for return type differences"
        );
    }

    #[test]
    fn namespace_mismatch_detected_in_convention() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/CreateFlow.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                type_name: Some("CreateFlow".to_string()),
                namespace: Some("SamplePlugin\\Abilities\\Flow".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/UpdateFlow.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                type_name: Some("UpdateFlow".to_string()),
                namespace: Some("SamplePlugin\\Abilities\\Flow".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/DeleteFlow.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                type_name: Some("DeleteFlow".to_string()),
                namespace: Some("SamplePlugin\\Flow".to_string()), // WRONG namespace
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Flow", "abilities/*", &fingerprints).unwrap();

        assert_eq!(
            convention.expected_namespace,
            Some("SamplePlugin\\Abilities\\Flow".to_string())
        );
        assert_eq!(convention.conforming.len(), 2);
        assert_eq!(convention.outliers.len(), 1);
        assert_eq!(convention.outliers[0].file, "abilities/DeleteFlow.php");
        assert!(convention.outliers[0]
            .deviations
            .iter()
            .any(|d| { d.kind == AuditFinding::NamespaceMismatch }));
    }

    #[test]
    fn missing_import_not_flagged_for_same_namespace_reference() {
        // Regression test for #1135 (case 2).
        //
        // Two classes in the same namespace don't need `use` statements to
        // reference each other. PHP resolves unqualified same-namespace
        // references automatically.
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/AgentTokenAbilities.php".to_string(),
                language: Language::Php,
                methods: vec!["register".to_string()],
                type_name: Some("AgentTokenAbilities".to_string()),
                namespace: Some("SamplePlugin\\Abilities".to_string()),
                // Imports PermissionHelper via fully-qualified name in the import list
                // in most files, but THIS file relies on same-namespace resolution.
                imports: vec![],
                content: "namespace SamplePlugin\\Abilities;\n\nclass AgentTokenAbilities {\n    public function register() { PermissionHelper::can_manage(); }\n}".to_string(),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/FlowAbilities.php".to_string(),
                language: Language::Php,
                methods: vec!["register".to_string()],
                type_name: Some("FlowAbilities".to_string()),
                namespace: Some("SamplePlugin\\Abilities".to_string()),
                imports: vec!["SamplePlugin\\Abilities\\PermissionHelper".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/JobAbilities.php".to_string(),
                language: Language::Php,
                methods: vec!["register".to_string()],
                type_name: Some("JobAbilities".to_string()),
                namespace: Some("SamplePlugin\\Abilities".to_string()),
                imports: vec!["SamplePlugin\\Abilities\\PermissionHelper".to_string()],
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Abilities", "abilities/*", &fingerprints).unwrap();

        assert!(convention
            .expected_imports
            .contains(&"SamplePlugin\\Abilities\\PermissionHelper".to_string()));

        // AgentTokenAbilities references PermissionHelper (same namespace) —
        // it should NOT be flagged as a missing import.
        let agent_outlier = convention
            .outliers
            .iter()
            .find(|o| o.file == "abilities/AgentTokenAbilities.php");

        if let Some(outlier) = agent_outlier {
            assert!(
                !outlier.deviations.iter().any(|d| {
                    d.kind == AuditFinding::MissingImport
                        && d.description.contains("PermissionHelper")
                }),
                "Same-namespace reference should not be flagged as missing import. Got: {:?}",
                outlier.deviations
            );
        }
    }

    #[test]
    fn missing_import_not_flagged_for_self_import() {
        // Regression test for #1135 (case 1).
        //
        // A file that *defines* class Foo in namespace X\Y should never be
        // flagged as needing `use X\Y\Foo;` — that's a self-import.
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/PermissionHelper.php".to_string(),
                language: Language::Php,
                methods: vec!["can_manage".to_string()],
                type_name: Some("PermissionHelper".to_string()),
                type_names: vec!["PermissionHelper".to_string()],
                namespace: Some("SamplePlugin\\Abilities".to_string()),
                // File defines the class; its convention peers might import it,
                // but self-import is nonsensical.
                imports: vec![],
                content: "namespace SamplePlugin\\Abilities;\n\nclass PermissionHelper { public function can_manage() {} }".to_string(),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/FlowAbilities.php".to_string(),
                language: Language::Php,
                methods: vec!["can_manage".to_string()],
                type_name: Some("FlowAbilities".to_string()),
                namespace: Some("SamplePlugin\\Abilities".to_string()),
                imports: vec!["SamplePlugin\\Abilities\\PermissionHelper".to_string()],
                content: "use SamplePlugin\\Abilities\\PermissionHelper;".to_string(),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/JobAbilities.php".to_string(),
                language: Language::Php,
                methods: vec!["can_manage".to_string()],
                type_name: Some("JobAbilities".to_string()),
                namespace: Some("SamplePlugin\\Abilities".to_string()),
                imports: vec!["SamplePlugin\\Abilities\\PermissionHelper".to_string()],
                content: "use SamplePlugin\\Abilities\\PermissionHelper;".to_string(),
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Abilities", "abilities/*", &fingerprints).unwrap();

        let helper_outlier = convention
            .outliers
            .iter()
            .find(|o| o.file == "abilities/PermissionHelper.php");

        if let Some(outlier) = helper_outlier {
            assert!(
                !outlier.deviations.iter().any(|d| {
                    d.kind == AuditFinding::MissingImport
                        && d.description.contains("PermissionHelper")
                }),
                "Self-import should not be flagged. Got deviations: {:?}",
                outlier.deviations
            );
        }
    }

    #[test]
    fn missing_import_not_flagged_when_terminal_only_appears_in_namespace() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "core/agents/AgentBundler.php".to_string(),
                language: Language::Php,
                methods: vec!["__construct".to_string()],
                type_name: Some("AgentBundler".to_string()),
                namespace: Some("SamplePlugin\\Core\\Agents".to_string()),
                imports: vec!["SamplePlugin\\Core\\Database\\Agents\\Agents".to_string()],
                content: "namespace SamplePlugin\\Core\\Agents;\nuse SamplePlugin\\Core\\Database\\Agents\\Agents;\nclass AgentBundler { public function __construct() { new Agents(); } }".to_string(),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "core/agents/AgentIdentityResolver.php".to_string(),
                language: Language::Php,
                methods: vec!["__construct".to_string()],
                type_name: Some("AgentIdentityResolver".to_string()),
                namespace: Some("SamplePlugin\\Core\\Agents".to_string()),
                imports: vec!["SamplePlugin\\Core\\Database\\Agents\\Agents".to_string()],
                content: "namespace SamplePlugin\\Core\\Agents;\nuse SamplePlugin\\Core\\Database\\Agents\\Agents;\nclass AgentIdentityResolver { public function __construct() { new Agents(); } }".to_string(),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "core/agents/AgentIdentity.php".to_string(),
                language: Language::Php,
                methods: vec!["__construct".to_string()],
                type_name: Some("AgentIdentity".to_string()),
                namespace: Some("SamplePlugin\\Core\\Agents".to_string()),
                imports: vec![],
                content: "namespace SamplePlugin\\Core\\Agents;\nclass AgentIdentity { public function __construct() {} }".to_string(),
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Agents", "core/agents/*", &fingerprints).unwrap();

        assert!(convention
            .expected_imports
            .contains(&"SamplePlugin\\Core\\Database\\Agents\\Agents".to_string()));

        let identity_outlier = convention
            .outliers
            .iter()
            .find(|o| o.file == "core/agents/AgentIdentity.php");

        if let Some(outlier) = identity_outlier {
            assert!(
                !outlier.deviations.iter().any(|d| {
                    d.kind == AuditFinding::MissingImport && d.description.contains("Agents")
                }),
                "Namespace-only terminal segment should not be flagged as missing import. Got deviations: {:?}",
                outlier.deviations
            );
        }
    }

    #[test]
    fn missing_import_detected_in_convention() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "abilities/A.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                imports: vec!["SamplePlugin\\Core\\Base".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/B.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                imports: vec!["SamplePlugin\\Core\\Base".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "abilities/C.php".to_string(),
                language: Language::Php,
                methods: vec!["execute".to_string()],
                // File uses Base but doesn't import it
                content: "class C extends Base {\n    public function execute() {}\n}".to_string(),
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Abilities", "abilities/*", &fingerprints).unwrap();

        assert!(convention
            .expected_imports
            .contains(&"SamplePlugin\\Core\\Base".to_string()));
        assert_eq!(convention.outliers.len(), 1);
        assert!(convention.outliers[0]
            .deviations
            .iter()
            .any(|d| { d.kind == AuditFinding::MissingImport }));
    }

    #[test]
    fn missing_namespace_detected() {
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "steps/A.php".to_string(),
                language: Language::Php,
                methods: vec!["run".to_string()],
                namespace: Some("App\\Steps".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "steps/B.php".to_string(),
                language: Language::Php,
                methods: vec!["run".to_string()],
                namespace: Some("App\\Steps".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "steps/C.php".to_string(),
                language: Language::Php,
                methods: vec!["run".to_string()],
                // Missing namespace entirely
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Steps", "steps/*", &fingerprints).unwrap();

        assert_eq!(
            convention.expected_namespace,
            Some("App\\Steps".to_string())
        );
        assert_eq!(convention.outliers.len(), 1);
        assert!(convention.outliers[0].deviations.iter().any(|d| {
            d.kind == AuditFinding::NamespaceMismatch && d.description.contains("Missing namespace")
        }));
    }

    // ========================================================================
    // has_import tests
    // ========================================================================

    // ========================================================================
    // type_names tests (issue #554)
    // ========================================================================

    #[test]
    fn no_naming_mismatch_when_type_names_includes_matching_type() {
        // Reproduces issue #554: version.rs has type_name=VersionOutput (first pub type)
        // but also has VersionArgs which matches the convention. Should NOT flag.
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "commands/deploy.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("DeployArgs".to_string()),
                type_names: vec!["DeployArgs".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "commands/lint.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("LintArgs".to_string()),
                type_names: vec!["LintArgs".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "commands/version.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                // Primary type is VersionOutput (first pub type in file)
                type_name: Some("VersionOutput".to_string()),
                // But file also contains VersionArgs
                type_names: vec!["VersionOutput".to_string(), "VersionArgs".to_string()],
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Commands", "commands/*.rs", &fingerprints).unwrap();

        // version.rs should NOT be an outlier because it has VersionArgs in type_names
        assert_eq!(
            convention.outliers.len(),
            0,
            "File with matching type in type_names should not be flagged"
        );
        assert_eq!(convention.conforming.len(), 3);
    }

    #[test]
    fn naming_mismatch_when_no_type_names_match() {
        // When type_names is populated but none match the convention, still flag it
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "commands/deploy.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("DeployArgs".to_string()),
                type_names: vec!["DeployArgs".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "commands/lint.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("LintArgs".to_string()),
                type_names: vec!["LintArgs".to_string()],
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "commands/utils.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("HelperUtils".to_string()),
                // No type matches Args convention
                type_names: vec!["HelperUtils".to_string(), "FormatConfig".to_string()],
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Commands", "commands/*.rs", &fingerprints).unwrap();

        // utils.rs should be an outlier — no type in type_names matches the Args convention
        assert_eq!(convention.outliers.len(), 1);
        assert_eq!(convention.outliers[0].file, "commands/utils.rs");
        assert!(convention.outliers[0]
            .deviations
            .iter()
            .any(|d| matches!(d.kind, AuditFinding::NamingMismatch)));
    }

    #[test]
    fn type_names_fallback_to_type_name_when_empty() {
        // When type_names is not populated (legacy extensions), fall back to type_name
        let fingerprints = vec![
            FileFingerprint {
                relative_path: "commands/deploy.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("DeployArgs".to_string()),
                // type_names empty — simulates old extension
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "commands/lint.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("LintArgs".to_string()),
                ..Default::default()
            },
            FileFingerprint {
                relative_path: "commands/utils.rs".to_string(),
                language: Language::Rust,
                methods: vec!["run".to_string()],
                type_name: Some("HelperUtils".to_string()),
                ..Default::default()
            },
        ];

        let convention = discover_conventions("Commands", "commands/*.rs", &fingerprints).unwrap();

        // utils.rs should be flagged via fallback to type_name
        assert_eq!(convention.outliers.len(), 1);
        assert_eq!(convention.outliers[0].file, "commands/utils.rs");
    }
}
