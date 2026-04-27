//! Shared `${...}` token expansion helpers.

/// Expand `${...}` tokens with a caller-provided vocabulary.
///
/// Unknown and unterminated tokens stay literal so the eventual path or
/// command failure still shows the user what token was unresolved.
pub(crate) fn expand_tokens(input: &str, resolve: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next();
            let mut token = String::new();
            let mut closed = false;
            for inner in chars.by_ref() {
                if inner == '}' {
                    closed = true;
                    break;
                }
                token.push(inner);
            }

            if !closed {
                out.push_str("${");
                out.push_str(&token);
                continue;
            }

            match resolve(&token) {
                Some(value) => out.push_str(&value),
                None => {
                    out.push_str("${");
                    out.push_str(&token);
                    out.push('}');
                }
            }
        } else {
            out.push(c);
        }
    }

    out
}

/// Expand `${...}` tokens, then apply shell-style `~` expansion.
pub(crate) fn expand_with_tilde(input: &str, resolve: impl Fn(&str) -> Option<String>) -> String {
    let substituted = expand_tokens(input, resolve);
    shellexpand::tilde(&substituted).into_owned()
}

#[cfg(test)]
#[path = "../../tests/core/expand_test.rs"]
mod expand_test;
