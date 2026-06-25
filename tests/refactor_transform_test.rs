use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use homeboy::core::refactor;

const NO_MATCH_FIXTURE: &str = include_str!("fixtures/refactor_transform_no_match.json");

fn tmp_dir(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("homeboy-refactor-{name}-{nanos}"))
}

#[test]
fn transform_output_samples_match_details_at_scale() {
    let root = tmp_dir("transform-scale");
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();

    for file_index in 0..200 {
        let mut content = String::new();
        for line_index in 0..100 {
            content.push_str(&format!("OLD marker {file_index} {line_index}\n"));
        }
        fs::write(src.join(format!("file{file_index}.txt")), content).unwrap();
    }

    let set = refactor::ad_hoc_transform("OLD", "NEW", "src/**/*.txt", "line");
    let result = refactor::apply_transforms(
        &root,
        "ad-hoc",
        &set,
        false,
        None,
        Some(refactor::DEFAULT_MATCH_DETAIL_LIMIT),
    )
    .unwrap();
    let rule = &result.rules[0];

    assert_eq!(result.total_replacements, 20_000);
    assert_eq!(rule.replacement_count, 20_000);
    assert_eq!(rule.matches.len(), refactor::DEFAULT_MATCH_DETAIL_LIMIT);
    assert!(rule.matches_truncated);
    assert_eq!(
        rule.omitted_match_count,
        20_000 - refactor::DEFAULT_MATCH_DETAIL_LIMIT
    );
    assert_eq!(
        rule.match_detail_limit,
        Some(refactor::DEFAULT_MATCH_DETAIL_LIMIT)
    );

    let json = serde_json::to_string(&result).unwrap();
    assert!(json.len() < 60_000, "json output was {} bytes", json.len());

    let _ = fs::remove_dir_all(root);
}

#[test]
fn transform_output_can_include_full_match_details() {
    let root = tmp_dir("transform-full-details");
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("file.txt"), "OLD one\nOLD two\nOLD three\n").unwrap();

    let set = refactor::ad_hoc_transform("OLD", "NEW", "src/**/*.txt", "line");
    let result = refactor::apply_transforms(&root, "ad-hoc", &set, false, None, None).unwrap();
    let rule = &result.rules[0];

    assert_eq!(rule.replacement_count, 3);
    assert_eq!(rule.matches.len(), 3);
    assert!(!rule.matches_truncated);
    assert_eq!(rule.omitted_match_count, 0);
    assert_eq!(rule.match_detail_limit, None);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn transform_no_match_status_fixture_is_successful_empty_result() {
    let fixture: serde_json::Value = serde_json::from_str(NO_MATCH_FIXTURE).unwrap();

    assert_eq!(fixture["success"], true);
    assert_eq!(fixture["data"]["command"], "refactor.transform");
    assert_eq!(fixture["data"]["total_replacements"], 0);
    assert_eq!(fixture["data"]["total_files"], 0);
    assert_eq!(fixture["data"]["written"], false);
}

#[test]
fn rename_defaults_to_cwd_git_worktree_without_component_metadata() {
    let root = tmp_dir("rename-cwd");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn old_name() -> i32 { 1 }\npub fn call() -> i32 { old_name() }\n",
    )
    .unwrap();

    let git_init = Command::new("git")
        .arg("init")
        .current_dir(&root)
        .output()
        .expect("git init");
    assert!(
        git_init.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&git_init.stderr)
    );

    let output = homeboy_command()
        .args([
            "refactor", "rename", "--from", "old_name", "--to", "new_name", "--write",
        ])
        .current_dir(&root)
        .env("HOME", &root)
        .output()
        .expect("run homeboy refactor rename");

    assert!(
        output.status.success(),
        "refactor rename failed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let source = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    assert!(source.contains("new_name"));
    assert!(!source.contains("old_name"));

    let _ = fs::remove_dir_all(root);
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}

fn homeboy_command() -> Command {
    let mut command = Command::new(homeboy_bin());
    command.env("HOMEBOY_NO_UPDATE_CHECK", "1");
    command
}
