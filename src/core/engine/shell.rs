//! Shell escaping and quoting utilities.

fn escape_single_quote_content(value: &str) -> String {
    value.replace('\'', "'\\''")
}

pub fn quote_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }

    const SHELL_META: &[char] = &[
        ' ', '\t', '\n', '\'', '"', '\\', '$', '`', '!', '*', '?', '[', ']', '(', ')', '{', '}',
        '<', '>', '|', '&', ';', '#', '~',
    ];

    if !arg.contains(SHELL_META) {
        return arg.to_string();
    }

    format!("'{}'", escape_single_quote_content(arg))
}

pub fn quote_args(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn normalize_args(args: &[String]) -> Vec<String> {
    if args.len() != 1 || !args[0].contains(' ') {
        return args.to_vec();
    }
    split_respecting_quotes(&args[0])
}

fn split_respecting_quotes(input: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double_quote => in_single_quote = !in_single_quote,
            '"' if !in_single_quote => in_double_quote = !in_double_quote,
            ' ' | '\t' if !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    result.push(std::mem::take(&mut current));
                }
            }
            '\\' if in_double_quote => {
                if let Some(&next) = chars.peek() {
                    if matches!(next, '"' | '\\' | '$' | '`') {
                        chars.next();
                        current.push(next);
                    } else {
                        current.push(c);
                    }
                } else {
                    current.push(c);
                }
            }
            _ => current.push(c),
        }
    }

    if !current.is_empty() {
        result.push(current);
    }

    result
}

pub fn quote_path(path: &str) -> String {
    format!("'{}'", escape_single_quote_content(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quote_arg_while_let_some_c_chars_next() {
        let arg = "";
        let _result = quote_arg(&arg);
    }

    #[test]
    fn test_quote_arg_current_is_empty() {
        let arg = "";
        let _result = quote_arg(&arg);
    }

    #[test]
    fn test_quote_arg_has_expected_effects() {
        // Expected effects: mutation
        let arg = "";
        let _ = quote_arg(&arg);
    }

    #[test]
    fn test_quote_args_while_let_some_c_chars_next() {
        let args = Vec::new();
        let _result = quote_args(&args);
    }

    #[test]
    fn test_quote_args_current_is_empty() {
        let args = Vec::new();
        let _result = quote_args(&args);
    }

    #[test]
    fn test_quote_args_has_expected_effects() {
        // Expected effects: mutation
        let args = Vec::new();
        let _ = quote_args(&args);
    }

    #[test]
    fn test_normalize_args_while_let_some_c_chars_next() {
        let args = Vec::new();
        let result = normalize_args(&args);
        assert!(
            !result.is_empty(),
            "expected non-empty collection for: while let Some(c) = chars.next() {"
        );
    }

    #[test]
    fn test_normalize_args_current_is_empty() {
        let args = Vec::new();
        let result = normalize_args(&args);
        assert!(
            !result.is_empty(),
            "expected non-empty collection for: !current.is_empty()"
        );
    }

    #[test]
    fn test_normalize_args_has_expected_effects() {
        // Expected effects: mutation
        let args = Vec::new();
        let _ = normalize_args(&args);
    }
}
