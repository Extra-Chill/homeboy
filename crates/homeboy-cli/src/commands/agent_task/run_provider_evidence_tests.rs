use super::*;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[cfg(unix)]
#[test]
fn streams_binary_artifacts_at_the_configured_limit_and_rejects_the_next_byte() {
    let temp = tempfile::tempdir().expect("temporary workspace");
    let exact = temp.path().join("exact.bin");
    let above = temp.path().join("above.bin");
    std::fs::write(&exact, [0u8, 1, 2, 255]).expect("write exact fixture");
    std::fs::write(&above, [0u8, 1, 2, 255, 3]).expect("write above-limit fixture");

    let destination = temp.path().join("provider-evidence/staging/exact.bin");
    let exact_admitted =
        admit_provider_evidence_source(exact.to_str().expect("exact fixture path is UTF-8"))
            .expect("admit exact fixture");
    let above_admitted =
        admit_provider_evidence_source(above.to_str().expect("above-limit fixture path is UTF-8"))
            .expect("admit above-limit fixture");
    let (size, digest) = secure_provider_evidence_copy_with_limit(&exact_admitted, &destination, 4)
        .expect("exact boundary is admitted");
    assert_eq!(size, 4);
    assert_eq!(
        digest,
        "sha256:3d1f57c984978ef98a18378c8166c1cb8ede02c03eeb6aee7e2f121dfeee3e56"
    );
    assert_eq!(
        std::fs::read(&destination).expect("read binary handoff"),
        [0, 1, 2, 255]
    );

    let error = secure_provider_evidence_copy_with_limit(
        &above_admitted,
        &temp.path().join("provider-evidence/staging/above.bin"),
        4,
    )
    .expect_err("one byte above the limit is rejected");
    assert_eq!(error.details["limit_bytes"], 4);
    assert_eq!(error.details["actual_bytes"], 5);
    assert_eq!(error.details["transport"], "content-addressed-blob/v1");
    assert_eq!(
        error.details["tried"]
            .as_array()
            .expect("remediation")
            .len(),
        1
    );
}

#[test]
fn stages_declared_file_outside_candidate_and_reuses_controller_owned_blob() {
    let temp = tempfile::tempdir().expect("temporary workspace");
    let source = temp.path().join("external.json");
    std::fs::write(&source, "{\"accepted\":true}").expect("write evidence");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).expect("create workspace");
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .args(args)
            .current_dir(&workspace)
            .output()
            .expect("run git");
        assert!(output.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    git(&["init", "--quiet", "--initial-branch=main"]);
    std::fs::write(workspace.join("tracked.txt"), "base\n").expect("write candidate base");
    git(&["add", "tracked.txt"]);
    git(&[
        "-c",
        "user.name=Homeboy Test",
        "-c",
        "user.email=homeboy@example.test",
        "commit",
        "--quiet",
        "-m",
        "base",
    ]);
    let source = source.canonicalize().expect("canonical source");
    let workspace = workspace.canonicalize().expect("canonical workspace");
    let input = AgentTaskProviderEvidenceInput {
        id: "acceptance".to_string(),
        source: source.display().to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
    };

    let admitted = admit_provider_evidence_inputs(&[input.clone()]).expect("admit evidence");
    let store = temp.path().join("provider-evidence");
    let projected =
        project_admitted_provider_evidence_inputs_at(&[input.clone()], &admitted, &store)
            .expect("project declared evidence");
    let path = PathBuf::from(projected[0]["path"].as_str().expect("projected path"));
    assert!(!path.starts_with(&workspace));
    assert!(!workspace.join(".homeboy").exists());
    assert!(git(&["status", "--porcelain=v1"]).is_empty());
    assert_eq!(
        std::fs::read_to_string(&path).expect("read projected evidence"),
        "{\"accepted\":true}"
    );
    assert!(std::fs::metadata(&path)
        .expect("evidence metadata")
        .permissions()
        .readonly());
    assert_eq!(
        projected[0]["sha256"],
        "sha256:11a49f853eb8befe94fef278d487125cd20930b9e41c4c0934394443e7f00878"
    );
    assert_eq!(projected[0]["transport"], "content-addressed-blob/v1");
    assert_eq!(projected[0]["visibility"], "private");
    assert_eq!(projected[0]["redaction"], "withhold-content");
    assert_eq!(
        projected[0]["ownership"]["owner"],
        "controller-artifact-store"
    );
    assert_eq!(projected[0]["provenance"]["source_name"], "external.json");
    assert!(!serde_json::to_string(&projected)
        .expect("serialize projection")
        .contains(&source.display().to_string()));
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(&path)
            .expect("evidence mode")
            .permissions()
            .mode()
            & 0o777,
        0o400
    );

    let mut prompt = Some(format!("Read {} before editing.", source.display()));
    let projected_paths = projected_provider_evidence_paths(&projected);
    rewrite_provider_evidence_prompt(
        &mut prompt,
        &[input.clone()],
        &admitted,
        &projected,
        &projected_paths,
    )
    .expect("rewrite evidence prompt");
    assert_eq!(
        prompt.expect("rewritten prompt"),
        format!("Read {} before editing.", path.display())
    );

    let retry = project_admitted_provider_evidence_inputs_at(&[input], &admitted, &store)
        .expect("retry reuses staged evidence");
    assert_eq!(retry[0]["path"], projected[0]["path"]);
    assert_eq!(
        std::fs::read_dir(store.join("files"))
            .expect("isolated file store")
            .count(),
        1
    );
    std::fs::remove_dir_all(&store).expect("controller owner reclaims evidence store");
    assert!(git(&["status", "--porcelain=v1"]).is_empty());
}

#[test]
fn projected_binary_digest_detects_post_admission_mutation_without_exposing_content() {
    let temp = tempfile::tempdir().expect("temporary workspace");
    let source = temp.path().join("fixture.bin");
    let workspace = temp.path().join("workspace");
    std::fs::write(&source, [0, 159, 146, 150, 255]).expect("write binary fixture");
    std::fs::create_dir(&workspace).expect("create workspace");
    let input = AgentTaskProviderEvidenceInput {
        id: "fixture".to_string(),
        source: source
            .canonicalize()
            .expect("canonical source")
            .display()
            .to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
    };

    let admitted = admit_provider_evidence_inputs(&[input.clone()]).expect("admit fixture");
    let projected = project_admitted_provider_evidence_inputs_at(
        &[input],
        &admitted,
        &temp.path().join("provider-evidence"),
    )
    .expect("project fixture");
    let path = PathBuf::from(projected[0]["path"].as_str().expect("path"));
    #[cfg(unix)]
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("simulate a controller-side mutation");
    std::fs::write(&path, b"mutated").expect("mutate after admission");
    let expected = projected[0]["sha256"]
        .as_str()
        .expect("digest")
        .trim_start_matches("sha256:");
    let actual = homeboy_engine_primitives::content_hash::sha256_hex(
        &std::fs::read(&path).expect("read mutated fixture"),
    );
    assert_ne!(
        actual, expected,
        "Lab handoff verifies this declared digest before publish"
    );
    let output = serde_json::to_string(&projected).expect("serialize projection");
    assert!(!output.contains("\u{0}"));
    assert!(output.len() < 1_024, "JSON carries refs, not fixture bytes");
}

#[test]
fn rejects_undeclared_absolute_prompt_path_before_provider_admission() {
    let error = validate_provider_evidence_inputs(&[], Some("Read /private/evidence.json."))
        .expect_err("undeclared prompt path is rejected");
    assert_eq!(error.details["field"], "prompt");
    assert!(error.message.contains("undeclared absolute evidence path"));
}

#[test]
fn scans_explicit_absolute_paths_across_provider_prompt_syntaxes() {
    for prompt in [
        "Read file:///private/evidence.json",
        "evidence=/private/evidence.json",
        "[evidence](/private/evidence.json)",
        "See </private/evidence.json>.",
    ] {
        let error = validate_provider_evidence_inputs(&[], Some(prompt))
            .expect_err("undeclared host path is rejected");
        assert_eq!(error.details["field"], "prompt", "{prompt}");
        assert!(
            error.message.contains("undeclared absolute evidence path"),
            "{prompt}: {}",
            error.message
        );
    }

    validate_provider_evidence_inputs(&[], Some("Read https://example.test/evidence.json"))
        .expect("HTTPS URLs are not host evidence");
}

#[test]
fn prompt_path_classifier_admits_device_pseudo_files_but_flags_real_devices() {
    for (prompt, flagged) in [
        (
            "--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+hello\n",
            false,
        ),
        ("Run `cmd 2>/dev/null` quietly.", false),
        (
            "Pipe from /dev/stdin to /dev/stdout; read /dev/fd/3.",
            false,
        ),
        ("Inspect /dev/sda1 for evidence.", true),
    ] {
        let result = validate_provider_evidence_inputs(&[], Some(prompt));
        assert_eq!(result.is_err(), flagged, "{prompt}");
        if let Err(error) = result {
            assert!(
                error.message.contains("undeclared absolute evidence path"),
                "{prompt}: {}",
                error.message
            );
        }
    }
}

#[test]
fn ignores_prompt_prose_that_uses_slashes_without_naming_local_evidence() {
    let prompt = r#"
Use core/html with direct/staged and model/tool modes, then move [queued/running] work.
Route requests through /response, /startup, /sw.js, and /wp-codebox.
Use / as a separator and retain https://example.test/response plus `// NOTE: implementation`.
"#;

    assert_eq!(
        absolute_host_paths_in_provider_prompt(prompt).expect("scan prose"),
        Vec::<String>::new()
    );
    validate_provider_evidence_inputs(&[], Some(prompt))
        .expect("slash-delimited prose is not filesystem evidence");
}

#[test]
fn prompt_path_classifier_separates_host_paths_from_urls_references_and_concepts() {
    for prompt in [
        "See https://example.test/report#fragment.",
        "Follow /#fragment.",
        "Track /blocks-engine#1032 and issue #12991.",
        "Preserve public/plan/report compatibility.",
        "Use /page/report as a conceptual route.",
    ] {
        assert_eq!(
            absolute_host_paths_in_provider_prompt(prompt).expect("classify accepted syntax"),
            Vec::<String>::new(),
            "{prompt}"
        );
        validate_provider_evidence_inputs(&[], Some(prompt))
            .expect("URLs, references, and concepts are not host evidence");
    }

    let classified = classified_absolute_host_paths_in_provider_prompt(
        "Read /private/evidence.json and set input=/tmp/command-input.json.",
    )
    .expect("classify host paths");
    assert_eq!(
        classified
            .iter()
            .map(|path| (path.path.as_str(), path.classification))
            .collect::<Vec<_>>(),
        vec![
            ("/private/evidence.json", "unix-host-root"),
            ("/tmp/command-input.json", "explicit-path-assignment"),
        ]
    );

    let error = validate_provider_evidence_inputs(
        &[],
        Some("Read /private/evidence.json and set input=/tmp/command-input.json."),
    )
    .expect_err("Unix and quoted command paths require evidence");
    let evidence = error.details["id"]
        .as_str()
        .expect("classification evidence");
    assert!(evidence.contains("classification=unix-host-root token=/private/evidence.json"));
    assert!(evidence
        .contains("classification=explicit-path-assignment token=input=/tmp/command-input.json."));
}

#[test]
fn prompt_path_guard_allows_routes_and_urls_but_blocks_host_evidence() {
    let evidence_dir = tempfile::tempdir().expect("temporary evidence directory");
    let evidence = evidence_dir.path().join("evidence.json");
    std::fs::write(&evidence, b"{}").expect("existing evidence file");
    let existing_prompt = format!("Read {}", evidence.display());
    for (prompt, blocked) in [
        ("Use /media", false),
        ("Visit /blog/post-1", false),
        ("Visit /wp-json/wp/v2/posts", false),
        ("Visit https://x.com/media", false),
        ("Read /Users/x/file", true),
        ("Read /tmp/evidence.json", true),
        (existing_prompt.as_str(), true),
    ] {
        assert_eq!(
            validate_provider_evidence_inputs(&[], Some(prompt)).is_err(),
            blocked,
            "{prompt}"
        );
    }
}

#[test]
fn scans_a_recognized_single_segment_root_path() {
    assert_eq!(
        absolute_host_paths_in_provider_prompt("Read /tmp.").expect("scan real path"),
        vec!["/tmp".to_string()]
    );
}

#[test]
fn reports_all_concrete_undeclared_prompt_evidence_paths() {
    let error = validate_provider_evidence_inputs(
        &[],
        Some(
            "Read /private/evidence.json, file:///private/other.json, \
                 [more](/tmp/evidence.txt), and //srv/evidence.log.",
        ),
    )
    .expect_err("concrete local paths require declared evidence");

    assert_eq!(error.details["field"], "prompt");
    for path in [
        "/private/evidence.json",
        "/private/other.json",
        "/tmp/evidence.txt",
        "//srv/evidence.log",
    ] {
        assert!(
            error.message.contains(path),
            "missing {path}: {}",
            error.message
        );
    }
}

#[test]
fn accepts_only_exact_admitted_projected_evidence_paths() {
    // Projections live in Homeboy-owned state outside the workspace, under
    // a recognized host root, so near-miss spellings are classified as
    // host paths and must match an admitted projection exactly.
    let projected = "/home/homeboy/.local/share/homeboy/provider-evidence/files/abc/input";
    let admitted = std::collections::BTreeSet::from([projected.to_string()]);
    validate_provider_evidence_prompt(Some(&format!("Read {projected}")), &admitted)
        .expect("exact admitted projection is allowed");

    for path in [
        "/tmp/provider-evidence/files/abc/secret",
        "/home/homeboy/.local/share/homeboy/provider-evidence/files/abc/input.backup",
    ] {
        let error = validate_provider_evidence_prompt(Some(&format!("Read {path}")), &admitted)
            .expect_err("unadmitted projected-looking path is rejected");
        assert_eq!(error.details["field"], "prompt");
    }
}

#[test]
fn rewrites_declared_paths_across_provider_prompt_syntaxes() {
    let temp = tempfile::tempdir().expect("temporary workspace");
    let source = temp.path().join("source.json");
    let workspace = temp.path().join("workspace");
    std::fs::write(&source, "{}").expect("write source");
    std::fs::create_dir(&workspace).expect("create workspace");
    let input = AgentTaskProviderEvidenceInput {
        id: "source".to_string(),
        source: source.display().to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
    };
    let mut prompt = Some(format!(
        "file://{} key={} [source]({}) '{}'",
        source.display(),
        source.display(),
        source.display(),
        source.display(),
    ));
    let admitted =
        admit_provider_evidence_inputs(&[input.clone()]).expect("admit declared path spellings");
    crate::test_support::with_isolated_home(|_| {
        let projected = projected_provider_evidence(&[input.clone()], workspace.to_str())
            .expect("derive projected path");
        let projected_paths = projected_provider_evidence_paths(&projected);
        rewrite_provider_evidence_prompt(
            &mut prompt,
            &[input],
            &admitted,
            &projected,
            &projected_paths,
        )
        .expect("rewrite declared paths");
        let rewritten = prompt.expect("rewritten prompt");
        let destination = projected[0]["path"].as_str().expect("projected path");
        assert!(!rewritten.contains(&source.display().to_string()));
        assert_eq!(rewritten.matches(destination).count(), 4);
    });
}

#[test]
fn prompt_path_validation_distinguishes_separators_urls_and_real_paths() {
    let prompt = r#"
These functions route through `write_batch` / `mutate_batch` / `read_batch`.
Issue: https://github.com/Extra-Chill/homeboy/issues/7505
    struct AgentTaskBatchStore { root: PathBuf }   // NOTE: private
```
let comment = "// not evidence";
let path = "/also/not-evidence";
```
Read /private/one.json and //private/two.json.
Evidence=file:///private/three.json path=/private/four.json.
"#;
    let error = validate_provider_evidence_inputs(&[], Some(prompt))
        .expect_err("only real prose paths are rejected");

    assert_eq!(error.details["field"], "prompt");
    assert!(error.message.contains("/private/one.json"));
    assert!(error.message.contains("//private/two.json"));
    assert!(error.message.contains("/private/three.json"));
    assert!(error.message.contains("/private/four.json"));
    assert!(!error.message.contains("/also/not-evidence"));
    assert!(!error.message.contains("`/`"));
    assert!(!error.message.contains("// not evidence"));
}

#[test]
fn prompt_path_scanner_ignores_quoted_examples_and_scans_explicit_local_paths() {
    let paths = absolute_host_paths_in_provider_prompt(
            r#"{"input":"/json/path.md"} '< /quoted/path.txt >' < /private/angle/path.rs > key=/assigned/path.toml file://localhost/local/file.json file:///private/file-url.json //private/double/path.md / // https://example.com/ignore/me"#,
        )
        .expect("scan bounded prompt");

    assert_eq!(
        paths,
        vec![
            "//private/double/path.md",
            "/assigned/path.toml",
            "/local/file.json",
            "/private/angle/path.rs",
            "/private/file-url.json",
        ]
    );
}

#[test]
fn ignores_endpoint_repository_and_code_path_vocabulary() {
    let prompt = r#"
Route requests through /response, /startup, /sw.js, and /wp-codebox.
Use core/html, direct/staged, and model/tool for Extra-Chill/homeboy.
`/private/inline-example.json`, "/tmp/quoted-example.json", and "file:///tmp/quoted.json" are examples.
    let path = "/private/indented-code.json";
```
let path = "/private/fenced-code.json";
```
"#;

    assert_eq!(
        absolute_host_paths_in_provider_prompt(prompt).expect("scan technical prose"),
        Vec::<String>::new()
    );
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_evidence_source() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().expect("temporary workspace");
    let source = temp.path().join("source.json");
    let link = temp.path().join("source-link.json");
    std::fs::write(&source, "{}").expect("write source");
    symlink(&source, &link).expect("create source symlink");
    let error = project_provider_evidence_inputs(
        &[AgentTaskProviderEvidenceInput {
            id: "source".to_string(),
            source: link.display().to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
        }],
        temp.path(),
        None,
    )
    .expect_err("symlink source rejected");
    assert!(error.message.contains("regular files"));
}

#[cfg(target_os = "macos")]
#[test]
fn admits_var_temporary_evidence_as_its_private_var_canonical_identity() {
    let var_temp = std::env::temp_dir();
    if !var_temp.starts_with("/var/") {
        return;
    }
    let temp = tempfile::Builder::new()
        .prefix("homeboy-provider-evidence-")
        .tempdir_in(&var_temp)
        .expect("temporary workspace under var");
    let source = temp.path().join("source.json");
    let workspace = temp.path().join("workspace");
    std::fs::write(&source, "{}").expect("write source");
    std::fs::create_dir(&workspace).expect("create workspace");
    let canonical_source = source.canonicalize().expect("canonical source");
    let supplied_source = Path::new("/var").join(
        canonical_source
            .strip_prefix("/private/var")
            .expect("temporary file is under private var"),
    );
    assert!(supplied_source.exists(), "var alias resolves to the source");
    let input = AgentTaskProviderEvidenceInput {
        id: "source".to_string(),
        source: supplied_source.display().to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
    };

    validate_provider_evidence_inputs(
        &[input.clone()],
        Some(&format!("Read {}", canonical_source.display())),
    )
    .expect("private var spelling is declared by its var alias");
    let projected = project_provider_evidence_inputs(&[input.clone()], &workspace, None)
        .expect("project var alias");
    assert!(projected[0].get("supplied_path").is_none());
    assert!(projected[0].get("canonical_path").is_none());
    assert!(projected[0].get("approved_root").is_none());
    let admitted = admit_provider_evidence_inputs(&[input.clone()]).expect("admit evidence");
    let provenance =
        provider_evidence_controller_provenance_from_admitted(&[input.clone()], &admitted);
    assert_eq!(provenance[0]["supplied_path"], "[redacted]");
    assert_eq!(provenance[0]["canonical_path"], "[redacted]");
    assert_eq!(provenance[0]["approved_root"], "[redacted]");
    assert_ne!(
        provenance[0]["canonical_path_sha256"],
        canonical_source.display().to_string()
    );

    let mut prompt = Some(format!("Read {}", canonical_source.display()));
    let admitted = admit_provider_evidence_inputs(&[input.clone()]).expect("admit evidence");
    let projected_paths = projected_provider_evidence_paths(&projected);
    rewrite_provider_evidence_prompt(
        &mut prompt,
        &[input],
        &admitted,
        &projected,
        &projected_paths,
    )
    .expect("rewrite evidence prompt");
    assert_eq!(
        prompt,
        Some(format!(
            "Read {}",
            projected[0]["path"].as_str().expect("projected path")
        ))
    );
}

#[cfg(target_os = "macos")]
#[test]
fn rewrites_var_prompt_alias_for_private_var_declaration() {
    let var_temp = std::env::temp_dir();
    if !var_temp.starts_with("/var/") {
        return;
    }
    let temp = tempfile::Builder::new()
        .prefix("homeboy-provider-evidence-")
        .tempdir_in(&var_temp)
        .expect("temporary workspace under var");
    let source = temp.path().join("source.json");
    let workspace = temp.path().join("workspace");
    std::fs::write(&source, "{}").expect("write source");
    std::fs::create_dir(&workspace).expect("create workspace");
    let canonical_source = source.canonicalize().expect("canonical source");
    let var_source = Path::new("/var").join(
        canonical_source
            .strip_prefix("/private/var")
            .expect("temporary file is under private var"),
    );
    let input = AgentTaskProviderEvidenceInput {
        id: "source".to_string(),
        source: canonical_source.display().to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
    };
    let prompt_source = var_source.display().to_string();

    validate_provider_evidence_inputs(&[input.clone()], Some(&format!("Read {prompt_source}")))
        .expect("var spelling is declared by its private var alias");
    let projected = project_provider_evidence_inputs(&[input.clone()], &workspace, None)
        .expect("project private var source");
    let config = serde_json::json!({ "evidence_inputs": projected });
    let encoded_config = config.to_string();
    assert!(!encoded_config.contains(&canonical_source.display().to_string()));
    assert!(!encoded_config.contains(&prompt_source));

    let mut prompt = Some(format!("Read {prompt_source}"));
    let admitted = admit_provider_evidence_inputs(&[input.clone()]).expect("admit evidence");
    let projected_paths = projected_provider_evidence_paths(&projected);
    rewrite_provider_evidence_prompt(
        &mut prompt,
        &[input],
        &admitted,
        &projected,
        &projected_paths,
    )
    .expect("rewrite evidence prompt");
    let rewritten = prompt.expect("rewritten prompt");
    assert_eq!(
        rewritten,
        format!(
            "Read {}",
            config["evidence_inputs"][0]["path"]
                .as_str()
                .expect("projected path")
        )
    );
    assert!(!rewritten.contains(&canonical_source.display().to_string()));
    assert!(!rewritten.contains(&prompt_source));
}

#[test]
fn accepts_only_the_exact_macos_system_alias_boundaries() {
    for (alias, root) in [
        ("/tmp", "/private/tmp"),
        ("/var", "/private/var"),
        ("/etc", "/private/etc"),
    ] {
        let canonical = Path::new(root).join("x");
        let alias = Path::new(alias).join("x");
        assert_eq!(
            macos_system_alias(&canonical),
            Some((Path::new(root), alias.clone()))
        );
        for supplied in [&alias, &canonical] {
            assert_eq!(
                approved_macos_temporary_root(supplied, &canonical),
                Some(PathBuf::from(root))
            );
        }
        let spellings = approved_provider_evidence_spellings(&canonical);
        assert!(spellings.contains(&canonical));
        assert_eq!(spellings.contains(&alias), cfg!(target_os = "macos"));
    }
    for (supplied, canonical) in [
        ("/private/tmpfoo/x", "/private/tmpfoo/x"),
        ("/tmpx/y", "/private/tmp/y"),
        ("/Users/a/link", "/private/tmp/x"),
    ] {
        assert!(approved_macos_temporary_root(Path::new(supplied), Path::new(canonical)).is_none());
    }
    assert!(macos_system_alias(Path::new("/private/tmpfoo/x")).is_none());
    assert!(macos_system_alias(Path::new("/tmpx/y")).is_none());
    assert!(approved_macos_temporary_root(
        Path::new("/var/folders/example/evidence.json"),
        Path::new("/private/var/elsewhere/evidence.json")
    )
    .is_none());
    assert!(approved_macos_temporary_root(
        Path::new("/var/../private/var/folders/example/evidence.json"),
        Path::new("/private/var/folders/example/evidence.json")
    )
    .is_none());
    assert!(approved_macos_temporary_root(
        Path::new("/private/var/folders/example/./evidence.json"),
        Path::new("/private/var/folders/example/evidence.json")
    )
    .is_none());
}

#[cfg(target_os = "macos")]
#[test]
fn projects_tmp_alias_directory_and_rewrites_both_prompt_spellings() {
    let temp = tempfile::Builder::new()
        .prefix("homeboy-provider-evidence-")
        .tempdir_in("/tmp")
        .expect("temporary workspace under tmp");
    let source = temp.path().join("source");
    std::fs::create_dir(&source).expect("create source directory");
    std::fs::write(source.join("evidence.json"), "{}").expect("write evidence");
    let canonical = source.canonicalize().expect("canonical source");
    let input = AgentTaskProviderEvidenceInput {
        id: "source".to_string(),
        source: source.display().to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
    };
    let admitted = admit_provider_evidence_inputs(&[input.clone()]).expect("admit tmp source");
    assert_eq!(admitted[0].copy_path(), canonical);
    let store = temp
        .path()
        .canonicalize()
        .expect("canonical workspace")
        .join("store");
    let projected =
        project_admitted_provider_evidence_inputs_at(&[input.clone()], &admitted, &store)
            .expect("copy tmp source through canonical system alias");
    let destination = Path::new(projected[0]["path"].as_str().expect("projected path"));
    assert_eq!(
        std::fs::read_to_string(destination.join("evidence.json")).expect("copied evidence"),
        "{}"
    );
    let projected_paths = projected_provider_evidence_paths(&projected);
    for spelling in [&source, &canonical] {
        let mut prompt = Some(format!("Read {}", spelling.display()));
        rewrite_provider_evidence_prompt(
            &mut prompt,
            &[input.clone()],
            &admitted,
            &projected,
            &projected_paths,
        )
        .expect("both system alias spellings are approved prompt references");
        assert!(!prompt.unwrap().contains(&spelling.display().to_string()));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn rejects_dot_segments_and_arbitrary_symlink_prompt_aliases() {
    use std::os::unix::fs::symlink;

    let var_temp = std::env::temp_dir();
    if !var_temp.starts_with("/var/") {
        return;
    }
    let temp = tempfile::Builder::new()
        .prefix("homeboy-provider-evidence-")
        .tempdir_in(&var_temp)
        .expect("temporary workspace under var");
    let source = temp.path().join("source.json");
    std::fs::write(&source, "{}").expect("write source");
    let canonical_source = source.canonicalize().expect("canonical source");
    let supplied_source = Path::new("/var").join(
        canonical_source
            .strip_prefix("/private/var")
            .expect("temporary file is under private var"),
    );
    let input = AgentTaskProviderEvidenceInput {
        id: "source".to_string(),
        source: supplied_source.display().to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
    };
    let dotted = canonical_source
        .parent()
        .expect("source parent")
        .join(".")
        .join(canonical_source.file_name().expect("source name"));
    let error = validate_provider_evidence_inputs(
        &[input.clone()],
        Some(&format!("Read {}", dotted.display())),
    )
    .expect_err("dot segment alias is not an approved spelling");
    assert_eq!(error.details["field"], "prompt");

    let arbitrary_alias = temp.path().join("arbitrary-alias.json");
    symlink(&canonical_source, &arbitrary_alias).expect("create arbitrary alias");
    let error = validate_provider_evidence_inputs(
        &[input],
        Some(&format!("Read {}", arbitrary_alias.display())),
    )
    .expect_err("arbitrary symlink alias is not an approved spelling");
    assert_eq!(error.details["field"], "prompt");
}

#[test]
fn provider_executor_config_excludes_host_source_paths() {
    let temp = tempfile::tempdir().expect("temporary workspace");
    let source = temp.path().join("external.json");
    let workspace = temp.path().join("workspace");
    std::fs::write(&source, "{}").expect("write source");
    std::fs::create_dir(&workspace).expect("create workspace");
    let input = AgentTaskProviderEvidenceInput {
        id: "source".to_string(),
        source: source.display().to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
    };

    let projected =
        project_provider_evidence_inputs(&[input], &workspace, None).expect("project evidence");
    let config = serde_json::json!({ "evidence_inputs": projected });
    let encoded = config.to_string();
    assert!(!encoded.contains(&source.display().to_string()));
    assert!(config["evidence_inputs"][0].get("supplied_path").is_none());
    assert!(config["evidence_inputs"][0].get("canonical_path").is_none());
    assert!(config["evidence_inputs"][0].get("approved_root").is_none());
}

#[cfg(unix)]
#[test]
fn rejects_evidence_behind_an_intermediate_symlink() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary workspace");
    let outside = temp.path().join("outside");
    let linked_parent = temp.path().join("linked-parent");
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&outside).expect("create outside");
    std::fs::create_dir(&workspace).expect("create workspace");
    std::fs::write(outside.join("source.json"), "{}").expect("write source");
    symlink(&outside, &linked_parent).expect("link parent");

    let error = project_provider_evidence_inputs(
        &[AgentTaskProviderEvidenceInput {
            id: "source".to_string(),
            source: linked_parent.join("source.json").display().to_string(),
            include: Vec::new(),
            exclude: Vec::new(),
        }],
        &workspace,
        None,
    )
    .expect_err("intermediate symlink is rejected during secure copy");
    assert!(error.message.contains("symlink or non-directory"));
}

#[cfg(unix)]
#[test]
fn rejects_a_source_replaced_with_a_symlink_after_admission() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("temporary workspace");
    let source = temp.path().join("source.json");
    let outside = temp.path().join("outside.json");
    let destination = temp.path().join("workspace/evidence.json");
    std::fs::write(&source, "accepted").expect("write source");
    std::fs::write(&outside, "outside").expect("write outside");
    let admitted = admit_provider_evidence_source(&source.display().to_string())
        .expect("admit regular source");
    std::fs::remove_file(&source).expect("remove admitted source");
    symlink(&outside, &source).expect("replace source with symlink");

    let error = secure_provider_evidence_copy(&admitted, &destination)
        .expect_err("secure reopen rejects the replacement symlink");
    assert!(error.message.contains("could not be securely opened"));
    assert!(!destination.exists());
}

#[cfg(unix)]
#[test]
fn rejects_a_source_replaced_with_a_fifo_without_blocking() {
    use std::os::unix::ffi::OsStrExt;

    let temp = tempfile::tempdir().expect("temporary workspace");
    let source = temp.path().join("source.json");
    let destination = temp.path().join("workspace/evidence.json");
    std::fs::write(&source, "accepted").expect("write source");
    let admitted = admit_provider_evidence_source(&source.display().to_string())
        .expect("admit regular source");
    std::fs::remove_file(&source).expect("remove admitted source");
    let source_name =
        std::ffi::CString::new(source.as_os_str().as_bytes()).expect("FIFO source name");
    assert_eq!(unsafe { libc::mkfifo(source_name.as_ptr(), 0o600) }, 0);

    let error = secure_provider_evidence_copy(&admitted, &destination)
        .expect_err("replacement FIFO is rejected without a writer");
    assert!(error.message.contains("identity changed after validation"));
    assert!(!destination.exists());
}

#[test]
fn aborts_projection_when_source_is_removed_after_validation() {
    let temp = tempfile::tempdir().expect("temporary workspace");
    let source = temp.path().join("source.json");
    let workspace = temp.path().join("workspace");
    std::fs::write(&source, "accepted").expect("write source");
    std::fs::create_dir(&workspace).expect("create workspace");
    let input = AgentTaskProviderEvidenceInput {
        id: "source".to_string(),
        source: source.display().to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
    };
    let admitted =
        admit_provider_evidence_inputs(&[input.clone()]).expect("validate and admit source");
    std::fs::remove_file(&source).expect("remove admitted source");

    let error = project_admitted_provider_evidence_inputs_at(
        &[input],
        &admitted,
        &workspace.join("provider-evidence"),
    )
    .expect_err("removed source aborts projection");
    assert!(error.message.contains("identity changed after validation"));
    assert!(!workspace.join(".homeboy").exists());
}

#[test]
fn aborts_rewrite_when_source_is_replaced_after_validation() {
    let temp = tempfile::tempdir().expect("temporary workspace");
    let source = temp.path().join("source.json");
    let replacement = temp.path().join("replacement.json");
    let workspace = temp.path().join("workspace");
    std::fs::write(&source, "accepted").expect("write source");
    std::fs::write(&replacement, "replaced").expect("write replacement");
    std::fs::create_dir(&workspace).expect("create workspace");
    let input = AgentTaskProviderEvidenceInput {
        id: "source".to_string(),
        source: source.display().to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
    };
    let mut prompt = Some(format!("Read {}", source.display()));
    let admitted =
        admit_provider_evidence_inputs(&[input.clone()]).expect("validate and admit source");
    std::fs::rename(&replacement, &source).expect("replace admitted source");

    crate::test_support::with_isolated_home(|_| {
        let projected = projected_provider_evidence(&[input.clone()], workspace.to_str())
            .expect("derive projected path");
        let projected_paths = projected_provider_evidence_paths(&projected);
        let error = rewrite_provider_evidence_prompt(
            &mut prompt,
            &[input],
            &admitted,
            &projected,
            &projected_paths,
        )
        .expect_err("replaced source aborts rewrite");
        assert!(error.message.contains("identity changed after validation"));
        assert_eq!(prompt, Some(format!("Read {}", source.display())));
    });
}

fn capture_directory() -> (tempfile::TempDir, AgentTaskProviderEvidenceInput) {
    let temp = tempfile::tempdir().expect("capture");
    std::fs::create_dir(temp.path().join("website")).expect("website");
    std::fs::write(temp.path().join("website/index.html"), "<html>home</html>").expect("html");
    std::fs::create_dir(temp.path().join("receipts")).expect("receipts");
    std::fs::write(temp.path().join("receipts/note.txt"), "receipt").expect("receipt");
    std::fs::create_dir(temp.path().join("screenshots")).expect("screenshots");
    std::fs::write(
        temp.path().join("screenshots/home.png"),
        vec![
            9u8;
            (super::super::provider_evidence::DEFAULT_EVIDENCE_MEDIA_FILE_CAP_BYTES as usize) + 1
        ],
    )
    .expect("screenshot");
    let input = AgentTaskProviderEvidenceInput {
        id: "capture".to_string(),
        source: temp
            .path()
            .canonicalize()
            .expect("canonical capture")
            .display()
            .to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
    };
    (temp, input)
}

#[test]
fn preview_and_execution_project_the_same_directory_tree() {
    let (_temp, input) = capture_directory();
    let store = tempfile::tempdir().expect("store");
    let admitted = admit_provider_evidence_inputs(&[input.clone()]).expect("admit directory");
    let preview =
        projected_provider_evidence_from_admitted(&[input.clone()], &admitted, store.path())
            .expect("preview directory");
    assert!(
        !Path::new(preview[0]["path"].as_str().expect("preview path")).exists(),
        "preview must not copy the capture"
    );
    let executed =
        project_admitted_provider_evidence_inputs_at(&[input.clone()], &admitted, store.path())
            .expect("execute directory projection");
    for field in ["path", "sha256", "size_bytes", "transport", "entries"] {
        assert_eq!(preview[0][field], executed[0][field], "{field}");
    }
    let root = PathBuf::from(executed[0]["path"].as_str().expect("path"));
    assert!(root.join("website/index.html").is_file());
    assert_eq!(
        std::fs::read_to_string(root.join("website/index.html")).expect("html"),
        "<html>home</html>"
    );
    assert!(root.join("receipts/note.txt").is_file());
    assert!(
        std::fs::metadata(&root)
            .expect("projected root metadata")
            .permissions()
            .readonly(),
        "projected directory is immutable"
    );
    assert!(
        std::fs::metadata(root.join("website/index.html"))
            .expect("projected file metadata")
            .permissions()
            .readonly(),
        "projected files are immutable"
    );
    assert!(
        !root.join("screenshots/home.png").exists(),
        "huge screenshots stay out of the projection"
    );
    assert_eq!(
        executed[0]["transport"],
        homeboy_engine_primitives::content_hash::PROVIDER_EVIDENCE_DIRECTORY_TRANSPORT
    );
    assert!(executed[0]["omitted"]["count"].as_u64().unwrap_or(0) >= 1);
    let mut prompt = Some(format!("Read {} before editing.", input.source));
    let projected_paths = projected_provider_evidence_paths(&executed);
    rewrite_provider_evidence_prompt(
        &mut prompt,
        &[input.clone()],
        &admitted,
        &executed,
        &projected_paths,
    )
    .expect("rewrite directory prompt");
    let rewritten = prompt.expect("rewritten");
    assert!(rewritten.contains(root.to_str().expect("utf-8 root")));
    assert!(!rewritten.contains(&input.source));
    let retry =
        project_admitted_provider_evidence_inputs_at(&[input.clone()], &admitted, store.path())
            .expect("retry reuses the directory projection");
    assert_eq!(retry[0]["path"], executed[0]["path"]);
    assert_eq!(retry[0]["sha256"], executed[0]["sha256"]);
    let mut permissions = std::fs::metadata(&root)
        .expect("root metadata")
        .permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(&root, permissions).expect("simulate mutable store");
    std::fs::write(root.join("unexpected.txt"), "injected")
        .expect("simulate unexpected content-addressed store mutation");
    let error = project_admitted_provider_evidence_inputs_at(&[input], &admitted, store.path())
        .expect_err("content-addressed tree with extra files is rejected");
    assert!(error.message.contains("storage is corrupt"));
    for directory in [root.clone(), root.join("website"), root.join("receipts")] {
        let mut permissions = std::fs::metadata(&directory)
            .expect("directory metadata")
            .permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(directory, permissions).expect("restore cleanup permissions");
    }
}

#[cfg(unix)]
#[test]
fn directory_projection_rejects_a_selected_symlink_without_copying_outside() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().expect("capture");
    let outside = temp.path().join("secret.txt");
    std::fs::write(&outside, "secret").expect("outside");
    let website = temp.path().join("website");
    std::fs::create_dir(&website).expect("website");
    symlink(&outside, website.join("index.html")).expect("symlink evidence");
    let input = AgentTaskProviderEvidenceInput {
        id: "capture".to_string(),
        source: temp
            .path()
            .canonicalize()
            .expect("canonical")
            .display()
            .to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
    };
    let admitted = admit_provider_evidence_inputs(&[input.clone()]).expect("admit");
    let error = project_admitted_provider_evidence_inputs_at(
        &[input],
        &admitted,
        &temp.path().join("store"),
    )
    .expect_err("symlink blocks projection");
    assert!(error.message.contains("cannot follow symlinks"));
    assert_eq!(
        std::fs::read_to_string(&outside).expect("untouched"),
        "secret"
    );
    assert!(!temp.path().join("store/trees").exists());
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_destination_ancestor_without_writing_through_it() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().expect("temporary workspace");
    let source = temp.path().join("source.json");
    let outside = temp.path().join("outside");
    let workspace = temp.path().join("workspace");
    std::fs::write(&source, "{}").expect("write source");
    std::fs::create_dir_all(&outside).expect("create outside");
    std::fs::create_dir_all(&workspace).expect("create workspace");
    let store = temp.path().join("provider-evidence");
    symlink(&outside, &store).expect("create destination symlink");
    let input = AgentTaskProviderEvidenceInput {
        id: "source".to_string(),
        source: source
            .canonicalize()
            .expect("canonical source")
            .display()
            .to_string(),
        include: Vec::new(),
        exclude: Vec::new(),
    };
    let admitted = admit_provider_evidence_inputs(&[input.clone()]).expect("admit source");
    let error = project_admitted_provider_evidence_inputs_at(&[input], &admitted, &store)
        .expect_err("symlink destination rejected");
    assert!(error.message.contains("symlink or non-directory"));
    assert!(std::fs::read_dir(&outside)
        .expect("outside directory")
        .next()
        .is_none());
}
