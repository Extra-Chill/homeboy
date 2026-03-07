//! Task building from verification results.
//!
//! Converts verification results into actionable tasks that agents can execute
//! step-by-step without interpretation.

use super::claims::{Claim, ClaimConfidence, ClaimType};
use super::verify::VerifyResult;

/// Status of an audit task.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditTaskStatus {
    /// Claim verified - no action needed
    Verified,
    /// Claim is broken - needs fixing
    Broken,
    /// Cannot verify mechanically - agent must check
    NeedsVerification,
}

/// An actionable task for documentation maintenance.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditTask {
    /// Documentation file containing the claim
    pub doc: String,
    /// Line number in the doc file
    pub line: usize,
    /// The claim being verified (human-readable)
    pub claim: String,
    /// Type of claim for filtering/grouping
    #[serde(rename = "type")]
    pub claim_type: ClaimType,
    /// Raw value extracted from the claim
    pub claim_value: String,
    /// Confidence that this claim is a real reference vs. example/placeholder
    pub confidence: ClaimConfidence,
    /// Verification status
    pub status: AuditTaskStatus,
    /// Action to take (null if verified)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// Build an actionable task from a claim and its verification result.
pub(crate) fn build_task(claim: Claim, result: VerifyResult) -> AuditTask {
    let (status, action) = match result {
        VerifyResult::Verified => (AuditTaskStatus::Verified, None),
        VerifyResult::Broken { suggestion } => (AuditTaskStatus::Broken, suggestion),
        VerifyResult::NeedsVerification { hint } => {
            (AuditTaskStatus::NeedsVerification, Some(hint))
        }
    };

    // Build human-readable claim description
    let claim_description = build_claim_description(&claim);

    AuditTask {
        doc: claim.doc_file,
        line: claim.line,
        claim: claim_description,
        claim_type: claim.claim_type,
        claim_value: claim.value,
        confidence: claim.confidence,
        status,
        action,
    }
}

/// Build a human-readable description of the claim.
fn build_claim_description(claim: &Claim) -> String {
    match claim.claim_type {
        ClaimType::FilePath => format!("file path `{}`", claim.value),
        ClaimType::DirectoryPath => format!("directory path `{}`", claim.value),
        ClaimType::CodeExample => {
            let preview = if claim.value.len() > 50 {
                format!("{}...", &claim.value[..50])
            } else {
                claim.value.clone()
            };
            format!("code example: {}", preview.replace('\n', " "))
        }
        ClaimType::ClassName => format!("class reference `{}`", claim.value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_verified_task() {
        let claim = Claim {
            claim_type: ClaimType::FilePath,
            value: "/src/main.rs".to_string(),
            doc_file: "docs/index.md".to_string(),
            line: 10,
            confidence: ClaimConfidence::Real,
            context: None,
        };

        let task = build_task(claim, VerifyResult::Verified);

        assert_eq!(task.status, AuditTaskStatus::Verified);
        assert!(task.action.is_none());
        assert_eq!(task.claim, "file path `/src/main.rs`");
    }

    #[test]
    fn test_build_broken_task() {
        let claim = Claim {
            claim_type: ClaimType::FilePath,
            value: "/src/old.rs".to_string(),
            doc_file: "docs/index.md".to_string(),
            line: 15,
            confidence: ClaimConfidence::Real,
            context: None,
        };

        let task = build_task(
            claim,
            VerifyResult::Broken {
                suggestion: Some("File was renamed to /src/new.rs".to_string()),
            },
        );

        assert_eq!(task.status, AuditTaskStatus::Broken);
        assert!(task.action.is_some());
        assert!(task.action.unwrap().contains("renamed"));
    }

    #[test]
    fn test_build_needs_verification_task() {
        let claim = Claim {
            claim_type: ClaimType::CodeExample,
            value: "fn process() { }".to_string(),
            doc_file: "docs/api.md".to_string(),
            line: 42,
            confidence: ClaimConfidence::Unclear,
            context: None,
        };

        let task = build_task(
            claim,
            VerifyResult::NeedsVerification {
                hint: "Verify code example syntax matches current API.".to_string(),
            },
        );

        assert_eq!(task.status, AuditTaskStatus::NeedsVerification);
        assert!(task.action.is_some());
    }

    #[test]
    fn test_claim_description_truncation() {
        let long_code = "function example() {\n    // This is a very long code example that should be truncated\n    return true;\n}";
        let claim = Claim {
            claim_type: ClaimType::CodeExample,
            value: long_code.to_string(),
            doc_file: "docs/test.md".to_string(),
            line: 1,
            confidence: ClaimConfidence::Unclear,
            context: None,
        };

        let description = build_claim_description(&claim);
        assert!(description.contains("..."));
        assert!(!description.contains('\n'));
    }
}
