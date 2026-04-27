const RUNTIME_HELPER_RS: &str = include_str!("../../../src/core/extension/runtime_helper.rs");
const RUNTIME_HELPER_TESTS_RS: &str =
    include_str!("../../../src/core/extension/runtime_helper/tests.rs");

#[test]
fn test_ensure_all_helpers() {
    assert!(
        RUNTIME_HELPER_RS.contains("legacy_fallback: true"),
        "bench helper entries should opt into legacy fallback writes"
    );
    assert!(
        RUNTIME_HELPER_RS.contains(".join(\".homeboy\").join(\"runtime\")"),
        "legacy fallback path should match extension runner fallback"
    );
    assert!(
        RUNTIME_HELPER_TESTS_RS.contains("ensure_all_helpers_writes_legacy_bench_fallbacks"),
        "unit tests should cover legacy bench helper materialization"
    );
}
