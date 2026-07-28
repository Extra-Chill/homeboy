use super::*;

fn target(
    root: &Path,
    runner_id: &str,
    run_id: &str,
    file_name: Option<&str>,
) -> Result<RunnerDownloadTarget> {
    resolve_runner_download_target(root, runner_id, run_id, file_name, "artifact-1")
}

#[test]
fn a_canonical_download_lands_in_its_cache_directory() {
    let home = tempfile::tempdir().expect("home");
    let resolved = target(home.path(), "lab", "run-1", Some("findings.json")).expect("resolve");

    assert_eq!(
        resolved.file_path,
        home.path()
            .join("runner")
            .join("lab")
            .join("run-1")
            .join("findings.json")
    );
    assert_eq!(
        resolved.cache_dir,
        home.path().join("runner").join("lab").join("run-1")
    );
    assert_eq!(resolved.file_name, "findings.json");
}

#[test]
fn a_traversal_shaped_remote_filename_is_sanitized_into_the_cache_directory() {
    // #10586, primary vector: the daemon controls `filename` outright. Before
    // this module the join produced `<cache>/../../../../../../root/.ssh/
    // authorized_keys` and `fs::write` honoured it.
    let home = tempfile::tempdir().expect("home");
    let cache = home.path().join("runner").join("lab").join("run-1");

    for hostile in [
        "../../../../../../root/.ssh/authorized_keys",
        "..",
        "../",
        "./../../etc/passwd",
        "a/b/c",
        "..\\..\\windows",
    ] {
        let resolved = target(home.path(), "lab", "run-1", Some(hostile)).expect("resolve");
        assert_eq!(
            resolved.cache_dir, cache,
            "{hostile} must stay in the cache directory"
        );
        assert!(
            resolved.file_path.starts_with(&cache),
            "{hostile} escaped to {}",
            resolved.file_path.display()
        );
        assert_eq!(
            resolved.file_path.parent(),
            Some(cache.as_path()),
            "{hostile} produced a nested path"
        );
        assert!(!resolved.file_name.contains('/'));
        assert!(!resolved.file_name.contains('\\'));
        assert_ne!(resolved.file_name, "..");
    }
}

#[test]
fn an_absolute_remote_filename_cannot_replace_the_whole_path() {
    // `PathBuf::join` with an absolute path discards everything before it, so
    // an absolute `filename` was the shortest escape of all: no `..` needed.
    let home = tempfile::tempdir().expect("home");
    let resolved = target(
        home.path(),
        "lab",
        "run-1",
        Some("/root/.ssh/authorized_keys"),
    )
    .expect("resolve");

    assert!(resolved
        .file_path
        .starts_with(home.path().join("runner").join("lab").join("run-1")));
    assert_eq!(resolved.file_name, "root_.ssh_authorized_keys");
}

#[test]
fn a_decoded_traversal_in_the_token_ids_is_rejected_not_sanitized() {
    // #10586, second vector: `RemoteArtifactToken::parse` splits on `/` and
    // *then* percent-decodes, so its containment check runs on the encoded
    // form. `%2E%2E%2F` arrives here already decoded to `../`.
    let home = tempfile::tempdir().expect("home");

    for (runner_id, run_id) in [
        ("../../../etc", "run-1"),
        ("lab", "../../../etc"),
        ("lab", "../../../../../../root/.ssh"),
        ("/absolute", "run-1"),
        ("lab", "/absolute"),
        ("..", "run-1"),
        ("lab", ".."),
        (".", "run-1"),
        ("", "run-1"),
        ("lab", ""),
        ("lab", "run/1"),
        ("run\\1", "run-1"),
    ] {
        let error = target(home.path(), runner_id, run_id, Some("trace.zip"))
            .expect_err("traversal must be refused");
        assert!(
            error.message.contains("single path component"),
            "unexpected error for {runner_id}/{run_id}: {}",
            error.message
        );
    }

    // Nothing was created on the way to the refusal.
    assert!(!home.path().join("runner").exists());
}

#[test]
fn a_symlinked_cache_level_is_refused() {
    let home = tempfile::tempdir().expect("home");
    let outside = home.path().join("outside");
    fs::create_dir_all(&outside).expect("outside");
    let root = home.path().join("runner");
    fs::create_dir_all(&root).expect("cache root");
    std::os::unix::fs::symlink(&outside, root.join("lab")).expect("symlink");

    let error = target(home.path(), "lab", "run-1", Some("trace.zip")).expect_err("symlink");
    assert!(error.message.contains("symlink"), "{}", error.message);
}

#[test]
fn the_file_name_falls_back_to_the_artifact_id_then_to_a_constant() {
    let home = tempfile::tempdir().expect("home");

    // Empty / degenerate remote name -> sanitized artifact id.
    let from_id =
        resolve_runner_download_target(home.path(), "lab", "run-1", Some("..."), "finding-packets")
            .expect("resolve");
    assert_eq!(from_id.file_name, "finding-packets");

    // Nothing usable anywhere -> the shared constant, never an empty name.
    let fallback =
        resolve_runner_download_target(home.path(), "lab", "run-1", None, "___").expect("resolve");
    assert_eq!(fallback.file_name, FALLBACK_FILE_NAME);
}

#[test]
fn a_remote_can_never_name_the_marker_file() {
    // Trimming leading dots is what buys this: a remote that returns
    // `.homeboy-download.json` cannot forge or clobber the intent tag.
    assert_eq!(
        sanitize_download_file_name(RUNNER_DOWNLOAD_MARKER_FILE).as_deref(),
        Some("homeboy-download.json")
    );
    assert_ne!(
        sanitize_artifact_file_name(RUNNER_DOWNLOAD_MARKER_FILE),
        RUNNER_DOWNLOAD_MARKER_FILE
    );
}

#[test]
fn sanitize_matches_the_pull_dir_helper_it_replaces() {
    // Behaviour parity with the former `runs/handlers.rs::sanitize_artifact_filename`.
    assert_eq!(
        sanitize_artifact_file_name("finding-packets.json"),
        "finding-packets.json"
    );
    assert_eq!(
        sanitize_artifact_file_name("../../etc/passwd"),
        "etc_passwd"
    );
    assert_eq!(sanitize_artifact_file_name("a/b\\c"), "a_b_c");
    assert_eq!(sanitize_artifact_file_name("..."), FALLBACK_FILE_NAME);
    assert_eq!(sanitize_artifact_file_name(""), FALLBACK_FILE_NAME);
}

#[test]
fn an_untagged_cache_directory_reads_as_operator_owned() {
    let home = tempfile::tempdir().expect("home");
    let cache = home.path().join("runner").join("lab").join("run-1");
    fs::create_dir_all(&cache).expect("cache");

    let ownership = read_download_ownership(&cache);
    assert_eq!(ownership, RunnerDownloadOwnership::Unrecorded);
    assert!(!ownership.is_reclaimable());
    assert!(ownership.retain_reason().is_some());
}

#[test]
fn an_unparseable_marker_reads_as_operator_owned() {
    let home = tempfile::tempdir().expect("home");
    let cache = home.path().join("runner").join("lab").join("run-1");
    fs::create_dir_all(&cache).expect("cache");
    fs::write(cache.join(RUNNER_DOWNLOAD_MARKER_FILE), b"{ not json").expect("marker");

    assert_eq!(
        read_download_ownership(&cache),
        RunnerDownloadOwnership::Unreadable
    );

    // A marker that parses as JSON but states no intent is equally unreadable:
    // `intent` deliberately has no serde default.
    fs::write(
        cache.join(RUNNER_DOWNLOAD_MARKER_FILE),
        b"{\"schema\":\"x\"}",
    )
    .expect("marker");
    assert_eq!(
        read_download_ownership(&cache),
        RunnerDownloadOwnership::Unreadable
    );

    // An unknown intent value is not silently coerced either.
    fs::write(
        cache.join(RUNNER_DOWNLOAD_MARKER_FILE),
        b"{\"intent\":\"whatever\"}",
    )
    .expect("marker");
    assert_eq!(
        read_download_ownership(&cache),
        RunnerDownloadOwnership::Unreadable
    );
}

#[test]
fn only_an_explicit_internal_fetch_is_reclaimable() {
    let home = tempfile::tempdir().expect("home");
    let cache = home.path().join("runner").join("lab").join("run-1");
    fs::create_dir_all(&cache).expect("cache");

    record_download_intent(&cache, RunnerDownloadIntent::InternalFetch, "artifact-1");
    assert_eq!(
        read_download_ownership(&cache),
        RunnerDownloadOwnership::Tagged(RunnerDownloadIntent::InternalFetch)
    );
    assert!(read_download_ownership(&cache).is_reclaimable());

    record_download_intent(&cache, RunnerDownloadIntent::OperatorPull, "artifact-2");
    assert!(!read_download_ownership(&cache).is_reclaimable());
}

#[test]
fn operator_ownership_is_sticky_across_later_internal_fetches() {
    let home = tempfile::tempdir().expect("home");
    let cache = home.path().join("runner").join("lab").join("run-1");
    fs::create_dir_all(&cache).expect("cache");

    record_download_intent(&cache, RunnerDownloadIntent::OperatorPull, "artifact-1");
    record_download_intent(&cache, RunnerDownloadIntent::InternalFetch, "artifact-2");

    assert_eq!(
        read_download_ownership(&cache),
        RunnerDownloadOwnership::Tagged(RunnerDownloadIntent::OperatorPull)
    );
}

#[test]
fn the_marker_records_provenance_and_stays_additive() {
    let home = tempfile::tempdir().expect("home");
    let cache = home.path().join("runner").join("lab").join("run-1");
    fs::create_dir_all(&cache).expect("cache");

    record_download_intent(&cache, RunnerDownloadIntent::InternalFetch, "artifact-1");
    record_download_intent(&cache, RunnerDownloadIntent::InternalFetch, "artifact-2");
    record_download_intent(&cache, RunnerDownloadIntent::InternalFetch, "artifact-1");

    let raw = fs::read_to_string(cache.join(RUNNER_DOWNLOAD_MARKER_FILE)).expect("marker");
    let marker: RunnerDownloadMarker = serde_json::from_str(&raw).expect("parse");
    assert_eq!(marker.schema, RUNNER_DOWNLOAD_MARKER_SCHEMA);
    assert_eq!(marker.intent, RunnerDownloadIntent::InternalFetch);
    assert_eq!(marker.artifact_ids, vec!["artifact-1", "artifact-2"]);
    assert!(marker.first_fetched_at.is_some());
    assert!(marker.last_fetched_at.is_some());

    // Unknown fields from a newer writer must not make the marker unreadable:
    // this file crosses independently shipped builds.
    let forward = format!(
        "{{\"schema\":\"{RUNNER_DOWNLOAD_MARKER_SCHEMA}\",\"intent\":\"internal_fetch\",\"a_future_field\":1}}"
    );
    fs::write(cache.join(RUNNER_DOWNLOAD_MARKER_FILE), forward).expect("marker");
    assert_eq!(
        read_download_ownership(&cache),
        RunnerDownloadOwnership::Tagged(RunnerDownloadIntent::InternalFetch)
    );
}

#[test]
fn a_marker_that_cannot_be_written_leaves_the_cache_untagged() {
    // Fail closed in the only direction that matters: no marker retains.
    let home = tempfile::tempdir().expect("home");
    let cache = home.path().join("runner").join("lab").join("run-1");
    fs::create_dir_all(&cache).expect("cache");
    record_download_intent(&cache, RunnerDownloadIntent::InternalFetch, "artifact-1");
    assert!(read_download_ownership(&cache).is_reclaimable());

    // A directory where the marker file should be cannot be written or parsed.
    fs::remove_file(cache.join(RUNNER_DOWNLOAD_MARKER_FILE)).expect("remove marker");
    fs::create_dir(cache.join(RUNNER_DOWNLOAD_MARKER_FILE)).expect("marker as directory");
    assert_eq!(
        read_download_ownership(&cache),
        RunnerDownloadOwnership::Unreadable
    );
    record_download_intent(&cache, RunnerDownloadIntent::InternalFetch, "artifact-2");
    assert!(!read_download_ownership(&cache).is_reclaimable());
}
