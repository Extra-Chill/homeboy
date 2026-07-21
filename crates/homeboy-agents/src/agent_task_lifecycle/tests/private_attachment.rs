use super::*;
use crate::agent_task_lifecycle::{load_private_run_attachment, persist_private_run_attachment};
use serde_json::{json, Value};

fn submit_attachment_run(run_id: &str) {
    submit_plan(&test_plan(), Some(run_id)).expect("submit attachment run");
}

fn attachment_path(run_id: &str, kind: &str) -> std::path::PathBuf {
    homeboy_core::paths::homeboy_data()
        .expect("data root")
        .join("agent-task-runs")
        .join(run_id)
        .join("private")
        .join(format!("{kind}.json"))
}

#[test]
fn private_attachments_require_an_authoritative_safe_run_and_kind() {
    with_isolated_home(|_| {
        let payload = json!({"value": "secret-marker"});
        for (run_id, kind) in [
            ("unknown", "recipe"),
            ("bad/../run", "recipe"),
            ("unknown", "../recipe"),
        ] {
            let error = persist_private_run_attachment(run_id, kind, &payload)
                .expect_err("reject unsafe attachment");
            assert!(!error.message.contains("secret-marker"));
            assert!(!error.message.contains("/Users/"));
        }
    });
}

#[test]
fn private_attachment_is_idempotent_and_conflicts_without_leaking_payload() {
    with_isolated_home(|_| {
        submit_attachment_run("attachment-replay");
        let payload = json!({"nested": {"value": "first-secret-marker"}});
        let first = persist_private_run_attachment("attachment-replay", "recipe", &payload)
            .expect("persist");
        assert_eq!(
            persist_private_run_attachment("attachment-replay", "recipe", &payload)
                .expect("replay"),
            first
        );
        let error = persist_private_run_attachment(
            "attachment-replay",
            "recipe",
            &json!({"value": "second-secret-marker"}),
        )
        .expect_err("conflict");
        assert!(!error.message.contains("first-secret-marker"));
        assert!(!error.message.contains("second-secret-marker"));
    });
}

#[test]
fn private_attachment_canonicalizes_unordered_maps() {
    with_isolated_home(|_| {
        submit_attachment_run("attachment-canonical");
        let first = json!({"z": [{"b": 2, "a": 1}], "a": {"d": 4, "c": 3}});
        let reordered = json!({"a": {"c": 3, "d": 4}, "z": [{"a": 1, "b": 2}]});
        let persisted = persist_private_run_attachment("attachment-canonical", "recipe", &first)
            .expect("persist");
        let replay = persist_private_run_attachment("attachment-canonical", "recipe", &reordered)
            .expect("canonical replay");
        assert_eq!(persisted.payload_digest, replay.payload_digest);
    });
}

#[test]
fn private_attachment_rejects_tampered_envelope_bindings_and_digest() {
    with_isolated_home(|_| {
        submit_attachment_run("attachment-tamper");
        for (kind, field, value) in [
            ("schema", "schema", json!("wrong")),
            ("run", "run_id", json!("other")),
            ("kind", "kind", json!("other")),
            ("digest", "payload_digest", json!("sha256:wrong")),
        ] {
            persist_private_run_attachment("attachment-tamper", kind, &json!({"value": 1}))
                .expect("persist");
            let path = attachment_path("attachment-tamper", kind);
            let mut tampered: Value =
                serde_json::from_slice(&std::fs::read(&path).expect("read attachment"))
                    .expect("parse attachment");
            tampered[field] = value;
            std::fs::write(&path, serde_json::to_vec(&tampered).expect("encode tamper"))
                .expect("tamper attachment");
            assert!(
                load_private_run_attachment::<Value>("attachment-tamper", kind).is_err(),
                "{field} tamper rejected"
            );
        }
    });
}

#[test]
fn concurrent_conflicting_private_attachment_writers_commit_once() {
    with_isolated_home(|_| {
        submit_attachment_run("attachment-concurrent");
        let threads: Vec<_> = [json!({"winner": "one"}), json!({"winner": "two"})]
            .into_iter()
            .map(|payload| {
                std::thread::spawn(move || {
                    persist_private_run_attachment("attachment-concurrent", "recipe", &payload)
                })
            })
            .collect();
        let results: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().expect("writer thread"))
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let stored = load_private_run_attachment::<Value>("attachment-concurrent", "recipe")
            .expect("load winner");
        assert!(
            stored.payload == json!({"winner":"one"}) || stored.payload == json!({"winner":"two"})
        );
    });
}

#[cfg(unix)]
#[test]
fn private_attachment_has_owner_permissions_and_rejects_symlink_directories() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    with_isolated_home(|_| {
        submit_attachment_run("attachment-permissions");
        persist_private_run_attachment("attachment-permissions", "recipe", &json!({"value": 1}))
            .expect("persist");
        let path = attachment_path("attachment-permissions", "recipe");
        assert_eq!(
            std::fs::metadata(&path)
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().expect("private dir"))
                .expect("dir metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let run_dir = path
            .parent()
            .expect("private dir")
            .parent()
            .expect("run dir");
        assert_eq!(
            std::fs::metadata(run_dir)
                .expect("run metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        submit_attachment_run("attachment-symlink");
        let unsafe_private = attachment_path("attachment-symlink", "recipe")
            .parent()
            .expect("private path")
            .to_path_buf();
        let target = tempfile::tempdir().expect("symlink target");
        symlink(target.path(), &unsafe_private).expect("create unsafe symlink");
        assert!(persist_private_run_attachment(
            "attachment-symlink",
            "recipe",
            &json!({"value": 1})
        )
        .is_err());
    });
}
