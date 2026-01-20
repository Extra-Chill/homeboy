//! Shell escaping and quoting utilities.

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

/// Escape an entire command string for sh -c execution.
/// Use this when passing a complete command (with operators) to sh -c.
/// Wraps entire command in single quotes and escapes embedded quotes.
pub fn escape_command_for_shell(command: &str) -> String {
    format!("'{}'", escape_single_quote_content(command))
}

/// Quote a path for shell execution (always quotes).
pub fn quote_path(path: &str) -> String {
    format!("'{}'", escape_single_quote_content(path))
}

/// Escape special characters for perl regex patterns.
/// Characters: \ ^ $ . | ? * + ( ) [ ] { } and the delimiter /
pub fn escape_perl_regex(pattern: &str) -> String {
    let mut escaped = String::new();
    for c in pattern.chars() {
        match c {
            '\\' | '^' | '$' | '.' | '|' | '?' | '*' | '+' | '(' | ')' | '[' | ']' | '{' | '}'
            | '/' => {
                escaped.push('\\');
                escaped.push(c);
            }
            _ => escaped.push(c),
        }
    }
    escaped
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

    #[test]
    fn escape_perl_regex_simple() {
        assert_eq!(escape_perl_regex("hello"), "hello");
        assert_eq!(escape_perl_regex("test123"), "test123");
    }

    #[test]
    fn escape_perl_regex_special_chars() {
        assert_eq!(escape_perl_regex("hello.world"), "hello\\.world");
        assert_eq!(escape_perl_regex("price$100"), "price\\$100");
        assert_eq!(escape_perl_regex("a|b|c"), "a\\|b\\|c");
        assert_eq!(escape_perl_regex("foo+"), "foo\\+");
        assert_eq!(escape_perl_regex("test*"), "test\\*");
    }

    #[test]
    fn escape_perl_regex_brackets() {
        assert_eq!(escape_perl_regex("[test]"), "\\[test\\]");
        assert_eq!(escape_perl_regex("func()"), "func\\(\\)");
    }

    #[test]
    fn escape_perl_regex_slash() {
        assert_eq!(escape_perl_regex("path/to/file"), "path\\/to\\/file");
        assert_eq!(escape_perl_regex("/var/www"), "\\/var\\/www");
    }
}
