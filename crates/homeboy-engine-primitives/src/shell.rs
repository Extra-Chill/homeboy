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

/// Shell preamble that normalizes `PATH` for a remote (SSH) command so common
/// user/tool bin directories and versioned Node installs are discoverable
/// before the dispatched command runs. Pure shell-string construction shared by
/// remote-dispatch callers; contains no runner-specific behavior.
pub fn remote_shell_path_preamble() -> &'static str {
    concat!(
        "export PATH=\"$HOME/.local/bin:$HOME/.",
        "car",
        "go/bin:$HOME/.kimaki/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin:${PATH:-}\"; ",
        "for d in \"$HOME\"/.local/opt/node-*/bin \"$HOME\"/.nvm/versions/node/*/bin; do ",
        "[ -d \"$d\" ] && PATH=\"$d:$PATH\"; done; export PATH"
    )
}

/// Quote an environment-variable value for inclusion in a remote `export`
/// statement. `PATH` is quoted with double quotes so `$PATH` expansion is
/// preserved; every other value is shell-quoted normally.
pub fn quote_runner_env_value(key: &str, value: &str) -> String {
    if key == "PATH" {
        return format!("\"{}\"", escape_double_quoted_env_value(value));
    }

    quote_arg(value)
}

fn escape_double_quoted_env_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
}

#[cfg(test)]
mod remote_env_tests {
    use super::*;

    #[test]
    fn remote_shell_path_preamble_includes_local_opt_node_glob() {
        let preamble = remote_shell_path_preamble();

        assert!(preamble.contains("$HOME/.local/bin"));
        assert!(preamble.contains("$HOME\"/.local/opt/node-*/bin"));
        assert!(preamble.contains("$HOME\"/.nvm/versions/node/*/bin"));
    }

    #[test]
    fn path_env_value_allows_existing_path_expansion() {
        assert_eq!(
            quote_runner_env_value("PATH", "$PATH:/custom/bin"),
            "\"$PATH:/custom/bin\""
        );
    }

    #[test]
    fn non_path_env_value_uses_shell_quoting() {
        assert_eq!(
            quote_runner_env_value("TOKEN", "hello world"),
            "'hello world'"
        );
    }
}
