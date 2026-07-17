use super::*;

#[test]
fn high_item_count_detected() {
    let dir = std::env::temp_dir().join("homeboy_structural_items_test");
    let _ = std::fs::create_dir_all(&dir);

    let mut content = String::new();
    for i in 0..35 {
        content.push_str(&format!("fn func_{}() {{}}\n", i));
    }
    std::fs::write(dir.join("many.rs"), &content).unwrap();

    let findings = analyze_structure(&dir);
    let item_findings: Vec<_> = findings
        .iter()
        .filter(|f| f.kind == AuditFinding::HighItemCount)
        .collect();

    assert_eq!(item_findings.len(), 1);
    assert!(item_findings[0].description.contains("35 top-level items"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn directory_sprawl_detected() {
    let dir = std::env::temp_dir().join("homeboy_structural_sprawl_test");
    let root = dir.join("src/core");
    let _ = std::fs::create_dir_all(&root);

    for i in 0..60 {
        std::fs::write(root.join(format!("mod_{}.rs", i)), "pub fn run() {}\n").unwrap();
    }

    let findings = analyze_structure(&dir);
    let sprawl: Vec<_> = findings
        .iter()
        .filter(|f| f.kind == AuditFinding::DirectorySprawl)
        .collect();

    assert_eq!(sprawl.len(), 1);
    assert_eq!(sprawl[0].file, "src/core");
    assert!(sprawl[0].description.contains("60 source files"));

    let _ = std::fs::remove_dir_all(&dir);
}
