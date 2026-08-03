use super::*;

fn masks(content: &str) -> SourceMasks {
    SourceMasks::new(content, Language::Rust)
}

#[test]
fn line_comments_are_removed_from_the_code_projection() {
    let masks = masks("let node = 1; // runs on every node\n");

    assert_eq!(masks.code(0).trim_end(), "let node = 1;");
    assert!(!masks.code(0).contains("runs on every"));
}

#[test]
fn doc_comments_are_removed() {
    // The dominant false-positive shape in homeboy #11298.
    let masks = masks("/// Globals are identical on every node and are excluded.\nfn f() {}\n");

    assert!(masks.code(0).trim().is_empty());
    assert_eq!(masks.code(1), "fn f() {}");
}

#[test]
fn inner_doc_comments_are_removed() {
    let masks = masks("//! HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli\n");

    assert!(masks.code(0).trim().is_empty());
}

#[test]
fn block_comments_span_lines_and_nest() {
    let masks = masks("a /* one\n two /* inner */ still comment */ b\n");

    assert_eq!(masks.code(0).trim_end(), "a");
    assert!(!masks.code(1).contains("inner"));
    assert!(!masks.code(1).contains("still comment"));
    assert!(masks.code(1).contains('b'));
}

#[test]
fn masking_preserves_column_positions() {
    // Findings report a line and a context; blanking rather than deleting keeps
    // any column-derived reporting honest.
    let source = "let x = 1; // cargo\n";
    let masks = masks(source);

    assert_eq!(masks.code(0).chars().count(), source.trim_end().len());
    assert!(masks.code(0).starts_with("let x = 1; "));
}

#[test]
fn comment_markers_inside_strings_do_not_start_a_comment() {
    // The classic masker bug: a URL is not a comment.
    let masks = masks("let url = \"https://example.com/node\"; let after = 2;\n");

    assert!(masks.code(0).contains("after = 2"));
    assert!(masks.code(0).contains("https://example.com/node"));
}

#[test]
fn quote_inside_a_comment_does_not_open_a_string() {
    let masks = masks("// it's fine\nlet real = \"kept\";\n");

    assert!(masks.code(0).trim().is_empty());
    assert!(masks.code(1).contains("kept"));
}

#[test]
fn escaped_quotes_do_not_end_a_string() {
    let masks = masks("let s = \"a\\\"b\"; let after = 1;\n");

    assert!(masks.code(0).contains("after = 1"));
    assert!(masks.strings(0).contains("a\\\"b"));
}

#[test]
fn rust_raw_strings_are_string_spans() {
    let masks = masks("let s = r#\"a \"quoted\" node\"#; let after = 1;\n");

    assert!(masks.strings(0).contains("quoted"));
    assert!(masks.code(0).contains("after = 1"));
}

#[test]
fn rust_lifetimes_are_not_char_literals() {
    // `'a` must not open a string and swallow the rest of the line.
    let masks = masks("fn f<'a>(x: &'a str) -> &'a str { x }\n");

    assert!(masks.code(0).contains("-> &'a str { x }"));
    assert!(
        masks.strings(0).trim().is_empty(),
        "lifetimes are code, not strings: {:?}",
        masks.strings(0)
    );
}

#[test]
fn rust_char_literals_are_string_spans() {
    let masks = masks("let c = 'x'; let n = '\\n';\n");

    assert!(masks.strings(0).contains('x'));
    assert!(masks.code(0).contains("let c ="));
}

#[test]
fn string_projection_keeps_only_literals() {
    let masks = masks("run_tool(\"florp-run\", node);\n");

    assert!(masks.strings(0).contains("florp-run"));
    assert!(
        !masks.strings(0).contains("run_tool"),
        "identifiers must not appear in the string projection: {:?}",
        masks.strings(0)
    );
    assert!(
        !masks.strings(0).contains("node"),
        "a local variable is not a string literal: {:?}",
        masks.strings(0)
    );
}

#[test]
fn string_projection_preserves_seam_punctuation() {
    // `detect_split` looks for `"car", "go"`; blanking the comma would hide it.
    let masks = masks("let joined = [\"car\", \"go\"].concat();\n");

    assert!(masks.strings(0).contains("\"car\", \"go\""));
}

#[test]
fn php_hash_comments_are_removed() {
    let masks = SourceMasks::new("$x = 1; # uses composer\n", Language::Php);

    assert_eq!(masks.code(0).trim_end(), "$x = 1;");
}

#[test]
fn php_single_quoted_strings_are_string_spans() {
    let masks = SourceMasks::new("$cmd = 'composer install';\n", Language::Php);

    assert!(masks.strings(0).contains("composer install"));
}

#[test]
fn javascript_template_literals_are_string_spans() {
    let masks = SourceMasks::new("const c = `npm ci`;\n", Language::JavaScript);

    assert!(masks.strings(0).contains("npm ci"));
}

#[test]
fn javascript_does_not_nest_block_comments() {
    // Only Rust nests; in JS the first `*/` closes the comment.
    let masks = SourceMasks::new("/* /* */ const after = 1;\n", Language::JavaScript);

    assert!(masks.code(0).contains("const after = 1"));
}

#[test]
fn out_of_range_lines_are_empty_rather_than_panicking() {
    let masks = masks("fn f() {}\n");

    assert_eq!(masks.code(99), "");
    assert_eq!(masks.strings(99), "");
}

#[test]
fn unterminated_block_comment_does_not_panic() {
    let masks = masks("fn f() {} /* never closed\nstill comment\n");

    assert!(masks.code(0).contains("fn f() {}"));
    assert!(masks.code(1).trim().is_empty());
}
