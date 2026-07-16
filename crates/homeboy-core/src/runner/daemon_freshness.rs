use crate::daemon::{DaemonFreshnessReport, DaemonStaleReasonCode};

pub(super) fn repair_or_fail(report: &DaemonFreshnessReport) -> std::result::Result<(), String> {
    if report.fresh {
        return Ok(());
    }
    let Some(code) = report.stale_reason_code else {
        return Err("daemon freshness report is stale without a reason code".to_string());
    };
    if report.restartable
        && matches!(
            code,
            DaemonStaleReasonCode::LeaseMissing
                | DaemonStaleReasonCode::PidDead
                | DaemonStaleReasonCode::BuildIdentityMismatch
                | DaemonStaleReasonCode::BinaryHashMismatch
                | DaemonStaleReasonCode::VersionMismatch
                | DaemonStaleReasonCode::RuntimePathsDrift
        )
    {
        return Ok(());
    }
    Err(format!(
        "runner daemon is stale ({code:?}) and is not automatically restartable"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(code: DaemonStaleReasonCode, restartable: bool) -> DaemonFreshnessReport {
        DaemonFreshnessReport {
            fresh: false,
            stale_reason_code: Some(code),
            restartable,
            lease_id: Some("lease".to_string()),
            pid: None,
            recovery_evidence: None,
            ownership_evidence: None,
            adoption_command: None,
            binary_hash: None,
            daemon_version: None,
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs: 0,
            termination_evidence: None,
            repair_plan: Vec::new(),
        }
    }

    #[test]
    fn repair_or_fail_decision_table() {
        for code in [
            DaemonStaleReasonCode::LeaseMissing,
            DaemonStaleReasonCode::PidDead,
            DaemonStaleReasonCode::BuildIdentityMismatch,
            DaemonStaleReasonCode::BinaryHashMismatch,
            DaemonStaleReasonCode::VersionMismatch,
            DaemonStaleReasonCode::RuntimePathsDrift,
        ] {
            assert!(repair_or_fail(&report(code, true)).is_ok(), "{code:?}");
            assert!(repair_or_fail(&report(code, false)).is_err(), "{code:?}");
        }
        for code in [
            DaemonStaleReasonCode::LeaseCorrupt,
            DaemonStaleReasonCode::LeaseSchemaMismatch,
            DaemonStaleReasonCode::TransportUnreachable,
        ] {
            assert!(repair_or_fail(&report(code, true)).is_err(), "{code:?}");
        }
    }
}
