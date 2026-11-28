//! Wave 8.0 (#19285): freeze literal top-level metadata accesses, not JSON schemas.
//! This is a lexical guard, not Rust type or data-flow analysis. It also sees
//! request/outcome accesses spelled `metadata`, and does not infer computed keys.

use proc_macro2::{Delimiter, Group, TokenStream, TokenTree};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const ALLOWLIST: &str = include_str!("fixtures/run_metadata_keys.txt");

fn source_files(dir: &Path, paths: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name = name.to_str().unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir()
            && !matches!(name, "tests" | "fixtures" | "test_support")
            && !name.ends_with("_tests")
        {
            source_files(&entry.path(), paths);
        } else if kind.is_file()
            && name.ends_with(".rs")
            && !matches!(name, "tests.rs" | "fixtures.rs" | "test_support.rs")
            && !name.starts_with("test_")
            && !name.ends_with("_test.rs")
            && !name.ends_with("_tests.rs")
            && !name.ends_with("_test_support.rs")
        {
            paths.push(entry.path());
        }
    }
}

fn ident(token: Option<&TokenTree>, name: &str) -> bool {
    matches!(token, Some(TokenTree::Ident(id)) if id == name)
}

fn punct(token: Option<&TokenTree>, ch: char) -> bool {
    matches!(token, Some(TokenTree::Punct(p)) if p.as_char() == ch)
}

fn group(token: Option<&TokenTree>, delimiter: Delimiter) -> Option<&Group> {
    match token {
        Some(TokenTree::Group(group)) if group.delimiter() == delimiter => Some(group),
        _ => None,
    }
}

// Would this cfg still be possible with test and test-support disabled?
fn cfg_possible(tokens: TokenStream) -> bool {
    let tokens: Vec<_> = tokens.into_iter().collect();
    match tokens.as_slice() {
        [TokenTree::Ident(name)] if name == "test" => false,
        [TokenTree::Ident(name), _, literal] if name == "feature" => {
            string(literal).as_deref() != Some("test-support")
        }
        [TokenTree::Ident(name), TokenTree::Group(args)] if name == "all" || name == "any" => {
            let mut clauses = vec![TokenStream::new()];
            for token in args.stream() {
                if punct(Some(&token), ',') {
                    clauses.push(TokenStream::new());
                } else {
                    clauses.last_mut().unwrap().extend([token]);
                }
            }
            let mut clauses = clauses.into_iter().filter(|clause| !clause.is_empty());
            if name == "all" {
                clauses.all(cfg_possible)
            } else {
                clauses.any(cfg_possible)
            }
        }
        _ => true,
    }
}

fn test_attribute(tokens: TokenStream) -> bool {
    let tokens: Vec<_> = tokens.into_iter().collect();
    match tokens.as_slice() {
        [TokenTree::Ident(name)] => name == "test",
        [TokenTree::Ident(name), TokenTree::Group(args)] if name == "cfg" => {
            !cfg_possible(args.stream())
        }
        _ => false,
    }
}

fn visit_tokens(stream: TokenStream, visit: &mut impl FnMut(&[TokenTree])) {
    let tokens: Vec<_> = stream.into_iter().collect();
    let mut i = 0;
    while i < tokens.len() {
        if punct(tokens.get(i), '#') {
            if let Some(attr) = group(tokens.get(i + 1), Delimiter::Bracket) {
                i += 2;
                if test_attribute(attr.stream()) {
                    // Skip the attributed item (an inline test module, function,
                    // or an out-of-line declaration), including its body.
                    while i < tokens.len() {
                        let end = punct(tokens.get(i), ';')
                            || group(tokens.get(i), Delimiter::Brace).is_some();
                        i += 1;
                        if end {
                            break;
                        }
                    }
                }
                continue;
            }
        }
        if let TokenTree::Group(group) = &tokens[i] {
            visit_tokens(group.stream(), visit);
        } else if matches!(tokens[i], TokenTree::Ident(_)) {
            visit(&tokens[i..]);
        }
        i += 1;
    }
}

fn string(token: &TokenTree) -> Option<String> {
    let TokenTree::Literal(literal) = token else {
        return None;
    };
    syn::parse2::<syn::LitStr>(TokenStream::from(TokenTree::Literal(literal.clone())))
        .ok()
        .map(|literal| literal.value())
}

// Resolve string constants by their last path segment. If two modules use the
// same name, retain both values rather than silently choosing one of them.
type Constants = BTreeMap<String, BTreeSet<String>>;

fn collect_constants(tokens: &[TokenTree], constants: &mut Constants) {
    if let [TokenTree::Ident(keyword), TokenTree::Ident(name), colon, amp, ty, eq, value, end, ..] =
        tokens
    {
        if keyword == "const"
            && punct(Some(colon), ':')
            && punct(Some(amp), '&')
            && ident(Some(ty), "str")
            && punct(Some(eq), '=')
            && punct(Some(end), ';')
        {
            if let Some(value) = string(value) {
                constants.entry(name.to_string()).or_default().insert(value);
            }
        }
    }
}

fn key_values(tokens: &[TokenTree], constants: &Constants) -> Vec<String> {
    let tokens = if punct(tokens.first(), '&') {
        &tokens[1..]
    } else {
        tokens
    };
    let Some(first) = tokens.first() else {
        return Vec::new();
    };
    if let Some(value) = string(first) {
        return vec![value];
    }
    let mut name = first;
    let mut rest = &tokens[1..];
    while let [a, b, next @ TokenTree::Ident(_), tail @ ..] = rest {
        if !punct(Some(a), ':') || !punct(Some(b), ':') {
            break;
        }
        name = next;
        rest = tail;
    }
    match name {
        TokenTree::Ident(name) => constants
            .get(&name.to_string())
            .into_iter()
            .flatten()
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

// Return the argument group, which argument carries the key, and whether
// the key is a JSON pointer rather than a map key.
fn metadata_access(tokens: &[TokenTree]) -> Option<(&Group, usize, bool)> {
    let method = |offset| {
        if !punct(tokens.get(offset), '.') {
            return None;
        }
        let name = tokens.get(offset + 1);
        let pointer = ident(name, "pointer") || ident(name, "pointer_mut");
        if pointer
            || [
                "get",
                "get_mut",
                "insert",
                "remove",
                "entry",
                "contains_key",
            ]
            .iter()
            .any(|method| ident(name, method))
        {
            return Some((
                group(tokens.get(offset + 2), Delimiter::Parenthesis)?,
                0,
                pointer,
            ));
        }
        None
    };
    if ident(tokens.first(), "metadata") || ident(tokens.first(), "metadata_json") {
        return group(tokens.get(1), Delimiter::Bracket)
            .map(|args| (args, 0, false))
            .or_else(|| method(1));
    }
    if ident(tokens.first(), "ensure_metadata_object") {
        group(tokens.get(1), Delimiter::Parenthesis)?;
        return method(2);
    }
    if ["record_metadata_value", "compare_and_set_metadata_value"]
        .iter()
        .any(|name| ident(tokens.first(), name))
    {
        return Some((group(tokens.get(1), Delimiter::Parenthesis)?, 1, false));
    }
    None
}

fn scan_metadata<'a>(
    sources: impl IntoIterator<Item = (&'a Path, &'a str)>,
) -> BTreeMap<String, BTreeSet<&'a Path>> {
    let mut constants = Constants::new();
    let mut accesses = Vec::new();
    for (path, source) in sources {
        let stream = source
            .parse::<TokenStream>()
            .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        visit_tokens(stream, &mut |tokens| {
            collect_constants(tokens, &mut constants);
            if let Some((args, index, pointer)) = metadata_access(tokens) {
                accesses.push((path, args.stream(), index, pointer));
            }
        });
    }
    let mut found = BTreeMap::<_, BTreeSet<_>>::new();
    for (path, args, index, pointer) in accesses {
        let args: Vec<_> = args.into_iter().collect();
        let Some(key_arg) = args.split(|token| punct(Some(token), ',')).nth(index) else {
            continue;
        };
        for value in key_values(key_arg, &constants) {
            let key = if pointer {
                let Some(path) = value.strip_prefix('/') else {
                    continue;
                };
                path.split('/')
                    .next()
                    .unwrap()
                    .replace("~1", "/")
                    .replace("~0", "~")
            } else {
                value
            };
            found.entry(key).or_default().insert(path);
        }
    }
    found
}

#[test]
fn metadata_keys_only_shrink() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut paths = Vec::new();
    for dir in ["crates/homeboy-agents/src", "crates/homeboy-cli/src"] {
        source_files(&root.join(dir), &mut paths);
    }
    let sources: Vec<_> = paths
        .iter()
        .map(|path| fs::read_to_string(path).unwrap())
        .collect();
    // Cheap file prefilter only; token matching below decides what counts.
    // A slash after a receiver may start a comment before the dot or bracket.
    let candidate = regex::Regex::new(concat!(
        r"\bmetadata(?:_json)?\s*(?:[/\[]|\.\s*(?:/|get|insert|remove|pointer|entry|contains_key))",
        r"|\b(?:ensure_metadata_object|record_metadata_value|compare_and_set_metadata_value)\s*[(/]",
        r"|\bconst\s*(?:/|\w+\s*(?:/|:\s*(?:/|&\s*(?:/|str\b))))",
    )).unwrap();
    let found = scan_metadata(
        paths
            .iter()
            .zip(&sources)
            .filter(|(_, source)| candidate.is_match(source))
            .map(|(path, source)| (path.strip_prefix(&root).unwrap(), source.as_str())),
    );

    let mut allowed = BTreeSet::new();
    let mut previous = None;
    for line in ALLOWLIST
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let (class, key) = line.split_once(' ').expect("expected `<class> <key>`");
        assert!(
            matches!(class, "decisional" | "annotation"),
            "invalid class: {line}"
        );
        assert!(
            previous.is_none_or(|prev| prev < (class, key)),
            "allowlist out of order: {line}"
        );
        assert!(allowed.insert(key), "duplicate key: {key}");
        previous = Some((class, key));
    }
    let actual: BTreeSet<_> = found.keys().map(String::as_str).collect();
    let mut problems = Vec::new();
    for key in actual.difference(&allowed) {
        problems.push(format!("new metadata key {key:?} in {:?}; add a typed field via a transition instead; see #19262", found[*key]));
    }
    for key in allowed.difference(&actual) {
        problems.push(format!(
            "unused metadata key {key:?}; delete it from the allowlist"
        ));
    }
    assert!(
        problems.is_empty(),
        "Run metadata key ratchet failed:\n{}",
        problems.join("\n")
    );
}

#[test]
fn metadata_key_scanner_cases() {
    let cases: &[(&str, &[&str])] = &[
        (
            r#"record.metadata["brand_new_key"] = json!(true);"#,
            &["brand_new_key"],
        ),
        (
            r#"record.metadata["outer"]["inner"].get("nested");"#,
            &["outer"],
        ),
        (
            r#"record.metadata.get("read"); metadata.insert("write".into(), x); metadata.remove("remove");"#,
            &["read", "remove", "write"],
        ),
        (
            r#"record.metadata.pointer("/foo~1bar/nested"); payload.pointer("/unrelated/nested");"#,
            &["foo/bar"],
        ),
        (
            r#"const LIFECYCLE_CONTINUATION_KEY: &str = "cook_continuation";
                metadata.insert(keys::LIFECYCLE_CONTINUATION_KEY.to_string(), value);
                const UNUSED_KEY: &str = "not_in_metadata";"#,
            &["cook_continuation"],
        ),
        (
            r##"record.ensure_metadata_object().insert(r#"handoff"#.to_string(), value);"##,
            &["handoff"],
        ),
        (
            r#"store.record_metadata_value(run_id, "attempt", value);"#,
            &["attempt"],
        ),
        (
            r#"// metadata["comment"]
            let text = "metadata.get(\"string\")";
            #[cfg(all(unix, test))] mod checks { fn helper() { record.metadata["test_only"]; } }
            #[cfg(any(test, feature = "test-support"))] fn helper() { metadata["fixture"]; }
            #[cfg(not(test))] fn live() { metadata["live"]; }"#,
            &["live"],
        ),
    ];
    for (source, expected) in cases {
        let found = scan_metadata([(Path::new("example.rs"), *source)]);
        assert_eq!(
            found.keys().map(String::as_str).collect::<Vec<_>>(),
            *expected,
            "{source}"
        );
    }
}
