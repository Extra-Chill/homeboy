//! Crate-internal shell argument quoting used by lab-runner command builders.
//!
//! This re-exports [`homeboy_engine_primitives::shell::shell_arg`], which uses an
//! *allowlist* policy: a value is emitted verbatim only when every character is
//! in a known-safe set (ASCII alphanumerics and `-_./:=@`), otherwise it is
//! single-quoted. An empty value renders as `''` so it is never silently dropped
//! from a rendered command. It is intentionally distinct from
//! [`homeboy_core::engine::shell::quote_arg`], which uses a shell-metacharacter
//! *denylist* and therefore quotes a different set of inputs.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_chars_are_not_quoted() {
        assert_eq!(shell_arg("abc-DEF_1.2/x:y=z"), "abc-DEF_1.2/x:y=z");
        assert_eq!(shell_arg("a@b"), "a@b");
    }

    #[test]
    fn unsafe_chars_are_single_quoted() {
        assert_eq!(shell_arg("a b"), "'a b'");
        assert_eq!(shell_arg("a$b"), "'a$b'");
    }

    #[test]
    fn embedded_single_quote_is_escaped() {
        assert_eq!(shell_arg("a'b"), "'a'\\''b'");
    }

    #[test]
    fn empty_value_is_quoted() {
        assert_eq!(shell_arg(""), "''");
    }
}

pub(crate) use homeboy_engine_primitives::shell::shell_arg;
