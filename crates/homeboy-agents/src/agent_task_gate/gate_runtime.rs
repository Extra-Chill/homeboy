//! Mutation-free executable discovery for declared shell gates.
use super::*;

pub(super) const RESOLVE_ONLY: &str = "__homeboy_gate_resolve_only__";

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
    for ch in program.chars().chain(std::iter::once('\n')) {
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
        if ch.is_whitespace() || ";|&()".contains(ch) {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            if ch == '\n' || ";|&()".contains(ch) {
                if let Some(command) = words.iter().find(|word| !assignment(word)) {
                    if !builtin(command)
                        && !command.contains(['$', '`', '*', '?', '/'])
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
            | "if"
            | "then"
            | "else"
            | "fi"
            | "for"
            | "do"
            | "done"
            | "while"
            | "until"
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
