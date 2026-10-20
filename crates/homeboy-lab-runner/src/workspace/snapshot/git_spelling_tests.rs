use super::*;
use std::process::Command;

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Simulate what a case-insensitive filesystem walk reports, independent of
/// the host filesystem: the index tracks `tracked` spellings while the files
/// physically exist only under the `on_disk` spellings. `core.ignorecase`
/// makes Git treat the on-disk spellings as the tracked entries, exactly as
/// it does on macOS APFS.
fn case_folded_checkout(entries: &[(&str, &str, &[u8])]) -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("checkout");
    git(root.path(), &["init", "-q", "."]);
    git(root.path(), &["config", "core.ignorecase", "true"]);
    for (tracked, on_disk, content) in entries {
        let path = root.path().join(on_disk);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        let blob = git(root.path(), &["hash-object", "-w", on_disk]);
        git(
            root.path(),
            &[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{blob},{tracked}"),
            ],
        );
    }
    root
}

fn host_filesystem_is_case_sensitive() -> bool {
    let probe = tempfile::tempdir().unwrap();
    fs::write(probe.path().join("case-probe"), b"").unwrap();
    !probe.path().join("CASE-PROBE").exists()
}

#[test]
fn case_folded_walk_records_git_tracked_spelling() {
    let checkout = case_folded_checkout(&[
        (
            "tests/fixtures/a.php",
            "tests/Fixtures/a.php",
            b"<?php // a\n",
        ),
        (
            "tests/Fixtures/B.php",
            "tests/Fixtures/B.php",
            b"<?php // B\n",
        ),
    ]);
    let root = checkout.path();
    let manifest = snapshot_input_manifest(root, &[]).expect("input manifest");

    let inventory = manifest
        .stable_manifest
        .inventory
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.kind.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        inventory,
        vec![
            ("tests", "directory"),
            ("tests/Fixtures", "directory"),
            ("tests/Fixtures/B.php", "file"),
            ("tests/fixtures", "directory"),
            ("tests/fixtures/a.php", "file"),
        ]
    );
    assert_eq!(manifest.stable_manifest.inventory.entry_count, 5);

    let selected = manifest
        .selection
        .iter()
        .filter(|entry| !entry.relative.starts_with(".git"))
        .map(|entry| (entry.relative.clone(), entry.kind.as_str()))
        .collect::<Vec<_>>();
    assert!(selected.contains(&(PathBuf::from("tests/fixtures/a.php"), "file")));
    assert!(selected.contains(&(PathBuf::from("tests/fixtures"), "directory")));
    assert!(!selected
        .iter()
        .any(|(relative, _)| relative == Path::new("tests/Fixtures/a.php")));
    let selected_a = manifest
        .selection
        .iter()
        .find(|entry| entry.relative == Path::new("tests/fixtures/a.php"))
        .unwrap();
    assert_eq!(fs::read(&selected_a.source).unwrap(), b"<?php // a\n");

    // The content identity is the digest of the correctly-cased tree a
    // case-sensitive runner recomputes.
    let (algorithm, capability) =
        workspace_content_hash_contract(WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY).unwrap();
    let mut expected = Sha256::new();
    expected.update(algorithm.as_bytes());
    expected.update(b"\0");
    for (path, on_disk, content) in [
        ("tests", None, None),
        ("tests/Fixtures", None, None),
        (
            "tests/Fixtures/B.php",
            Some("tests/Fixtures/B.php"),
            Some(&b"<?php // B\n"[..]),
        ),
        ("tests/fixtures", None, None),
        (
            "tests/fixtures/a.php",
            Some("tests/Fixtures/a.php"),
            Some(&b"<?php // a\n"[..]),
        ),
    ] {
        expected.update(path.as_bytes());
        match (on_disk, content) {
            (Some(on_disk), Some(content)) => {
                expected.update(b"\0file\0");
                let metadata = fs::metadata(root.join(on_disk)).unwrap();
                if let Some(executable) = capability.value(&metadata) {
                    expected.update([executable as u8]);
                }
                expected.update((content.len() as u64).to_le_bytes());
                expected.update(content);
            }
            _ => expected.update(b"\0dir\0"),
        }
    }
    let expected = format!("sha256:{:x}", expected.finalize());
    assert_eq!(manifest.stable_manifest.content_identity, expected);

    if host_filesystem_is_case_sensitive() {
        let correct = tempfile::tempdir().unwrap();
        fs::create_dir_all(correct.path().join("tests/fixtures")).unwrap();
        fs::create_dir_all(correct.path().join("tests/Fixtures")).unwrap();
        fs::write(correct.path().join("tests/fixtures/a.php"), b"<?php // a\n").unwrap();
        fs::write(correct.path().join("tests/Fixtures/B.php"), b"<?php // B\n").unwrap();
        assert_eq!(
            workspace_content_hash(correct.path(), &[]).unwrap(),
            expected
        );
    }

    // The staged copy reproduces the same manifest, so stability holds.
    let scratch = tempfile::tempdir().unwrap();
    let stage = materialize_snapshot_stage(root, &manifest, Some(scratch.path())).unwrap();
    let staged = stage.path().join("source");
    let staged_manifest = snapshot_stable_manifest(&staged, &[]).unwrap();
    let current = snapshot_stable_manifest(root, &[]).unwrap();
    validate_snapshot_stability(
        &manifest.stable_manifest,
        &staged_manifest,
        &current,
        root,
        &staged,
    )
    .expect("stable respelled snapshot");
}

#[test]
fn ambiguous_case_folded_walk_fails_closed_naming_both_tracked_paths() {
    let checkout = case_folded_checkout(&[
        ("tests/fixtures/a.php", "tests/FIXTURES/a.php", b"lower\n"),
        ("tests/Fixtures/a.php", "tests/FIXTURES/a.php", b"lower\n"),
    ]);
    let error = snapshot_input_manifest(checkout.path(), &[]).unwrap_err();
    let message = error.message.clone();
    assert!(
        message.contains("`tests/Fixtures/a.php`") && message.contains("`tests/fixtures/a.php`"),
        "{message}"
    );
    assert!(message.contains("tests/FIXTURES/a.php"), "{message}");
    assert!(snapshot_stable_manifest(checkout.path(), &[]).is_err());
}

#[test]
fn walked_spelling_matching_git_is_unchanged() {
    let known = vec!["src/Lib.php".to_string(), "untracked.txt".to_string()];
    let walked = ["src", "src/Lib.php", "untracked.txt", "vendor", "vendor/x"]
        .iter()
        .map(|path| WorkspaceContentManifestEntry {
            path: (*path).to_string(),
            kind: if path.contains('.') || *path == "vendor/x" {
                "file"
            } else {
                "directory"
            }
            .to_string(),
            sha256: None,
            bytes: None,
            owner_executable: None,
        })
        .collect::<Vec<_>>();
    let spelling = git_spelling::tracked_spelling_for_walk(&known, &walked).unwrap();
    assert_eq!(
        spelling,
        walked
            .iter()
            .map(|entry| Some(entry.path.clone()))
            .collect::<Vec<_>>()
    );
}
