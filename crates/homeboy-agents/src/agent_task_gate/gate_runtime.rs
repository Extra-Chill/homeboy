//! Mutation-free executable discovery for declared shell gates.
use super::*;

/// Only literal bare command names resolved through PATH are inferred. Quoted
/// separators are arguments, while pipelines, lists and newlines start another
/// simple command. Path-style commands (`./scripts/check.sh`) are left to the
/// gate itself: the candidate may create them.
pub(super) fn executables(program: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut word = String::new();
    let mut words = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;
    let mut chars = program.chars().chain(std::iter::once('\n')).peekable();
    while let Some(ch) = chars.next() {
        if comment {
            if ch != '\n' {
                continue;
            }
            comment = false;
        }
        if ch == '#' && quote.is_none() && word.is_empty() {
            comment = true;
            continue;
        }
        if escaped {
            word.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                word.push(ch);
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            continue;
        }
        // `$(...)` and `$((...))` stay inside the current word: arithmetic
        // operands are never commands, and a substitution's commands run
        // under the gate itself rather than as literal PATH lookups.
        if ch == '$' && chars.peek() == Some(&'(') {
            word.push(ch);
            consume_parenthesized(&mut chars, &mut word);
            continue;
        }
        // A `(( expr ))` arithmetic command starts a command with `((`.
        if ch == '(' && word.is_empty() && words.is_empty() && chars.peek() == Some(&'(') {
            let mut arithmetic = String::from("(");
            consume_parenthesized(&mut chars, &mut arithmetic);
            words.push(arithmetic);
            continue;
        }
        // `2>&1`, `>&2` and `<&0` duplicate a descriptor, and `&>`/`&>>`
        // redirect both streams: those `&`s are redirect syntax, not lists.
        if ch == '&' && (word.ends_with(['>', '<']) || chars.peek() == Some(&'>')) {
            word.push(ch);
            continue;
        }
        if ch.is_whitespace() || ";|&()".contains(ch) {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            if ch == '\n' || ";|&()".contains(ch) {
                if let Some(command) = command_word(&words) {
                    if !builtin(command)
                        && !command.contains(['$', '`', '*', '?', '/', '('])
                        && !commands.contains(command)
                    {
                        commands.push(command.clone());
                    }
                }
                words.clear();
            }
        } else {
            word.push(ch);
        }
    }
    commands
}

/// Consume a balanced `( ... )` group (the opening `(` is still pending) into
/// `word`, honouring nested parentheses and quotes.
fn consume_parenthesized(
    chars: &mut std::iter::Peekable<impl Iterator<Item = char>>,
    word: &mut String,
) {
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for ch in chars.by_ref() {
        word.push(ch);
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' => quote = Some(ch),
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return;
                }
            }
            _ => {}
        }
    }
}

/// The first word that is neither an assignment nor a redirection. A bare
/// redirect operator (`>`, `2>`, `&>`) takes the next word as its target.
fn command_word(words: &[String]) -> Option<&String> {
    let mut words = words.iter();
    while let Some(word) = words.next() {
        if let Some(operator) = redirection(word) {
            if operator.len() == word.len() {
                words.next();
            }
            continue;
        }
        if !assignment(word) {
            return Some(word);
        }
    }
    None
}

/// The redirect operator a word starts with: optional descriptor digits, then
/// `<`/`>` runs and an optional `&`, or `&>`/`&>>`.
fn redirection(word: &str) -> Option<&str> {
    let rest = word.trim_start_matches(|ch: char| ch.is_ascii_digit());
    let start = word.len() - rest.len();
    let operator = if let Some(after) = rest.strip_prefix("&>") {
        2 + after.len() - after.trim_start_matches('>').len()
    } else if rest.starts_with(['<', '>']) {
        let after = rest.trim_start_matches(['<', '>']);
        let mut len = rest.len() - after.len();
        if after.starts_with('&') {
            len += 1;
        }
        len
    } else {
        return None;
    };
    Some(&word[..start + operator])
}

fn assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().enumerate().all(|(i, ch)| {
                ch == '_' || ch.is_ascii_alphabetic() || (i > 0 && ch.is_ascii_digit())
            })
    })
}
fn builtin(word: &str) -> bool {
    matches!(
        word,
        ":" | "."
            | "true"
            | "false"
            | "echo"
            | "printf"
            | "cd"
            | "pwd"
            | "export"
            | "unset"
            | "set"
            | "test"
            | "["
            | "exit"
            | "return"
            | "read"
            | "shift"
            | "umask"
            | "source"
            | "alias"
            | "unalias"
            | "command"
            | "type"
            | "eval"
            | "exec"
            | "wait"
            | "kill"
            | "trap"
            | "jobs"
            | "local"
            | "declare"
            | "let"
            | "if"
            | "then"
            | "else"
            | "fi"
            | "for"
            | "do"
            | "done"
            | "elif"
            | "in"
            | "while"
            | "until"
            | "select"
            | "function"
            | "time"
            | "continue"
            | "break"
            | "getopts"
            | "hash"
            | "ulimit"
            | "times"
            | "readonly"
            | "[["
            | "]]"
            | "case"
            | "esac"
            | "!"
            | "{"
            | "}"
    )
}

pub(super) fn unavailable(gate: &str, executable: &str, runner: &str) -> Error {
    let action = format!("Install `{executable}` on runner `{runner}` and expose it on the gate PATH, or select a runner that provides it, then retry Cook.");
    Error::new(homeboy_core::ErrorCode::GateRuntimeUnavailable,
        format!("gate `{gate}` requires unavailable executable `{executable}` on runner `{runner}`. {action}"),
        json!({"gate":gate,"executable":executable,"runner":runner,"next_action":action}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gate_runtime_parser_table() {
        for (program, expected) in [
            ("php -v", vec!["php"]),
            (
                "A=1 B=two php -v && composer --version",
                vec!["php", "composer"],
            ),
            ("cd src; php -v | grep version", vec!["php", "grep"]),
            ("printf 'php; fake'\nnode --version", vec!["node"]),
            ("A=1; true && echo ok", vec![]),
            ("./scripts/check.sh && vendor/bin/phpunit", vec![]),
            ("$RUNTIME -v", vec![]),
            ("# a gate\nphp -v # explanation", vec!["php"]),
            (
                "bash \"$IMPORT_LOOP_GATE\" >\"$acceptance\" 2>&1 & gate=$!; wait $gate",
                vec!["bash"],
            ),
            ("cmd &>log; grep x log", vec!["cmd", "grep"]),
            ("node a.mjs >&2 2>/dev/null", vec!["node"]),
            ("> out php -v; 2>err <in node x", vec!["php", "node"]),
            ("php -v & node x", vec!["php", "node"]),
            // #18253: shell keywords and arithmetic are never PATH lookups.
            (
                "n=0; for t in tests/*.php; do [ -f \"$t\" ] || continue; php \"$t\"; n=$((n+1)); done",
                vec!["php"],
            ),
            ("while true; do break; done; node x", vec!["node"]),
            ("if [ -f a ]; then :; elif [[ -f b ]]; then :; fi", vec![]),
            ("(( n += 2 )); php -v", vec!["php"]),
            ("x=$(git rev-parse HEAD); grep -q y \"$(pwd)/f\"", vec!["grep"]),
            ("echo $(( (1+2) * 3 )) && composer -V", vec!["composer"]),
            ("(cd src && make)", vec!["make"]),
        ] {
            assert_eq!(executables(program), expected, "{program}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn gate_runtime_php_admission_uses_sandbox_path() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let mut gates = VerifyGateOptions::default();
        gates.verify = vec!["php -v".into()];
        gates.gate_environment.mode = AgentTaskGateEnvironmentMode::Replace;
        gates
            .gate_environment
            .variables
            .insert("PATH".into(), root.path().display().to_string());
        let error = gates.preflight(root.path()).unwrap_err();
        assert_eq!(error.code, homeboy_core::ErrorCode::GateRuntimeUnavailable);
        assert_eq!(error.details["gate"], "php -v");
        assert_eq!(error.details["executable"], "php");
        let php = root.path().join("php");
        fs::write(&php, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&php, fs::Permissions::from_mode(0o755)).unwrap();
        gates.preflight(root.path()).unwrap();
    }
}
