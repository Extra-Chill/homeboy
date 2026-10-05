//! Unbounded command-output capture detector.
//!
//! This is a conservative first slice for command hygiene: it flags code that
//! accumulates stdout/stderr-style stream chunks into memory without nearby
//! evidence of a retention bound and truncation metadata.

use super::conventions::{AuditFinding, Language};
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;
use super::source_text::SourceMasks;

pub(crate) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = Vec::new();

    for fp in fingerprints {
        if !fp.relative_path.ends_with(".rs") {
            continue;
        }

        let masks = SourceMasks::new(&fp.content, Language::Rust);
        let syntax = fp
            .content
            .lines()
            .enumerate()
            .map(|(index, _)| masks.syntax(index))
            .collect::<Vec<_>>()
            .join("\n");
        let detector_content = strip_test_modules(&syntax);

        if has_unbounded_capture_shape(&detector_content) {
            findings.push(Finding {
                convention: "output_capture".to_string(),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description: "Command output capture appears to append stream chunks without an explicit retained-byte bound or truncation metadata.".to_string(),
                suggestion: "Use a bounded tail buffer and expose structured capture metadata: bytes seen, bytes retained, byte limit, and truncated flag for each captured stream.".to_string(),
                kind: AuditFinding::UnboundedOutputCapture,
                            line: None,
            });
        }

        if has_unbounded_detail_output_shape(&detector_content) {
            findings.push(Finding {
                convention: "output_capture".to_string(),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description: "Reporter output appears to emit per-match or per-file details without an explicit item cap or omitted-count metadata.".to_string(),
                suggestion: "Cap detail rows with take/truncate and report detail metadata: item limit, items rendered, omitted item count, and truncated flag.".to_string(),
                kind: AuditFinding::UnboundedOutputCapture,
                            line: None,
            });
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file));
    findings
}

fn strip_test_modules(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut stripped = String::new();
    let mut index = 0;
    let module_header = regex::Regex::new(r"^(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*(?:\{|$)")
        .expect("test module declaration regex");

    while index < lines.len() {
        if lines[index].trim() == "#[cfg(test)]" {
            let declaration = (index + 1..lines.len()).find(|line| {
                let text = lines[*line].trim();
                !text.is_empty() && !text.starts_with("#[")
            });
            if let Some(declaration) =
                declaration.filter(|line| module_header.is_match(lines[*line].trim()))
            {
                let open = if lines[declaration].contains('{') {
                    Some(declaration)
                } else {
                    (declaration + 1..lines.len())
                        .find(|line| !lines[*line].trim().is_empty())
                        .filter(|line| lines[*line].trim().starts_with('{'))
                };
                let mut depth = 0_i32;
                let mut end = None;
                for line in open.into_iter().flat_map(|start| start..lines.len()) {
                    for ch in lines[line].chars() {
                        match ch {
                            '{' => depth += 1,
                            '}' => depth -= 1,
                            _ => {}
                        }
                    }
                    if depth == 0 {
                        end = Some(line + 1);
                        break;
                    }
                }
                if let Some(end) = end {
                    index = end;
                    continue;
                }
            }
        }

        stripped.push_str(lines[index]);
        stripped.push('\n');
        index += 1;
    }

    stripped
}

/// Number of lines scanned on each side of a capture site when looking for
/// evidence of a retention bound.
const CAPTURE_BOUND_WINDOW_LINES: usize = 24;

/// Shapes that accumulate a child process' stdout/stderr into memory.
const CAPTURE_MARKERS: [&str; 4] = [
    "extend_from_slice(&buf[..n])",
    "captured.extend_from_slice",
    "read_to_string(&mut",
    "wait_with_output()",
];

fn has_unbounded_capture_shape(content: &str) -> bool {
    let output_like = content.contains("stdout") || content.contains("stderr");
    if !output_like {
        return false;
    }

    let lines: Vec<&str> = content.lines().collect();
    lines.iter().enumerate().any(|(index, line)| {
        if !CAPTURE_MARKERS.iter().any(|marker| line.contains(marker)) {
            return false;
        }

        // A retention bound is only evidence for *this* capture when it sits
        // near the capture site. Matching anywhere in the file lets an
        // unrelated token elsewhere exempt every capture in the module.
        let start = index.saturating_sub(CAPTURE_BOUND_WINDOW_LINES);
        let end = lines.len().min(index + CAPTURE_BOUND_WINDOW_LINES + 1);
        !has_capture_bound(&lines[start..end].join("\n"))
    })
}

fn has_capture_bound(window: &str) -> bool {
    // `take(` is deliberately absent: `Option::take`, `Iterator::take` and
    // `ChildStdin::take` are common enough that matching it exempts captures
    // that have no output bound at all.
    window.contains("limit_bytes")
        || window.contains("retained_bytes")
        || window.contains("truncated")
        || window.contains("BoundedCapture")
        || window.contains("MAX_OUTPUT")
        || window.contains("OUTPUT_LIMIT")
}

fn has_unbounded_detail_output_shape(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().collect();
    lines.iter().enumerate().any(|(index, line)| {
        let line = line.trim();
        if !line.starts_with("for ") || !line.contains(" in ") {
            return false;
        }

        let lower = line.to_ascii_lowercase();
        let detail_loop = [
            "matches", "findings", "files", "details", "entries", "items",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
        if !detail_loop {
            return false;
        }

        let window_end = lines.len().min(index + 24);
        let window = lines[index..window_end].join("\n");
        emits_text(&window) && !has_detail_bound(&window)
    })
}

fn emits_text(content: &str) -> bool {
    content.contains("println!")
        || content.contains("eprintln!")
        || content.contains("writeln!")
        || content.contains("push_str(&format!")
        || content.contains("push_str(format!")
}

fn has_detail_bound(content: &str) -> bool {
    content.contains(".take(")
        || content.contains(".truncate(")
        || content.contains("omitted")
        || content.contains("remaining")
        || content.contains("remainder")
        || content.contains("detail_limit")
        || content.contains("DETAIL_LIMIT")
        || content.contains("MAX_DETAIL")
        || content.contains("max_detail")
        || content.contains("TOP_N")
        || content.contains("summary")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(path: &str, content: &str) -> FileFingerprint {
        FileFingerprint {
            relative_path: path.to_string(),
            content: content.to_string(),
            ..FileFingerprint::default()
        }
    }

    #[test]
    fn flags_stream_capture_without_bound_metadata() {
        let file = fp(
            "src/run.rs",
            r#"
            fn tee_to<R: Read>(mut src: R) -> String {
                let mut captured = Vec::new();
                let mut buf = [0u8; 4096];
                if let Ok(n) = src.read(&mut buf) {
                    captured.extend_from_slice(&buf[..n]);
                }
                String::from_utf8_lossy(&captured).to_string()
            }
            fn uses_stdout(stdout: &str, stderr: &str) { let _ = (stdout, stderr); }
            "#,
        );

        let findings = run(&[&file]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::UnboundedOutputCapture);
    }

    #[test]
    fn accepts_bounded_capture_with_truncation_metadata() {
        let file = fp(
            "src/run.rs",
            r#"
            struct BoundedCapture { limit_bytes: usize, retained_bytes: usize, truncated: bool }
            fn capture_stdout() {
                let mut captured = BoundedCapture { limit_bytes: 65536, retained_bytes: 0, truncated: false };
                let stdout = "";
                let stderr = "";
                let _ = (&mut captured, stdout, stderr);
            }
            "#,
        );

        assert!(run(&[&file]).is_empty());
    }

    #[test]
    fn unrelated_option_take_does_not_exempt_capture() {
        // Regression: `child.stdin.take()` is an `Option::take`, not an output
        // bound, but a whole-file `contains("take(")` treated it as one.
        let file = fp(
            "src/download.rs",
            r#"
            fn run_child(mut child: Child) -> Result<Output, Error> {
                let mut stdin = child.stdin.take().ok_or_else(|| Error::NoStdin)?;
                stdin.write_all(b"payload")?;
                drop(stdin);
                let output = child.wait_with_output().map_err(|e| Error::Wait(e))?;
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let _ = stderr;
                Ok(output)
            }
            "#,
        );

        let findings = run(&[&file]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::UnboundedOutputCapture);
    }

    #[test]
    fn bound_far_from_capture_site_does_not_exempt_it() {
        let mut content = String::from(
            r#"
            struct Bounded { limit_bytes: usize }
            fn bounded_capture(stdout: &str) -> Bounded {
                Bounded { limit_bytes: 65536 }
            }
            "#,
        );
        for index in 0..80 {
            content.push_str(&format!("            fn filler_{index}() {{}}\n"));
        }
        content.push_str(
            r#"
            fn unbounded_capture(child: Child) -> Output {
                child.wait_with_output().expect("wait")
            }
            "#,
        );

        let findings = run(&[&fp("src/capture.rs", &content)]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::UnboundedOutputCapture);
    }

    #[test]
    fn accepts_capture_with_bound_at_the_capture_site() {
        let file = fp(
            "src/capture.rs",
            r#"
            fn capture(child: Child) -> String {
                let limit_bytes = 65536;
                let output = child.wait_with_output().expect("wait");
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let truncated = stdout.len() > limit_bytes;
                stdout.truncate(limit_bytes);
                let _ = truncated;
                stdout
            }
            "#,
        );

        assert!(run(&[&file]).is_empty());
    }

    #[test]
    fn flags_detail_reporter_without_item_cap() {
        let file = fp(
            "src/report.rs",
            r#"
            fn render(matches: Vec<String>, out: &mut String) {
                for item in matches {
                    out.push_str(&format!("- {item}\n"));
                }
            }
            "#,
        );

        let findings = run(&[&file]);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].kind, AuditFinding::UnboundedOutputCapture);
        assert!(findings[0].description.contains("per-match"));
    }

    #[test]
    fn accepts_detail_reporter_with_cap_and_omitted_count() {
        let file = fp(
            "src/report.rs",
            r#"
            const DETAIL_LIMIT: usize = 20;
            fn render(matches: Vec<String>, out: &mut String) {
                for item in matches.iter().take(DETAIL_LIMIT) {
                    out.push_str(&format!("- {item}\n"));
                }
                let omitted = matches.len().saturating_sub(DETAIL_LIMIT);
                if omitted > 0 {
                    out.push_str(&format!("... {omitted} omitted\n"));
                }
            }
            "#,
        );

        assert!(run(&[&file]).is_empty());
    }

    #[test]
    fn ignores_detail_shapes_inside_test_modules() {
        let file = fp(
            "src/report.rs",
            r#"
            fn production() {}

            #[cfg(test)]
            mod tests {
                fn sample() {
                    let matches = vec!["a", "b"];
                    for item in matches {
                        println!("{item}");
                    }
                }
            }
            "#,
        );

        assert!(run(&[&file]).is_empty());
    }

    #[test]
    fn ignores_detail_shapes_inside_string_literals() {
        let file = fp(
            "src/report.rs",
            r#"
            fn sample_fixture() -> &'static str {
                "for item in items { println!(\"{item}\"); }"
            }
            "#,
        );

        assert!(run(&[&file]).is_empty());
    }

    #[test]
    fn raw_fixture_and_comment_capture_shapes_are_not_production_findings() {
        let file = fp(
            "src/fixture.rs",
            r###"
const FIXTURE: &str = r#"quoted " stdout child.wait_with_output() " tail"#;
// stdout child.wait_with_output()
/* for item in files { println!("item"); } */
fn real() { let c = '{'; }
"###,
        );
        assert!(run(&[&file]).is_empty());
    }

    #[test]
    fn comments_cannot_supply_a_retention_bound_for_a_real_capture() {
        let file = fp("src/capture.rs", "fn capture(child: Child) {\n// TODO add limit_bytes and truncated metadata for stdout\nlet stdout = child.wait_with_output();\n}\n");
        let findings = run(&[&file]);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].description.contains("append stream chunks"));
    }

    #[test]
    fn raw_fixture_test_attributes_cannot_hide_a_real_capture() {
        let file = fp(
            "src/capture.rs",
            r###"
const FIXTURE: &str = r#"
#[cfg(test)]
mod fake_tests {
"#;
fn capture(child: Child) {
    let stdout = child.wait_with_output();
}
"###,
        );
        assert_eq!(run(&[&file]).len(), 1);
    }

    #[test]
    fn inline_test_module_does_not_hide_following_production_capture() {
        let file = fp("src/capture.rs", "#[cfg(test)]\nmod tests { fn fixture() {} }\nfn capture(child: Child) {\nlet stdout = child.wait_with_output();\n}\n");
        assert_eq!(run(&[&file]).len(), 1, "production capture remains visible");
    }

    #[test]
    fn test_only_unbraced_item_does_not_hide_following_production_capture() {
        let file = fp("src/capture.rs", "#[cfg(test)]\nuse test_support::Fixture;\nfn capture(child: Child) {\nlet stdout = child.wait_with_output();\n}\n");
        assert_eq!(run(&[&file]).len(), 1, "cfg(test) import is not a module");
    }

    #[test]
    fn attributed_test_module_with_separate_opening_brace_is_excluded() {
        let file = fp("src/capture.rs", "#[cfg(test)]\n#[allow(dead_code)]\nmod tests\n{\nfn capture(child: Child) { let stdout = child.wait_with_output(); }\n}\nfn production() {}\n");
        assert!(run(&[&file]).is_empty());
    }
}
