//! Source language primitive.
//!
//! `Language` is the framework-agnostic classification of a source file
//! (Rust / PHP / JavaScript / TypeScript / Unknown) plus the builtin token
//! tables and per-language behavioral predicates that many subsystems need.
//!
//! It lives in `homeboy-engine-primitives` because it is a foundational
//! primitive with zero dependencies on higher layers (audit, refactor,
//! component). The audit, refactor, and fixer layers all classify source
//! files, so this type sits below them in the dependency graph. Core re-exports
//! it as `crate::engine::language`, and `code_audit::conventions` re-exports
//! `Language` for backward compatibility.

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
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

    /// Method names that are universally idiomatic-shape across the ecosystems
    /// Homeboy can classify — stdlib/trait methods, common conversions and
    /// accessors, builder/serde hooks, and framework lifecycle/magic methods.
    ///
    /// These names are *expected* to carry boilerplate-shaped bodies across
    /// unrelated types (e.g. every collection wrapper defines the same
    /// `len`/`is_empty`), so coverage and duplication detectors treat them as
    /// idiomatic rather than as gaps or smells. The concrete ecosystem literals
    /// live here in the agnostic conventions home so detector implementations
    /// under `code_audit::detectors` stay free of hardcoded language names;
    /// components that opt into builtin defaults inherit this set and others
    /// declare their own via `TestMappingConfig`.
    pub fn builtin_trivial_method_names() -> &'static [&'static str] {
        &[
            // Core trait methods
            "new",
            "default",
            "from",
            "into",
            "clone",
            "fmt",
            "display",
            "eq",
            "hash",
            "drop",
            // Common conversions
            "as_str",
            "as_ref",
            "as_mut",
            "to_string",
            "to_str",
            "to_owned",
            // Common accessors
            "is_empty",
            "len",
            "iter",
            // Serialization hooks
            "serialize",
            "deserialize",
            // Builder pattern
            "build",
            "builder",
            // Magic / constructor methods
            "__construct",
            "__destruct",
            "__toString",
            "__clone",
            "get_instance",
            "getInstance",
            // Test lifecycle methods (optional base-class overrides — not every
            // test class needs to define them).
            "set_up",
            "tear_down",
            "set_up_before_class",
            "tear_down_after_class",
            "setUp",
            "tearDown",
            "setUpBeforeClass",
            "tearDownAfterClass",
        ]
    }

    /// Method-name prefixes that mark a method as a simple getter / predicate
    /// (e.g. `get_`, `is_`, `has_`). Like [`Self::builtin_trivial_method_names`],
    /// these are kept in the agnostic conventions home so detectors do not bake
    /// in language-shaped accessor conventions. Components that opt into builtin
    /// defaults inherit this set; others declare their own.
    pub fn builtin_trivial_method_prefixes() -> &'static [&'static str] {
        &["get_", "is_", "has_"]
    }

    /// Whether this language's only declaration visibility is "public" — i.e. it
    /// has no narrower-than-public visibility modifier (no `pub(crate)` / module
    /// scoping). For such languages a top-level/public symbol called from
    /// anywhere in its own file IS genuinely referenced, so the dead-code
    /// detector must not suggest narrowing its visibility. Languages that *do*
    /// support visibility narrowing (e.g. module-scoped `pub(...)`) return
    /// `false`, because a self-only public symbol there is actionable dead code.
    ///
    /// Keeping this classification in the agnostic conventions home lets the
    /// dead-code detector under `code_audit` stay free of hardcoded ecosystem
    /// names.
    pub fn lacks_visibility_narrowing(&self) -> bool {
        matches!(
            self,
            Language::Php | Language::JavaScript | Language::TypeScript
        )
    }

    /// Whether this language dispatches methods through the type system (trait /
    /// interface implementations invoked by the compiler rather than by explicit
    /// call sites). Detectors treat such methods as entry points because they
    /// are reachable even with no direct caller in source.
    pub fn has_typesystem_trait_dispatch(&self) -> bool {
        matches!(self, Language::Rust)
    }

    /// Whether this language's runtime commonly dispatches lifecycle / magic /
    /// hook callbacks by convention (methods the framework invokes by name
    /// rather than by an explicit call site). Detectors treat such methods as
    /// entry points so convention-invoked callbacks are not flagged as dead.
    pub fn has_framework_lifecycle_dispatch(&self) -> bool {
        matches!(self, Language::Php)
    }

    /// Source markers that open an *inline* test region — a block embedded in a
    /// production file whose contents are test scaffolding (fixtures, in-file
    /// unit tests), e.g. Rust's `#[cfg(test)]` module attribute. Detectors that
    /// scan raw content brace-match the block following each marker so they can
    /// skip test-only literals/commands/duplicates that `is_test_path` (which
    /// only classifies whole test *files*) cannot see.
    ///
    /// Empty for languages whose test code lives exclusively in separate files
    /// (handled by [`Self::builtin_test_file_suffixes`]) rather than inline
    /// blocks — those need no in-file region stripping.
    ///
    /// The concrete syntax lives here in the agnostic language home so detector
    /// implementations under `code_audit::detectors` stay free of hardcoded
    /// language tokens.
    pub fn inline_test_region_markers(&self) -> &'static [&'static str] {
        match self {
            Language::Rust => &["#[cfg(test)]"],
            // PHP / JS / TS keep tests in separate files; no inline block marker.
            Language::Php | Language::JavaScript | Language::TypeScript | Language::Unknown => &[],
        }
    }

    /// The *reachability boundary* a source file belongs to, derived from its
    /// path — the unit across which a symbol must be publicly exported to be
    /// referenced. For Rust that is the Cargo crate (`crates/<crate>/…`).
    ///
    /// Returns `None` when the language has no path-derivable boundary (or the
    /// path does not match its layout). Detectors that attribute a caller's raw
    /// literal/command to a definition elsewhere use this together with
    /// [`Self::is_boundary_public_visibility`]: a cross-boundary attribution is
    /// only valid when the definition is boundary-public. `None` on either side
    /// means "same boundary" (conservative — do not suppress).
    ///
    /// Ecosystem-specific layout lives here in the agnostic language home so
    /// detectors carry no hardcoded path convention.
    pub fn module_boundary_of_path<'a>(&self, path: &'a str) -> Option<&'a str> {
        match self {
            Language::Rust => {
                let rest = path.strip_prefix("crates/")?;
                rest.split('/').next().filter(|s| !s.is_empty())
            }
            // PHP autoloads across the whole package; JS/TS resolve via module
            // paths. Neither has a simple path-derived reachability boundary
            // that maps to the crate model, so cross-boundary attribution is
            // not attempted (callers treat this as "same boundary").
            Language::Php | Language::JavaScript | Language::TypeScript | Language::Unknown => None,
        }
    }

    /// Whether a captured visibility prefix makes a symbol reachable *across* a
    /// [`Self::module_boundary_of_path`] boundary. For Rust only crate-wide
    /// `pub` qualifies (`pub(crate)`, `pub(super)`, `pub(in …)`, private do
    /// not). Languages without a crate-style boundary return `false` (their
    /// cross-boundary attribution is disabled by the `None` boundary above, so
    /// this is only consulted for Rust in practice).
    pub fn is_boundary_public_visibility(&self, visibility_prefix: &str) -> bool {
        match self {
            Language::Rust => visibility_prefix.trim() == "pub",
            Language::Php | Language::JavaScript | Language::TypeScript | Language::Unknown => {
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Language;

    #[test]
    fn rust_declares_the_cfg_test_inline_marker() {
        assert_eq!(
            Language::Rust.inline_test_region_markers(),
            &["#[cfg(test)]"]
        );
    }

    #[test]
    fn rust_module_boundary_is_the_cargo_crate() {
        assert_eq!(
            Language::Rust.module_boundary_of_path("crates/homeboy-core/src/lib.rs"),
            Some("homeboy-core")
        );
        // Non-workspace layout → no derivable boundary.
        assert_eq!(Language::Rust.module_boundary_of_path("src/lib.rs"), None);
    }

    #[test]
    fn non_rust_languages_have_no_path_boundary() {
        for lang in [Language::Php, Language::JavaScript, Language::TypeScript] {
            assert_eq!(
                lang.module_boundary_of_path("crates/x/src/lib.rs"),
                None,
                "{lang:?} has no crate-style path boundary"
            );
        }
    }

    #[test]
    fn rust_boundary_public_visibility_requires_bare_pub() {
        assert!(Language::Rust.is_boundary_public_visibility("pub "));
        assert!(!Language::Rust.is_boundary_public_visibility("pub(crate) "));
        assert!(!Language::Rust.is_boundary_public_visibility("pub(super) "));
        assert!(!Language::Rust.is_boundary_public_visibility(""));
        // Non-crate languages never report boundary-public (cross-boundary
        // attribution is disabled for them via the None boundary).
        assert!(!Language::Php.is_boundary_public_visibility("public"));
    }

    #[test]
    fn separate_file_test_languages_declare_no_inline_marker() {
        // PHP/JS/TS/Unknown keep tests in separate files — no inline block to
        // strip, so detectors get an empty marker set (and thus no regions).
        for lang in [
            Language::Php,
            Language::JavaScript,
            Language::TypeScript,
            Language::Unknown,
        ] {
            assert!(
                lang.inline_test_region_markers().is_empty(),
                "{lang:?} should declare no inline test-region marker"
            );
        }
    }
}
