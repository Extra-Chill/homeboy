//! Declared test secret names for the components a gate actually tests.
//!
//! A Cook admits one component, but on a monorepo its gate may test a
//! different, nested component (`homeboy review test extrachill-events`, or
//! `( cd plugins/extrachill-events && homeboy review test )`). The runner only
//! materializes, and the gate child only receives, the secret names this
//! function returns, so it must cover every tested component (#19187).
//!
//! Only names some relevant component's test extension declares are produced;
//! values are never read here.

use std::collections::BTreeSet;
use std::path::{Component as PathPart, Path, PathBuf};

use homeboy_core::component::Component;

/// Union of the declared `test.secret_env` names of the admitted component and
/// every component the gate commands target.
///
/// Each gate command is an argv: either a direct `homeboy ... test [<id>]`
/// invocation or a shell program (`sh -c '<script>'`). Shell programs are
/// scanned for simple `homeboy [review] test` commands, honouring `cd <dir>`
/// (scoped to `( ... )` subshells) so a component addressed by directory
/// resolves at that directory. Directories outside `cwd` are ignored.
/// Unresolvable components contribute no names; the gate reports its own
/// error as before.
pub fn gate_test_secret_env_names<'a>(
    admitted_component_id: Option<&str>,
    cwd: &Path,
    gate_argvs: impl IntoIterator<Item = &'a [String]>,
) -> Vec<String> {
    let mut names = BTreeSet::new();
    let mut add = |component: Option<Component>| {
        if let Some(component) = component {
            names.extend(
                homeboy_core::extension::test::declared_secret_env_names(&component)
                    .unwrap_or_default(),
            );
        }
    };
    add(homeboy_core::component::resolve_effective(
        admitted_component_id,
        Some(&cwd.to_string_lossy()),
        None,
    )
    .ok());
    for argv in gate_argvs {
        for target in gate_test_targets(cwd, argv) {
            add(target.resolve(cwd));
        }
    }
    names.into_iter().collect()
}

/// A component a gate's `homeboy test` invocation addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GateTestTarget {
    /// `homeboy review test <id>` (resolved from the gate tree, then registry).
    Id(String),
    /// The component at a directory (`cd <dir> && homeboy review test`, or
    /// `--path <dir>`).
    Directory(PathBuf),
}

impl GateTestTarget {
    fn resolve(&self, cwd: &Path) -> Option<Component> {
        match self {
            Self::Directory(dir) => resolve_at(dir),
            Self::Id(id) => {
                let candidate = cwd.join(id);
                if contained(cwd, &candidate) && candidate.join("homeboy.json").is_file() {
                    return resolve_at(&candidate);
                }
                std::iter::once(cwd.to_path_buf())
                    .chain(homeboy_core::component::nested_portable_directories(cwd))
                    .find_map(|dir| {
                        homeboy_core::component::try_discover_from_portable(&dir)
                            .ok()
                            .flatten()
                            .filter(|component| &component.id == id)
                    })
                    .or_else(|| {
                        homeboy_core::component::resolve_effective(Some(id), None, None).ok()
                    })
            }
        }
    }
}

fn resolve_at(dir: &Path) -> Option<Component> {
    homeboy_core::component::resolve_effective(None, Some(&dir.to_string_lossy()), None).ok()
}

/// The `homeboy test` targets one gate argv names.
pub(crate) fn gate_test_targets(cwd: &Path, argv: &[String]) -> Vec<GateTestTarget> {
    let mut targets = Vec::new();
    match shell_script(argv) {
        Some(script) => scan_script(cwd, script, &mut targets),
        None => command_targets(cwd, cwd, argv, &mut targets),
    }
    targets
}

fn shell_script(argv: &[String]) -> Option<&str> {
    let program = program_name(argv.first()?);
    if !matches!(program, "sh" | "bash" | "dash" | "zsh") {
        return None;
    }
    let flag = argv
        .iter()
        .skip(1)
        .position(|arg| arg.starts_with('-') && !arg.starts_with("--") && arg.contains('c'))?;
    argv.get(flag + 2).map(String::as_str)
}

fn program_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

#[derive(Debug, PartialEq)]
enum Token {
    Word(String),
    Op(&'static str),
}

/// Tokenize a shell program into words and the control operators that
/// separate simple commands. Expansions are kept literal; anything this does
/// not understand only yields fewer targets.
fn tokenize(script: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = script.chars().peekable();
    let flush = |tokens: &mut Vec<Token>, word: &mut String, in_word: &mut bool| {
        if *in_word {
            tokens.push(Token::Word(std::mem::take(word)));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    word.push(c);
                }
            }
            '"' => {
                in_word = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => {
                            if let Some(next) = chars.next() {
                                word.push(next);
                            }
                        }
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                if let Some(next) = chars.next() {
                    if next != '\n' {
                        in_word = true;
                        word.push(next);
                    }
                }
            }
            '#' if !in_word => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
                tokens.push(Token::Op(";"));
            }
            c if c.is_whitespace() && c != '\n' => flush(&mut tokens, &mut word, &mut in_word),
            '\n' | ';' | '(' | ')' | '&' | '|' => {
                flush(&mut tokens, &mut word, &mut in_word);
                let op = match c {
                    '(' => "(",
                    ')' => ")",
                    '&' if chars.peek() == Some(&'&') => {
                        chars.next();
                        "&&"
                    }
                    '|' if chars.peek() == Some(&'|') => {
                        chars.next();
                        "||"
                    }
                    '|' => "|",
                    _ => ";",
                };
                tokens.push(Token::Op(op));
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    flush(&mut tokens, &mut word, &mut in_word);
    tokens
}

fn scan_script(cwd: &Path, script: &str, targets: &mut Vec<GateTestTarget>) {
    let mut dir = cwd.to_path_buf();
    let mut subshells = Vec::new();
    let mut words = Vec::new();
    for token in tokenize(script) {
        match token {
            Token::Word(word) => words.push(word),
            Token::Op(op) => {
                simple_command(cwd, &mut dir, &std::mem::take(&mut words), targets);
                match op {
                    "(" => subshells.push(dir.clone()),
                    ")" => {
                        if let Some(outer) = subshells.pop() {
                            dir = outer;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    simple_command(cwd, &mut dir, &words, targets);
}

fn simple_command(
    cwd: &Path,
    dir: &mut PathBuf,
    words: &[String],
    targets: &mut Vec<GateTestTarget>,
) {
    let words: Vec<String> = words
        .iter()
        .skip_while(|word| {
            matches!(word.as_str(), "exec" | "env" | "command" | "time")
                || (word.contains('=') && !word.starts_with('-') && !word.starts_with('='))
        })
        .cloned()
        .collect();
    let Some(program) = words.first() else {
        return;
    };
    if program == "cd" || program == "pushd" {
        // `cd` with no operand (or `-`) leaves the gate tree; stop resolving
        // directory-addressed targets until the subshell ends.
        match words.get(1).filter(|arg| !arg.starts_with('-')) {
            Some(target) => *dir = normalize(&dir.join(target)),
            None => *dir = PathBuf::new(),
        }
        return;
    }
    command_targets(cwd, dir, &words, targets);
}

/// Targets of one `homeboy [global flags] [review] test [<id>] [--path <dir>]`
/// argv run from `dir`.
fn command_targets(cwd: &Path, dir: &Path, words: &[String], targets: &mut Vec<GateTestTarget>) {
    let Some(program) = words.first() else {
        return;
    };
    if program_name(program) != "homeboy" {
        return;
    }
    let mut rest = None;
    let mut previous_flag = false;
    for (index, word) in words.iter().enumerate().skip(1) {
        if word == "review" && words.get(index + 1).is_some_and(|next| next == "test") {
            rest = Some(index + 2);
            break;
        }
        if word == "test" {
            rest = Some(index + 1);
            break;
        }
        if word.starts_with('-') {
            previous_flag = !word.contains('=');
            continue;
        }
        if !previous_flag {
            return;
        }
        previous_flag = false;
    }
    let Some(rest) = rest else {
        return;
    };
    let args = &words[rest..];
    let path = args.iter().enumerate().find_map(|(index, arg)| {
        arg.strip_prefix("--path=").map(str::to_string).or_else(|| {
            (arg == "--path")
                .then(|| args.get(index + 1).cloned())
                .flatten()
        })
    });
    let id = args.first().filter(|arg| !arg.starts_with('-')).cloned();
    let in_tree = |path: PathBuf| contained(cwd, &path).then_some(path);
    let target = match (path, id) {
        (Some(path), _) => in_tree(normalize(&dir.join(path))).map(GateTestTarget::Directory),
        (None, Some(id)) => Some(GateTestTarget::Id(id)),
        (None, None) => in_tree(dir.to_path_buf()).map(GateTestTarget::Directory),
    };
    if let Some(target) = target.filter(|target| !targets.contains(target)) {
        targets.push(target);
    }
}

/// Lexical normalization; no filesystem access, so `..` cannot escape `cwd`
/// through containment checks.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            PathPart::CurDir => {}
            PathPart::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    normalized
}

fn contained(cwd: &Path, path: &Path) -> bool {
    !path.as_os_str().is_empty() && normalize(path).starts_with(normalize(cwd))
}

#[cfg(test)]
#[path = "gate_secret_env_tests.rs"]
mod tests;
