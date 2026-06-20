use super::{is_mutating_method, require_apply_for_request};

#[test]
fn http_request_mutating_methods_require_apply() {
    for method in ["POST", "put", "PATCH", "DELETE"] {
        let err = require_apply_for_request(method, false, "https://example.test/api")
            .expect_err("mutating HTTP method should require --apply");

        assert!(err.message.contains("requires explicit --apply"));
        assert!(err
            .message
            .contains(&format!("homeboy http request {method} --apply")));
    }
}

#[test]
fn http_request_safe_methods_do_not_require_apply() {
    for method in ["GET", "head", "Options"] {
        assert!(!is_mutating_method(method));
        require_apply_for_request(method, false, "https://example.test/api")
            .expect("safe HTTP method should not require --apply");
    }
}

#[test]
fn http_request_applied_mutation_passes_guard() {
    assert!(is_mutating_method("POST"));
    require_apply_for_request("POST", true, "https://example.test/api")
        .expect("applied mutation should pass guard");
}
