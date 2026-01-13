/// Escape a value for use inside single quotes.
/// Replaces `'` with `'\''` (end quote, escaped quote, start quote).
pub fn escape_single_quote_content(value: &str) -> String {
    value.replace('\'', "'\\''")
}

/// Quote a single argument for shell execution.
/// - Empty strings become `''`
/// - Strings with shell metacharacters are wrapped in single quotes
/// - Embedded single quotes are escaped
pub fn quote_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }

    // Characters that require quoting
    const SHELL_META: &[char] = &[
        ' ', '\t', '\n', '\'', '"', '\\', '$', '`', '!', '*', '?', '[', ']', '(', ')', '{', '}',
        '<', '>', '|', '&', ';', '#', '~',
    ];

    if !arg.contains(SHELL_META) {
        return arg.to_string();
    }

    format!("'{}'", escape_single_quote_content(arg))
}

/// Quote and join multiple arguments for shell execution.
pub fn quote_args(args: &[String]) -> String {
    args.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Quote a path for shell execution (always quotes).
pub fn quote_path(path: &str) -> String {
    format!("'{}'", escape_single_quote_content(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_arg_simple() {
        assert_eq!(quote_arg("version"), "version");
        assert_eq!(quote_arg("core"), "core");
    }

    #[test]
    fn quote_arg_with_spaces() {
        assert_eq!(quote_arg("hello world"), "'hello world'");
    }

    #[test]
    fn quote_arg_with_parens() {
        assert_eq!(
            quote_arg("var_export(wp_get_current_user()->ID);"),
            "'var_export(wp_get_current_user()->ID);'"
        );
    }

    #[test]
    fn quote_arg_with_single_quote() {
        assert_eq!(quote_arg("it's"), "'it'\\''s'");
    }

    #[test]
    fn quote_arg_empty() {
        assert_eq!(quote_arg(""), "''");
    }

    #[test]
    fn quote_args_mixed() {
        let args = vec!["eval".to_string(), "echo 'test';".to_string()];
        assert_eq!(quote_args(&args), "eval 'echo '\\''test'\\'';'");
    }

    #[test]
    fn quote_path_simple() {
        assert_eq!(quote_path("/var/www"), "'/var/www'");
    }

    #[test]
    fn quote_path_with_quote() {
        assert_eq!(quote_path("/var/www/it's"), "'/var/www/it'\\''s'");
    }
}
