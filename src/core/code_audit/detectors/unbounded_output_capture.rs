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

        if has_unbounded_capture_shape(&fp.content) {
            findings.push(Finding {
                convention: "output_capture".to_string(),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description: "Command output capture appears to append stream chunks without an explicit retained-byte bound or truncation metadata.".to_string(),
                suggestion: "Use a bounded tail buffer and expose limit/seen/retained/truncated metadata in structured output.".to_string(),
                kind: AuditFinding::UnboundedOutputCapture,
            });
        }

        if has_unbounded_detail_output_shape(&fp.content) {
            findings.push(Finding {
                convention: "output_capture".to_string(),
                severity: Severity::Warning,
                file: fp.relative_path.clone(),
                description: "Reporter output appears to emit per-match or per-file details without an explicit item cap or omitted-count metadata.".to_string(),
                suggestion: "Cap detail rows with take/truncate and report how many items were omitted from the detailed output.".to_string(),
                kind: AuditFinding::UnboundedOutputCapture,
            });
        }
    }

    findings.sort_by(|a, b| a.file.cmp(&b.file));
    findings
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
    let emits_text = content.contains("println!")
        || content.contains("eprintln!")
        || content.contains("writeln!")
        || content.contains("push_str(&format!")
        || content.contains("push_str(format!");
    if !emits_text {
        return false;
    }

    let has_detail_loop = content.lines().any(|line| {
        let line = line.trim();
        if !line.starts_with("for ") || !line.contains(" in ") {
            return false;
        }

        let lower = line.to_ascii_lowercase();
        [
            "matches", "findings", "files", "details", "entries", "items",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
    });
    if !has_detail_loop {
        return false;
    }

    let has_bound = content.contains(".take(")
        || content.contains(".truncate(")
        || content.contains("omitted")
        || content.contains("remaining")
        || content.contains("detail_limit")
        || content.contains("DETAIL_LIMIT")
        || content.contains("MAX_DETAIL")
        || content.contains("max_detail")
        || content.contains("summary");

    !has_bound
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
}
