use super::*;

#[test]
fn cancelling_either_runner_call_reaps_its_child_scope() {
    crate::test_support::with_isolated_home(|_| {
        let fixture = tempfile::tempdir().unwrap();
        let bin = fixture.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let accepted = fixture.path().join("accepted");
        let descendant = fixture.path().join("pid");
        homeboy_test_support::write_executable(&bin.join("ssh"), &format!(
            "#!/bin/sh\nsleep 60 & child=$!\nprintf '%s' \"$child\" > {}\nprintf accepted > {}\nwait\n",
            shell::quote_arg(&descendant.display().to_string()), shell::quote_arg(&accepted.display().to_string()),
        ), 0o755).unwrap();
        homeboy_core::server::create(
            r#"{"id":"protocol-transport","host":"materialization.invalid","user":"test"}"#,
            false,
        )
        .unwrap();
        crate::create(&serde_json::json!({"id":"protocol-transport","kind":"ssh","server_id":"protocol-transport","workspace_root":fixture.path(),"homeboy_path":"/runner/homeboy"}).to_string(), false).unwrap();
        let runner = load("protocol-transport").unwrap();
        fs::write(fixture.path().join("plan.json"), b"{}").unwrap();
        let old_path = std::env::var("PATH").unwrap();
        std::env::set_var("PATH", format!("{}:{old_path}", bin.display()));
        fs::write(fixture.path().join("commit.json"), b"{}").unwrap();
        let sources = std::collections::BTreeMap::new();
        for commit in [false, true] {
            let marker = accepted.clone();
            let control =
                WorkspaceControl::new(homeboy_core::cooperative_control::CooperativeControl::new(
                    Instant::now() + Duration::from_secs(10),
                    std::sync::Arc::new(move || marker.exists()),
                ));
            let error = protocol_controller::invoke(
                &runner,
                fixture.path(),
                commit.then_some((&sources, &[])),
                &control,
            )
            .unwrap_err();
            assert_eq!(error.details["workspace_sync"]["cancelled"], true);
            if commit {
                assert_eq!(
                    error.details["workspace_sync"]["remote_effect_uncertain"],
                    true
                );
            }
            let pid = fs::read_to_string(&descendant)
                .unwrap()
                .parse::<u32>()
                .unwrap();
            assert!(
                !homeboy_core::process::pid_is_running(pid),
                "runner child leaked on cancellation"
            );
            fs::remove_file(&accepted).unwrap();
        }
        std::env::set_var("PATH", old_path);
    });
}

/// Member names and pax records of a ustar stream, read without a tar
/// implementation of the host platform (macOS `bsdtar -t` hides AppleDouble
/// members that GNU tar on a Linux runner extracts as ordinary files).
fn tar_members(archive: &[u8]) -> (Vec<String>, Vec<String>) {
    let (mut names, mut pax) = (Vec::new(), Vec::new());
    let mut offset = 0;
    while offset + 512 <= archive.len() && archive[offset..offset + 512].iter().any(|b| *b != 0) {
        let header = &archive[offset..offset + 512];
        let field = |range: std::ops::Range<usize>| {
            String::from_utf8_lossy(&header[range])
                .trim_end_matches('\0')
                .trim()
                .to_string()
        };
        let size = usize::from_str_radix(&field(124..136), 8).unwrap_or(0);
        let body = &archive[offset + 512..(offset + 512 + size).min(archive.len())];
        match header[156] {
            b'x' | b'g' => pax.push(String::from_utf8_lossy(body).into_owned()),
            _ => names.push(field(0..100)),
        }
        offset += 512 + size.div_ceil(512) * 512;
    }
    (names, pax)
}

/// Blob streams carry no platform sidecars, even when source files have
/// extended attributes (macOS `com.apple.provenance`).
#[test]
fn commit_archive_carries_no_platform_metadata_members() {
    crate::test_support::with_isolated_home(|_| {
        let fixture = tempfile::tempdir().unwrap();
        let bin = fixture.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let archive = fixture.path().join("archive.tar");
        homeboy_test_support::write_executable(
            bin.join("ssh"),
            format!(
                "#!/bin/sh\ncat > {}\nprintf '{{}}'\n",
                shell::quote_arg(&archive.display().to_string())
            ),
            0o755,
        )
        .unwrap();
        homeboy_core::server::create(
            r#"{"id":"protocol-archive","host":"materialization.invalid","user":"test"}"#,
            false,
        )
        .unwrap();
        crate::create(&serde_json::json!({"id":"protocol-archive","kind":"ssh","server_id":"protocol-archive","workspace_root":fixture.path(),"homeboy_path":"/runner/homeboy"}).to_string(), false).unwrap();
        let runner = load("protocol-archive").unwrap();
        let directory = fixture.path().join("commit");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("commit.json"), b"{}").unwrap();
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("file.txt");
        fs::write(&file, b"bytes\n").unwrap();
        // Best effort: only hosts with `xattr` (macOS) can tag the file.
        let _ = std::process::Command::new("xattr")
            .args(["-w", "com.example.provenance", "1"])
            .arg(&file)
            .status();
        let old_path = std::env::var("PATH").unwrap();
        std::env::set_var("PATH", format!("{}:{old_path}", bin.display()));
        let digest = format!(
            "sha256:{}",
            super::super::snapshot::snapshot_file_sha256(&file, &WorkspaceControl::default())
                .unwrap()
        );
        let mut sources = std::collections::BTreeMap::new();
        sources.insert(
            digest.clone(),
            super::blobs::Source::new(file, digest.clone()).unwrap(),
        );
        let result = protocol_controller::invoke(
            &runner,
            &directory,
            Some((&sources, std::slice::from_ref(&digest))),
            &WorkspaceControl::default(),
        );
        std::env::set_var("PATH", old_path);
        result.unwrap();
        let (names, pax) = tar_members(&fs::read(&archive).unwrap());
        let mut names: Vec<_> = names
            .into_iter()
            .map(|name| {
                name.trim_start_matches("./")
                    .trim_end_matches('/')
                    .to_string()
            })
            .filter(|name| !name.is_empty())
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                format!("blobs/{}", super::blobs::key(&digest).unwrap()),
                "commit.json".into()
            ]
        );
        assert!(
            pax.iter().all(|record| !record.contains("xattr")),
            "{pax:?}"
        );
    });
}
