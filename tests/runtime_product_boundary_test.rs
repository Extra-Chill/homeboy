use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn core_files_do_not_name_extension_owned_runtime_products() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let needles = [
        format!("{}box", "code"),
        format!("wp-{}box", "code"),
        format!("{}machine", "data"),
        format!("{}-machine", "data"),
        format!("{} Machine", "Data"),
        format!("{}Machine", "Data"),
    ];
    let mut violations = Vec::new();

    for root in ["src", "tests", "docs"] {
        collect_violations(&repo.join(root), &repo, &needles, &mut violations);
    }

    assert!(
        violations.is_empty(),
        "Homeboy core must keep extension-owned runtime product knowledge in extensions/adapters:\n{}",
        violations.join("\n")
    );
}

fn collect_violations(path: &Path, repo: &Path, needles: &[String], violations: &mut Vec<String>) {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_violations(&path, repo, needles, violations);
            continue;
        }
        if should_skip(&path, repo) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let lower = content.to_lowercase();
        if needles
            .iter()
            .any(|needle| lower.contains(&needle.to_lowercase()))
        {
            violations.push(
                path.strip_prefix(repo)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            );
        }
    }
}

fn should_skip(path: &Path, repo: &Path) -> bool {
    let relative = path.strip_prefix(repo).unwrap_or(path);
    relative == Path::new("docs/changelog.md")
        || relative == Path::new("tests/runtime_product_boundary_test.rs")
}
