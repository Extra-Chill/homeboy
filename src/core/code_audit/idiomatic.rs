//! Shared predicates for "this method name is idiomatic-shape across types."
//!
//! Some method names — `len`, `is_empty`, `iter`, `new`, `default`, `from`,
//! `into`, `clone`, `fmt`, `as_str`, `to_string`, etc. — are **expected** to
//! have boilerplate-shaped bodies across unrelated types. That's the language
//! and stdlib doing what they're designed to do, not a code-smell.
//!
//! Two audit detectors care about this from different angles:
//!
//! - **test_coverage**: don't expect a dedicated test for a method whose name
//!   is universally idiomatic. `len`/`is_empty`/`fmt` get tested transitively.
//! - **near_duplicate (duplication.rs)**: don't flag a method whose name is
//!   universally idiomatic — every collection wrapper in the Rust ecosystem
//!   defines `fn len(&self) -> usize { self.inner.len() }`, and Clippy's
//!   `len_without_is_empty` lint actually *requires* you to add `is_empty`
//!   alongside it. Treating these as duplication findings is a false positive.
//!
//! Lifted from `test_coverage.rs` so both detectors consult the same predicate.

/// Method names that are universally idiomatic-shape across types.
///
/// Returns true if the name is either:
/// - in a curated list of stdlib-trait / common-accessor / lifecycle method
///   names that are expected to look the same across unrelated types, or
/// - prefixed with `get_`, `is_`, or `has_` (simple getters / predicates).
pub(super) fn is_trivial_method(name: &str) -> bool {
    let trivial = [
        // Rust core trait methods
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
        // Rust common conversions
        "as_str",
        "as_ref",
        "as_mut",
        "to_string",
        "to_str",
        "to_owned",
        // Rust common accessors
        "is_empty",
        "len",
        "iter",
        // Serde
        "serialize",
        "deserialize",
        // Builder pattern
        "build",
        "builder",
        // PHP magic methods
        "__construct",
        "__destruct",
        "__toString",
        "__clone",
        "get_instance",
        "getInstance",
        // Test lifecycle methods (PHPUnit / WP_UnitTestCase)
        // These are optional overrides inherited from the base test class —
        // not every test class needs to define them.
        "set_up",
        "tear_down",
        "set_up_before_class",
        "tear_down_after_class",
        "setUp",
        "tearDown",
        "setUpBeforeClass",
        "tearDownAfterClass",
    ];
    if trivial.contains(&name) {
        return true;
    }
    // Prefix-based rules: simple getters/accessors/predicates
    if name.starts_with("get_") || name.starts_with("is_") || name.starts_with("has_") {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_trivial_method_recognizes_collection_idioms() {
        // The triggering case: standard collection-wrapper boilerplate.
        // Every Vec/HashMap/String wrapper in the ecosystem looks the same
        // for these names, and Clippy's `len_without_is_empty` lint requires
        // them paired.
        assert!(is_trivial_method("len"));
        assert!(is_trivial_method("is_empty"));
        assert!(is_trivial_method("iter"));
    }

    #[test]
    fn is_trivial_method_recognizes_prefix_rules() {
        // Simple getters / predicates / capability checks.
        assert!(is_trivial_method("get_foo"));
        assert!(is_trivial_method("is_bar"));
        assert!(is_trivial_method("has_baz"));
    }

    #[test]
    fn is_trivial_method_rejects_real_methods() {
        // Domain methods with substantive bodies should not be considered
        // trivial — they carry real logic that's worth testing and worth
        // flagging if duplicated.
        assert!(!is_trivial_method("compute_fixability"));
        assert!(!is_trivial_method("from_snapshot"));
    }

    #[test]
    fn is_trivial_method_recognizes_stdlib_trait_methods() {
        // Core trait methods on the curated list.
        assert!(is_trivial_method("new"));
        assert!(is_trivial_method("default"));
        assert!(is_trivial_method("from"));
        assert!(is_trivial_method("into"));
        assert!(is_trivial_method("clone"));
        assert!(is_trivial_method("fmt"));
    }

    #[test]
    fn is_trivial_method_recognizes_php_magic_methods() {
        assert!(is_trivial_method("__construct"));
        assert!(is_trivial_method("__toString"));
        assert!(is_trivial_method("getInstance"));
    }
}
