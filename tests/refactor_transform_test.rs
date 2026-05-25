use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use homeboy::core::refactor;

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
    let result = refactor::apply_transforms(&root, "ad-hoc", &set, false, None).unwrap();
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
    let result = refactor::apply_transforms_with_options(
        &root,
        "ad-hoc",
        &set,
        false,
        None,
        refactor::TransformOptions {
            match_detail_limit: None,
        },
    )
    .unwrap();
    let rule = &result.rules[0];

    assert_eq!(rule.replacement_count, 3);
    assert_eq!(rule.matches.len(), 3);
    assert!(!rule.matches_truncated);
    assert_eq!(rule.omitted_match_count, 0);
    assert_eq!(rule.match_detail_limit, None);

    let _ = fs::remove_dir_all(root);
}
