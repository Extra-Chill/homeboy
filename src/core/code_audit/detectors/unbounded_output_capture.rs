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
}
