//! Unbounded command-output capture detector.
//!
//! This is a conservative first slice for command hygiene: it flags Rust code
//! that accumulates stdout/stderr-style stream chunks into memory without nearby
//! evidence of a retention bound and truncation metadata.

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use super::fingerprint::FileFingerprint;

pub(in crate::core::code_audit) fn run(fingerprints: &[&FileFingerprint]) -> Vec<Finding> {
    let mut findings = Vec::new();

    for fp in fingerprints {
        if !fp.relative_path.ends_with(".rs") {
            continue;
        }

        let detector_content = strip_test_modules(&strip_string_literals(&fp.content));

        if has_unbounded_capture_shape(&detector_content) {
            findings.push(Finding {
                convention: "output_capture".to_string(),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description: "Command output capture appears to append stream chunks without an explicit retained-byte bound or truncation metadata.".to_string(),
                suggestion: "Use a bounded tail buffer and expose structured capture metadata: bytes seen, bytes retained, byte limit, and truncated flag for each captured stream.".to_string(),
                kind: AuditFinding::UnboundedOutputCapture,
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
            });
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file));
    findings
}

fn strip_string_literals(content: &str) -> String {
    let mut stripped = String::with_capacity(content.len());
    let chars = content.chars();
    let mut in_string = false;
    let mut escaped = false;

    for ch in chars {
        if in_string {
            if escaped {
                escaped = false;
                stripped.push(' ');
                continue;
            }
            match ch {
                '\\' => {
                    escaped = true;
                    stripped.push(' ');
                }
                '"' => {
                    in_string = false;
                    stripped.push('"');
                }
                '\n' => stripped.push('\n'),
                _ => stripped.push(' '),
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
        }
        stripped.push(ch);
    }

    stripped
}

fn strip_test_modules(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut stripped = String::new();
    let mut index = 0;

    while index < lines.len() {
        if lines[index].trim() == "#[cfg(test)]" {
            let mut skip_until = index + 1;
            while skip_until < lines.len() && !lines[skip_until].contains('{') {
                skip_until += 1;
            }
            if skip_until < lines.len() {
                let mut depth = 0_i32;
                for (offset, line) in lines[skip_until..].iter().enumerate() {
                    for ch in line.chars() {
                        match ch {
                            '{' => depth += 1,
                            '}' => depth -= 1,
                            _ => {}
                        }
                    }
                    if depth <= 0 && offset > 0 {
                        index = skip_until + offset + 1;
                        break;
                    }
                }
                if index > skip_until {
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

fn has_unbounded_capture_shape(content: &str) -> bool {
    let captures_stream_chunks = content.contains("extend_from_slice(&buf[..n])")
        || content.contains("captured.extend_from_slice")
        || content.contains("read_to_string(&mut")
        || content.contains("wait_with_output()");
    if !captures_stream_chunks {
        return false;
    }

    let output_like = content.contains("stdout") || content.contains("stderr");
    if !output_like {
        return false;
    }

    let has_bound = content.contains("limit_bytes")
        || content.contains("retained_bytes")
        || content.contains("truncated")
        || content.contains("BoundedCapture")
        || content.contains("MAX_OUTPUT")
        || content.contains("OUTPUT_LIMIT")
        || content.contains("take(");

    !has_bound
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
}
