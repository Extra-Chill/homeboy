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

/// Normalize argument list - if single arg contains spaces, split it while respecting quotes.
/// Handles both input styles for CLI tool commands:
/// - Multiple args: ["arg1", "arg2", "--flag"] -> unchanged
/// - Single quoted arg: ["arg1 arg2 --flag"] -> split to ["arg1", "arg2", "--flag"]
/// - Quoted content preserved: ["eval 'echo foo;'"] -> ["eval", "echo foo;"]
///
/// This provides a consistent experience for users who may quote arguments
/// in their shell vs. provide them as separate args.
pub fn normalize_args(args: &[String]) -> Vec<String> {
    if args.len() != 1 || !args[0].contains(' ') {
        return args.to_vec();
    }
    split_respecting_quotes(&args[0])
}

/// Split a string on whitespace while respecting single and double quotes.
/// Quotes are consumed (not included in output).
fn split_respecting_quotes(input: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut in_single_quote = false;
    let mut in_double_quote = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
                // Quote character consumed, not included in output
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                // Quote character consumed, not included in output
            }
            ' ' | '\t' if !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    result.push(std::mem::take(&mut current));
                }
            }
            '\\' if in_double_quote => {
                // Handle escape sequences in double quotes (bash semantics)
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

    #[test]
    fn normalize_args_multiple_args_unchanged() {
        let args = vec![
            "arg1".to_string(),
            "arg2".to_string(),
            "--flag".to_string(),
        ];
        assert_eq!(normalize_args(&args), args);
    }

    #[test]
    fn normalize_args_single_arg_with_spaces_splits() {
        let args = vec!["arg1 arg2 --flag".to_string()];
        assert_eq!(
            normalize_args(&args),
            vec!["arg1".to_string(), "arg2".to_string(), "--flag".to_string()]
        );
    }

    #[test]
    fn normalize_args_single_arg_no_spaces_unchanged() {
        let args = vec!["simple".to_string()];
        assert_eq!(normalize_args(&args), args);
    }

    #[test]
    fn normalize_args_empty_vec() {
        let args: Vec<String> = vec![];
        assert_eq!(normalize_args(&args), args);
    }

    #[test]
    fn normalize_args_quoted_and_unquoted_equivalent() {
        // Simulate what clap gives us for each syntax:
        // `homeboy wp proj post list`     → ["post", "list"]
        // `homeboy wp proj "post list"`   → ["post list"]

        let unquoted = vec!["post".to_string(), "list".to_string()];
        let quoted = vec!["post list".to_string()];

        let normalized_unquoted = normalize_args(&unquoted);
        let normalized_quoted = normalize_args(&quoted);

        assert_eq!(normalized_unquoted, normalized_quoted);
        assert_eq!(normalized_quoted, vec!["post", "list"]);
    }

    #[test]
    fn normalize_args_respects_single_quotes() {
        // `homeboy wp proj "eval 'echo foo;'"` → ["eval 'echo foo;'"]
        let args = vec!["eval 'echo foo;'".to_string()];
        assert_eq!(normalize_args(&args), vec!["eval", "echo foo;"]);
    }

    #[test]
    fn normalize_args_respects_double_quotes() {
        let args = vec!["eval \"echo foo;\"".to_string()];
        assert_eq!(normalize_args(&args), vec!["eval", "echo foo;"]);
    }

    #[test]
    fn normalize_args_preserves_backslashes_in_single_quotes() {
        // Single quotes preserve everything literally (no escape processing)
        let args = vec!["eval 'print_r(\\Namespace\\Class::method());'".to_string()];
        assert_eq!(
            normalize_args(&args),
            vec!["eval", "print_r(\\Namespace\\Class::method());"]
        );
    }

    #[test]
    fn normalize_args_mixed_content() {
        let args = vec!["cmd 'arg with spaces' --flag value".to_string()];
        assert_eq!(
            normalize_args(&args),
            vec!["cmd", "arg with spaces", "--flag", "value"]
        );
    }

    #[test]
    fn normalize_args_wp_eval_php_code() {
        // Real-world use case: WP-CLI eval with PHP code
        let args = vec!["eval 'echo json_encode(get_option(\"blogname\"));'".to_string()];
        assert_eq!(
            normalize_args(&args),
            vec!["eval", "echo json_encode(get_option(\"blogname\"));"]
        );
    }

    #[test]
    fn normalize_args_nested_quotes() {
        // Double quotes inside single quotes are literal
        let args = vec!["cmd 'say \"hello\"'".to_string()];
        assert_eq!(normalize_args(&args), vec!["cmd", "say \"hello\""]);
    }

    #[test]
    fn normalize_args_double_quote_escapes() {
        // Within double quotes, backslash-escaped chars are processed
        let args = vec!["cmd \"path\\\\to\\\\file\"".to_string()];
        assert_eq!(normalize_args(&args), vec!["cmd", "path\\to\\file"]);
    }
}
