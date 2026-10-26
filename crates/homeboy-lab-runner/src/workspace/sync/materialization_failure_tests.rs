use super::*;
use std::io::Read;
use std::process::{Command, Stdio};

#[test]
fn silent_binary_failure_has_phase_status_stage_listing_and_response() {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("silent-homeboy");
    homeboy_test_support::write_executable(
        &binary,
        "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do if [ \"$1\" = --response ]; then shift; printf 'failure-envelope' > \"$1\"; fi; shift; done\nexit 1\n",
        0o755,
    ).unwrap();
    let script = protocol_controller::materialization_script(
        root.path().to_str().unwrap(),
        binary.to_str().unwrap(),
        true,
    );
    let output = super::blobs::pipe(
        |writer| {
            let mut archive = tar::Builder::new(writer);
            archive.finish().map_err(protocol::io)
        },
        |reader| {
            Command::new("sh")
                .args(["-c", &script])
                .stdin(reader)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .map_err(protocol::io)
        },
    )
    .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("materialization failed rc=1 phase=execute"),
        "{stderr}"
    );
    assert!(stderr.contains("response.json"), "{stderr}");
    assert!(stderr.contains("failure-envelope"), "{stderr}");
    assert!(!fs::read_dir(root.path()).unwrap().any(|e| e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".materialization.")));
}

#[test]
fn producer_error_is_retained_when_consumer_also_fails() {
    let error = super::blobs::pipe::<()>(
        |_writer| Err(protocol::io("dependency source disappeared")),
        |mut reader| {
            reader.read_to_end(&mut Vec::new()).unwrap();
            let mut error = protocol::io("remote archive rejected");
            error.details["workspace_sync"] = serde_json::json!({"remote_effect_uncertain": true});
            Err(error)
        },
    )
    .unwrap_err();
    assert_eq!(
        error.details["blob_stream_producer"]["details"]["error"],
        "dependency source disappeared"
    );
    assert_eq!(
        error.details["workspace_sync"]["remote_effect_uncertain"],
        true
    );
}
