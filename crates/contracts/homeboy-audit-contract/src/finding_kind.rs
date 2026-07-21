//! Audit finding kinds — the canonical vocabulary of what the audit engine can
//! report.
//!
//! `AuditFinding` is a pure, dependency-free enum (plus its snake_case
//! serde/`FromStr`/`all_names` helpers) naming every kind of finding: god-files,
//! duplicate functions, unused parameters, missing tests, doc drift, and so on.
//! It is consumed everywhere — the audit engine that produces findings, the
//! refactor engine that fixes them, the CLI that renders them — so it lives in
//! the shared audit contract rather than inside `code_audit`.

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
    /// Two functions with *different names* have identical normalized bodies —
    /// the same logic reimplemented under a local name (often a copy of an
    /// existing shared primitive). Name-keyed duplicate detection misses this.
    CrossNameDuplicate,
    /// Function has identical structure but different identifiers/literals.
    NearDuplicate,
    /// Two functions share an identical call/control-flow skeleton but differ
    /// in their error/return tail — the same primitive reimplemented with a
    /// different local error type or return wrapper. Coarser than
    /// `NearDuplicate` (which requires matching expression *shape*), this
    /// catches hand-rolled wrappers like per-module `git_output` helpers that
    /// map failures onto different error enums.
    SkeletonDuplicate,
    /// A raw string literal whose value is byte-identical to a named constant
    /// already defined elsewhere in the codebase — the constant was bypassed
    /// with a hand-typed copy. Editing the constant leaves these copies stale,
    /// so they should reference the constant instead.
    ConstantBypassLiteral,
    /// A raw subprocess call whose literal argument vector is byte-identical to
    /// the one wrapped by an existing thin helper — the command analog of
    /// `ConstantBypassLiteral`. The caller re-invoked the primitive raw instead
    /// of calling the helper (e.g. `git ["rev-parse","HEAD"]` when `head_sha`
    /// exists), so the command spelling can drift out of one place.
    CommandWrapperBypass,
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
    /// Test method's entire body is a call to another test in the same file,
    /// re-running that test for no added coverage — usually a leftover rename
    /// shim.
    RedundantTestWrapper,
    /// Test is disabled with a bare skip attribute that carries no reason,
    /// leaving no record of why it is skipped or when it should run again.
    IgnoredTestWithoutReason,
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
    /// Policy-bearing state is dropped by an aggregate projection and a
    /// downstream decision reimplements the authoritative policy seam.
    LossyPolicyProjection,
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
            "cross_name_duplicate",
            "near_duplicate",
            "skeleton_duplicate",
            "constant_bypass_literal",
            "command_wrapper_bypass",
            "unused_parameter",
            "ignored_parameter",
            "dead_code_marker",
            "unreferenced_export",
            "orphaned_internal",
            "missing_test_file",
            "missing_test_method",
            "orphaned_test",
            "vacuous_test",
            "redundant_test_wrapper",
            "ignored_test_without_reason",
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
            "lossy_policy_projection",
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
