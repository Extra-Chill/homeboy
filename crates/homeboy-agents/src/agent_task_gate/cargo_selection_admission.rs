//! Compile-free admission check for focused `cargo test` gate filters.
//!
//! A focused Cargo gate whose filter selects no tests can never pass, and it
//! used to be discovered only after a full provider run. Building the test
//! binaries at admission is too expensive, so this reads the Rust sources
//! instead and refuses a filter only when it provably cannot be part of any
//! test path that exists in the checkout.
//!
//! The check over-approximates the test inventory: any doubt (doctests,
//! custom harnesses, test-generating macros, generated sources, an
//! unresolvable package) skips it and leaves runtime selection evidence
//! authoritative. Exact filters (`-- --exact`) are never checked here: they
//! are how a gate names a test the provider has not written yet.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use homeboy_core::{Error, Result};
use serde_json::json;

const MAX_SCANNED_FILES: usize = 50_000;
const MAX_SCANNED_BYTES: u64 = 256 * 1024 * 1024;

/// Refuse a declared focused `cargo test` gate whose substring filter matches
/// no test path in the checkout, before any provider is dispatched.
pub(crate) fn reject_unmatched_focused_cargo_filters<'a>(
    cwd: &Path,
    gates: impl IntoIterator<Item = &'a String>,
) -> Result<()> {
    for command in gates {
        let Some(selection) = FocusedSelection::parse(command) else {
            continue;
        };
        let Some(scope) = selection.scope_dirs(cwd) else {
            continue;
        };
        let Some(inventory) = TestPathInventory::scan(&scope, selection.doctests_possible) else {
            continue;
        };
        if inventory.may_match(&selection.filter) {
            continue;
        }
        let scope_label = selection
            .packages
            .iter()
            .map(|package| format!("package `{package}`"))
            .collect::<Vec<_>>()
            .join(", ");
        let scope_label = if scope_label.is_empty() {
            "this workspace".to_string()
        } else {
            scope_label
        };
        let mut error = Error::validation_invalid_argument(
            "gate declaration",
            format!(
                "declared Cargo gate `{command}` selects no tests: filter `{}` is not part of any test path in {scope_label}. Cargo matches a filter as a substring of full test paths such as `module::tests::test_name`, so a module without tests or a misspelled name selects nothing and the gate can never pass. Refused before provider dispatch.",
                selection.filter
            ),
            Some(command.clone()),
            None,
        );
        error.retryable = Some(false);
        error.details = json!({
            "failure_classification": "zero_tests_selected",
            "command": command,
            "filter": selection.filter,
            "packages": selection.packages,
            "tried": [
                "Choose a filter that is a substring of an existing test path; list them with the same selectors and `-- --list`.",
                "To gate on a test the provider will add, name its full path with `-- --exact`, for example `cargo test -p <package> --lib module::tests::new_test -- --exact`.",
            ],
        });
        return Err(error);
    }
    Ok(())
}

struct FocusedSelection {
    filter: String,
    packages: Vec<String>,
    manifest_path: Option<String>,
    doctests_possible: bool,
}

impl FocusedSelection {
    fn parse(command: &str) -> Option<Self> {
        if super::failure_evidence::has_shell_control_operator(command) {
            return None;
        }
        let tokens = crate::agent_task::tokenize_command(command);
        let cargo_index = super::failure_evidence::cargo_test_index(&tokens)?;
        let args = &tokens[cargo_index + 1..];
        let test_index = args.iter().position(|token| token == "test")?;
        let harness = args.iter().position(|token| token == "--");
        let cargo_args = &args[test_index + 1..harness.unwrap_or(args.len())];
        let filter_index = super::failure_evidence::cargo_test_filter_index(cargo_args)?;
        let filter = cargo_args[filter_index].clone();
        // A second positional is not a valid cargo filter shape; let Cargo
        // report it.
        if cargo_args[filter_index + 1..]
            .iter()
            .any(|argument| !argument.starts_with('-'))
        {
            return None;
        }
        if let Some(harness) = harness {
            // `--exact` may name a test the provider will create, and a
            // harness positional (a value or an extra filter) widens the
            // selection beyond what this check models.
            if args[harness + 1..]
                .iter()
                .any(|argument| argument == "--exact" || !argument.starts_with('-'))
            {
                return None;
            }
        }
        let mut packages = Vec::new();
        let mut manifest_path = None;
        let mut whole_workspace = false;
        let mut target_selected = false;
        let mut index = 0;
        while let Some(argument) = cargo_args.get(index) {
            let value = cargo_args.get(index + 1).cloned();
            match argument.as_str() {
                "-p" | "--package" => {
                    packages.push(value?);
                    index += 1;
                }
                "--manifest-path" => {
                    manifest_path = Some(value?);
                    index += 1;
                }
                "--workspace" | "--all" => whole_workspace = true,
                "--doc" => return None,
                "--lib" | "--bins" | "--tests" | "--examples" | "--benches" | "--all-targets" => {
                    target_selected = true
                }
                "--bin" | "--test" | "--example" | "--bench" => {
                    target_selected = true;
                    index += 1;
                }
                "--exclude" | "--features" | "--target" | "--target-dir" | "--profile" | "-j"
                | "--jobs" | "--config" | "--message-format" | "--timings" => index += 1,
                other => {
                    if let Some(package) = other.strip_prefix("--package=") {
                        packages.push(package.to_string());
                    } else if let Some(path) = other.strip_prefix("--manifest-path=") {
                        manifest_path = Some(path.to_string());
                    } else if let Some(package) =
                        other.strip_prefix("-p").filter(|rest| !rest.is_empty())
                    {
                        packages.push(package.to_string());
                    }
                }
            }
            index += 1;
        }
        if whole_workspace {
            packages.clear();
        }
        Some(Self {
            filter,
            packages,
            manifest_path,
            doctests_possible: !target_selected,
        })
    }

    /// Directories whose sources contain every test this gate could select,
    /// or `None` when a selected package cannot be located.
    fn scope_dirs(&self, cwd: &Path) -> Option<Vec<PathBuf>> {
        let root = match &self.manifest_path {
            Some(manifest) => cwd.join(manifest).parent()?.to_path_buf(),
            None => cwd.to_path_buf(),
        };
        if !root.join("Cargo.toml").is_file() {
            return None;
        }
        if self.packages.is_empty() {
            return Some(vec![root]);
        }
        let mut manifests = Vec::new();
        collect_files(&root, &mut |path: &Path| {
            if path.file_name().is_some_and(|name| name == "Cargo.toml") {
                manifests.push(path.to_path_buf());
            }
        })?;
        // A package name declared by more than one manifest (for example a
        // test fixture) is ambiguous; leave it to runtime evidence.
        self.packages
            .iter()
            .map(|package| {
                let mut matches = manifests.iter().filter(|manifest| {
                    std::fs::read_to_string(manifest)
                        .ok()
                        .and_then(|text| manifest_package_name(&text))
                        .as_deref()
                        == Some(package.as_str())
                });
                let manifest = matches.next()?;
                if matches.next().is_some() {
                    return None;
                }
                manifest.parent().map(Path::to_path_buf)
            })
            .collect()
    }
}

fn manifest_package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() == "name" {
            let value = value.trim();
            return value
                .strip_prefix('"')
                .and_then(|value| value.split_once('"'))
                .map(|(name, _)| name.to_string());
        }
    }
    None
}

/// Every identifier that can be a segment of a test path in the scanned
/// sources: the path components of files holding tests, the functions and
/// modules declared in them, and modules mounted with `#[path]` together with
/// their declaring file's components.
struct TestPathInventory {
    segments: BTreeSet<String>,
}

impl TestPathInventory {
    fn scan(scope: &[PathBuf], doctests_possible: bool) -> Option<Self> {
        let mut files = Vec::new();
        let mut manifests = Vec::new();
        for root in scope {
            collect_files(root, &mut |path: &Path| {
                if path.extension().is_some_and(|extension| extension == "rs") {
                    files.push((root.clone(), path.to_path_buf()));
                } else if path.file_name().is_some_and(|name| name == "Cargo.toml") {
                    manifests.push(path.to_path_buf());
                }
            })?;
        }
        if files.is_empty() || files.len() > MAX_SCANNED_FILES {
            return None;
        }
        // A custom harness names its tests however it likes.
        for manifest in &manifests {
            let text = std::fs::read_to_string(manifest).ok()?;
            if text
                .lines()
                .any(|line| line.contains("harness") && line.contains("false"))
            {
                return None;
            }
        }
        let mut bytes = 0u64;
        let mut segments = BTreeSet::new();
        for (root, file) in &files {
            bytes += std::fs::metadata(file).ok()?.len();
            if bytes > MAX_SCANNED_BYTES {
                return None;
            }
            let source = std::fs::read_to_string(file).ok()?;
            // Doc comments may hold doctests, whose names this does not model.
            if doctests_possible && source.contains("```") {
                return None;
            }
            // Fixture strings and comments mention attributes and macros
            // without declaring them; read only code.
            let code = strip_comments_and_literals(&source);
            if code.contains("include!") || (doctests_possible && code.contains("include_str!")) {
                return None;
            }
            if macro_definitions_declare_tests(&code)? {
                return None;
            }
            let components = path_components(root, file);
            if test_markers(&code)? {
                segments.extend(components.iter().cloned());
                segments.extend(declared_idents(&code, &["fn", "mod"]));
            }
            if code.contains("#[path") {
                segments.extend(components);
                segments.extend(path_mounted_modules(&code));
            }
        }
        Some(Self { segments })
    }

    /// Libtest test paths consist of identifiers joined by `::`, so a filter
    /// can match only if each `:`-separated piece lies inside one segment.
    fn may_match(&self, filter: &str) -> bool {
        filter
            .split(':')
            .filter(|piece| !piece.is_empty())
            .all(|piece| self.segments.iter().any(|segment| segment.contains(piece)))
    }
}

/// Whether code declares tests, or `None` when it uses a test attribute or
/// macro whose generated names this check cannot model.
fn test_markers(text: &str) -> Option<bool> {
    let squashed: String = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if ["proptest!", "quickcheck!", "paste!", "concat_idents!"]
        .iter()
        .any(|marker| squashed.contains(marker))
    {
        return None;
    }
    let mut marked = false;
    for (start, _) in text.match_indices("#[").chain(text.match_indices("#![")) {
        let body = &text[start..];
        let body = &body[body.find('[')? + 1..];
        let end = body.find(']').unwrap_or(body.len());
        let attribute: String = body[..end]
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        let name = attribute
            .split(['(', '='])
            .next()
            .unwrap_or_default()
            .to_string();
        match name.as_str() {
            "test" | "bench" | "tokio::test" | "async_std::test" => marked = true,
            "cfg" | "cfg_attr" | "allow" | "expect" | "warn" | "deny" | "doc" | "ignore"
            | "should_panic" => {}
            other if other.contains("test") => return None,
            _ => {}
        }
    }
    Some(marked)
}

/// Whether a `macro_rules!` body declares tests: invocations elsewhere name
/// them from their arguments. `None` when a body cannot be delimited.
fn macro_definitions_declare_tests(code: &str) -> Option<bool> {
    for (start, _) in code.match_indices("macro_rules!") {
        let rest = &code[start..];
        let open = rest.find(['{', '(', '['])?;
        let mut depth = 0usize;
        let mut end = None;
        for (offset, character) in rest[open..].char_indices() {
            match character {
                '{' | '(' | '[' => depth += 1,
                '}' | ')' | ']' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        end = Some(open + offset);
                        break;
                    }
                }
                _ => {}
            }
        }
        if test_markers(&rest[open..end?]) != Some(false) {
            return Some(true);
        }
    }
    Some(false)
}

/// Replace comments and string/char literal contents with spaces so only
/// code is inspected. Lifetimes are kept.
fn strip_comments_and_literals(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let is_ident = |character: char| character.is_alphanumeric() || character == '_';
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        let next = chars.get(index + 1).copied();
        if character == '/' && next == Some('/') {
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
            continue;
        }
        if character == '/' && next == Some('*') {
            let mut depth = 0usize;
            while index < chars.len() {
                if chars[index] == '/' && chars.get(index + 1) == Some(&'*') {
                    depth += 1;
                    index += 2;
                } else if chars[index] == '*' && chars.get(index + 1) == Some(&'/') {
                    depth -= 1;
                    index += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    index += 1;
                }
            }
            out.push(' ');
            continue;
        }
        let previous_is_ident = index > 0 && is_ident(chars[index - 1]);
        // Raw strings: r"..", r#".."#, br#".."#.
        if character == 'r' && (!previous_is_ident || (index > 0 && chars[index - 1] == 'b')) {
            let mut cursor = index + 1;
            while chars.get(cursor) == Some(&'#') {
                cursor += 1;
            }
            if chars.get(cursor) == Some(&'"') {
                let hashes = cursor - index - 1;
                cursor += 1;
                while cursor < chars.len() {
                    if chars[cursor] == '"'
                        && (1..=hashes).all(|offset| chars.get(cursor + offset) == Some(&'#'))
                    {
                        cursor += 1 + hashes;
                        break;
                    }
                    cursor += 1;
                }
                out.push_str(" \"\" ");
                index = cursor;
                continue;
            }
        }
        if character == '"' {
            let mut cursor = index + 1;
            while cursor < chars.len() && chars[cursor] != '"' {
                cursor += if chars[cursor] == '\\' { 2 } else { 1 };
            }
            out.push_str(" \"\" ");
            index = cursor + 1;
            continue;
        }
        if character == '\'' {
            if next == Some('\\') {
                // `'\n'`, `'\''`, `'\u{..}'`: the escaped character is never
                // the closing quote.
                let mut cursor = index + 3;
                while cursor < chars.len() && chars[cursor] != '\'' {
                    cursor += 1;
                }
                out.push_str(" ' ' ");
                index = cursor + 1;
                continue;
            }
            if chars.get(index + 2) == Some(&'\'') {
                out.push_str(" ' ' ");
                index += 3;
                continue;
            }
        }
        out.push(character);
        index += 1;
    }
    out
}

fn path_components(root: &Path, file: &Path) -> Vec<String> {
    file.strip_prefix(root)
        .unwrap_or(file)
        .with_extension("")
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_string))
        .collect()
}

fn declared_idents(text: &str, keywords: &[&str]) -> Vec<String> {
    let tokens = identifiers(text);
    tokens
        .windows(2)
        .filter(|pair| keywords.contains(&pair[0]))
        .map(|pair| pair[1].to_string())
        .collect()
}

/// `#[path = "..."] mod name;` mounts a file under `name`, independent of
/// its file location.
fn path_mounted_modules(text: &str) -> Vec<String> {
    text.match_indices("#[path")
        .filter_map(|(start, _)| {
            let rest = &text[start..];
            let rest = &rest[rest.find(']')? + 1..];
            let tokens = identifiers(rest);
            let position = tokens.iter().take(8).position(|token| *token == "mod")?;
            tokens.get(position + 1).map(|name| name.to_string())
        })
        .collect()
}

fn identifiers(text: &str) -> Vec<&str> {
    text.split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .filter(|token| !token.is_empty())
        .collect()
}

/// Visit regular files under `root`, skipping build output, VCS metadata,
/// dependency trees, and hidden directories. `None` when the tree is too
/// large to scan within the admission budget.
fn collect_files(root: &Path, visit: &mut dyn FnMut(&Path)) -> Option<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut visited = 0usize;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > MAX_SCANNED_FILES * 4 {
                return None;
            }
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if file_type.is_dir() {
                if !(name.starts_with('.') || name == "target" || name == "node_modules") {
                    pending.push(path);
                }
            } else if file_type.is_file() {
                visit(&path);
            }
        }
    }
    Some(())
}
