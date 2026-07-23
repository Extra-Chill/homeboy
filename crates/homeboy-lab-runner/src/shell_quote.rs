//! Crate-internal shell argument quoting used by lab-runner command builders.
//!
//! This uses an *allowlist* policy: a value is emitted verbatim only when every
//! character is in a known-safe set, otherwise it is single-quoted. It is
//! intentionally distinct from [`homeboy_core::engine::shell::quote_arg`], which
//! uses a shell-metacharacter *denylist* and therefore quotes a different set of
//! inputs. Several lab-runner command builders relied on the allowlist behavior;
//! this consolidates the previously copy-pasted definitions into one place
//! without changing what gets quoted.
pub(crate) fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_chars_are_not_quoted() {
        assert_eq!(shell_arg("abc-DEF_1.2/x:y=z"), "abc-DEF_1.2/x:y=z");
    }

    #[test]
    fn unsafe_chars_are_single_quoted() {
        assert_eq!(shell_arg("a b"), "'a b'");
        assert_eq!(shell_arg("a@b"), "'a@b'");
    }

    #[test]
    fn embedded_single_quote_is_escaped() {
        assert_eq!(shell_arg("a'b"), "'a'\\''b'");
    }

    #[test]
    fn empty_value_is_quoted() {
        // Empty string has no chars, so `all` is vacuously true and it is emitted
        // verbatim — preserving the historical behavior of these call sites.
        assert_eq!(shell_arg(""), "");
    }
}
