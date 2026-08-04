use std::path::PathBuf;

fn main() {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    homeboy_cli::cli_surface::reference_docs::write_cli_reference(&workspace_root)
        .expect("generate CLI reference");
}
