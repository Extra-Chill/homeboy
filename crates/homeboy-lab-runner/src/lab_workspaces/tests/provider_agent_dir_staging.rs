#![cfg(test)]

use std::path::Path;

use super::super::provider_agent_dir_staging::{
    is_native_binary_magic, plan_provider_agent_dir_staging,
};
use super::super::{sync_extra_lab_workspaces, ExtraLabWorkspace};

const MACH_O_ARM64: &[u8] = &[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0x00, 0x00, 0x01];

fn agent_dir_fixture(root: &Path) -> std::path::PathBuf {
    let agent = root.join("agent");
    std::fs::create_dir_all(agent.join("bin")).expect("agent bin dir");
    std::fs::create_dir_all(agent.join("prompts")).expect("agent prompts dir");
    std::fs::write(agent.join("models.json"), r#"{"providers":{}}"#).expect("models");
    std::fs::write(agent.join("settings.json"), r#"{"theme":"dark"}"#).expect("settings");
    std::fs::write(agent.join("auth.json"), "{}").expect("auth");
    std::fs::write(agent.join("prompts/review.md"), "# review").expect("prompt");
    std::fs::write(agent.join("bin/rg"), MACH_O_ARM64).expect("mach-o rg");
    agent
}

#[test]
fn provider_agent_dir_staging_detects_native_binary_magic() {
    assert!(is_native_binary_magic(b"\x7fELF\x02\x01"));
    assert!(is_native_binary_magic(b"MZ\x90\x00"));
    assert!(is_native_binary_magic(MACH_O_ARM64));
    assert!(is_native_binary_magic(&[0xfe, 0xed, 0xfa, 0xce]));
    assert!(is_native_binary_magic(&[0xca, 0xfe, 0xba, 0xbe]));
    assert!(!is_native_binary_magic(b"{\"a\":1}"));
    assert!(!is_native_binary_magic(b""));
}

#[test]
fn provider_agent_dir_staging_plan_omits_bin_and_native_binaries() {
    let controller = tempfile::tempdir().expect("controller");
    let agent = agent_dir_fixture(controller.path());
    // A stray native binary outside bin/ is withheld by magic alone.
    std::fs::write(agent.join("prompts/helper"), b"\x7fELF\x02\x01\x01").expect("elf");

    let receipt = plan_provider_agent_dir_staging(&agent).expect("staging plan");

    assert_eq!(receipt.omitted, vec!["bin/", "prompts/helper"]);
    assert_eq!(receipt.omitted_count, 2);
    assert_eq!(
        receipt.snapshot_excludes(),
        vec!["./bin/**".to_string(), "./prompts/helper".to_string()]
    );
}

#[cfg(unix)]
#[test]
fn provider_agent_dir_staging_plan_keeps_portable_executable_scripts() {
    let controller = tempfile::tempdir().expect("controller");
    let agent = agent_dir_fixture(controller.path());
    let script = agent.join("prompts/tool.sh");
    homeboy_test_support::write_executable(&script, "#!/bin/sh\n", 0o755).expect("script");

    let receipt = plan_provider_agent_dir_staging(&agent).expect("staging plan");

    // A shebang script is portable across platforms; only native binaries
    // and the platform tool cache are omitted.
    assert_eq!(receipt.omitted, vec!["bin/"]);
}

#[cfg(unix)]
#[test]
fn provider_agent_dir_staging_copies_portable_config_and_omits_mach_o_bin() {
    crate::test_support::with_isolated_home(|_| {
        let controller = tempfile::tempdir().expect("controller");
        let runner_root = tempfile::tempdir().expect("runner root");
        let primary = tempfile::tempdir().expect("primary workspace");
        let agent = agent_dir_fixture(controller.path());

        crate::create(
            &serde_json::json!({"id": "agent-dir-staging", "kind": "local",
            "workspace_root": runner_root.path()})
            .to_string(),
            false,
        )
        .expect("local runner");
        let mut mapping = Vec::new();
        let synced = sync_extra_lab_workspaces(
            "agent-dir-staging",
            primary.path().to_str().expect("primary path"),
            vec![ExtraLabWorkspace {
                role: "provider_config".to_string(),
                path: agent.canonicalize().expect("canonical agent dir"),
                snapshot_includes: Vec::new(),
                git_fetch_refs: Vec::new(),
                allow_dirty_lab_workspace: false,
                source_provenance: None,
            }],
            &mut mapping,
        )
        .expect("stage provider agent dir");
        assert_eq!(synced.len(), 1);
        assert_eq!(mapping.len(), 1);

        let staged = Path::new(mapping[0].remote_path());
        for file in [
            "models.json",
            "settings.json",
            "auth.json",
            "prompts/review.md",
        ] {
            assert_eq!(
                std::fs::read(staged.join(file)).expect("staged portable file"),
                std::fs::read(agent.join(file)).expect("source portable file"),
                "{file} must be staged"
            );
        }
        assert!(
            !staged.join("bin/rg").exists(),
            "controller-platform bin/rg must not be staged"
        );
        assert!(!staged.join("bin").exists(), "bin/ must not be staged");

        let receipt = serde_json::to_value(&mapping[0]).expect("mapping entry json");
        assert_eq!(
            receipt["staging_receipt"]["omitted"],
            serde_json::json!(["bin/"])
        );
        assert_eq!(receipt["staging_receipt"]["omitted_count"], 1);
        assert_eq!(
            receipt["staging_receipt"]["schema"],
            "homeboy/provider-agent-dir-staging/v1"
        );
    });
}

#[cfg(unix)]
#[test]
fn provider_agent_dir_staging_leaves_non_provider_workspaces_untouched() {
    crate::test_support::with_isolated_home(|_| {
        let controller = tempfile::tempdir().expect("controller");
        let runner_root = tempfile::tempdir().expect("runner root");
        let primary = tempfile::tempdir().expect("primary workspace");
        let agent = agent_dir_fixture(controller.path());

        crate::create(
            &serde_json::json!({"id": "extra-staging", "kind": "local",
            "workspace_root": runner_root.path()})
            .to_string(),
            false,
        )
        .expect("local runner");
        let mut mapping = Vec::new();
        sync_extra_lab_workspaces(
            "extra-staging",
            primary.path().to_str().expect("primary path"),
            vec![ExtraLabWorkspace {
                role: "extra".to_string(),
                path: agent.canonicalize().expect("canonical dir"),
                snapshot_includes: Vec::new(),
                git_fetch_refs: Vec::new(),
                allow_dirty_lab_workspace: false,
                source_provenance: None,
            }],
            &mut mapping,
        )
        .expect("stage extra workspace");

        assert!(Path::new(mapping[0].remote_path()).join("bin/rg").exists());
        let entry = serde_json::to_value(&mapping[0]).expect("mapping entry json");
        assert!(entry.get("staging_receipt").is_none());
    });
}
