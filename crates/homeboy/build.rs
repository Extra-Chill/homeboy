use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR missing"));
    let docs_root = manifest_dir.join("../..").join("docs");

    if !docs_root.exists() {
        panic!("Docs directory not found: {}", docs_root.display());
    }

    let mut doc_paths = Vec::new();
    collect_md_files(&docs_root, &mut doc_paths);
    doc_paths.sort();

    for path in &doc_paths {
        println!("cargo:rerun-if-changed={}", path.display());
    }

    let generated = generate_docs_rs(&docs_root, &doc_paths);

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR missing"));
    fs::write(out_dir.join("generated_docs.rs"), generated)
        .expect("Failed to write generated_docs.rs");
}

fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("Failed to read dir {}: {}", dir.display(), err));

    for entry in entries {
        let entry = entry.unwrap_or_else(|err| panic!("Failed to read dir entry: {}", err));
        let path = entry.path();

        if path.is_dir() {
            collect_md_files(&path, out);
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            out.push(path);
        }
    }
}

fn generate_docs_rs(docs_root: &Path, doc_paths: &[PathBuf]) -> String {
    let mut out = String::new();
    out.push_str("pub static GENERATED_DOCS: &[(&'static str, &'static str)] = &[\n");

    for path in doc_paths {
        let key = key_for_path(docs_root, path);
        let content = fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("Failed to read doc {}: {}", path.display(), err));

        out.push_str("    (\"");
        out.push_str(&escape_rust_string(&key));
        out.push_str("\", r#\"");
        out.push_str(&content);
        out.push_str("\"#),\n");
    }

    out.push_str("];\n");
    out
}

fn key_for_path(docs_root: &Path, path: &Path) -> String {
    let relative = path
        .strip_prefix(docs_root)
        .unwrap_or_else(|_| panic!("Doc path is not under docs dir: {}", path.display()));

    let mut key = relative.to_string_lossy().replace('\\', "/");

    if let Some(without_ext) = key.strip_suffix(".md") {
        key = without_ext.to_string();
    }

    if key == "index" {
        return "index".to_string();
    }

    key
}

fn escape_rust_string(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}
