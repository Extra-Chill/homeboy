pub fn lab_offload_metadata(
    source: &str,
    runner_id: Option<&str>,
    status: &str,
    remote_workspace: Option<&str>,
    fallback_reason: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "source": source,
        "status": status,
        "runner_id": runner_id,
        "remote_workspace": remote_workspace,
        "fallback_reason": fallback_reason,
    })
}

pub fn capture_lab_offload_metadata(metadata: serde_json::Value) {
    if let Ok(raw) = serde_json::to_string(&metadata) {
        std::env::set_var(crate::core::observation::LAB_OFFLOAD_METADATA_ENV, raw);
    }
}

#[cfg(test)]
mod tests {
    use super::lab_offload_metadata;

    #[test]
    fn lab_offload_metadata_records_explicit_auto_skipped_and_fallback_states() {
        let explicit = lab_offload_metadata(
            "explicit",
            Some("lab-explicit"),
            "offloaded",
            Some("/srv/homeboy/project"),
            None,
        );
        assert_eq!(explicit["source"], "explicit");
        assert_eq!(explicit["status"], "offloaded");
        assert_eq!(explicit["runner_id"], "lab-explicit");
        assert_eq!(explicit["remote_workspace"], "/srv/homeboy/project");
        assert!(explicit["fallback_reason"].is_null());

        let fallback = lab_offload_metadata(
            "automatic",
            Some("lab"),
            "fallback",
            None,
            Some("runner connect timed out after 3s"),
        );
        assert_eq!(fallback["source"], "automatic");
        assert_eq!(fallback["status"], "fallback");
        assert_eq!(fallback["runner_id"], "lab");
        assert_eq!(
            fallback["fallback_reason"],
            "runner connect timed out after 3s"
        );

        let skipped = lab_offload_metadata(
            "automatic",
            None,
            "skipped",
            None,
            Some("no_default_runner"),
        );
        assert_eq!(skipped["source"], "automatic");
        assert_eq!(skipped["status"], "skipped");
        assert!(skipped["runner_id"].is_null());
        assert_eq!(skipped["fallback_reason"], "no_default_runner");
    }
}
