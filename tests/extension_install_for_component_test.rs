use homeboy::component;
use homeboy::extension;
use std::fs;
use std::path::Path;
use std::sync::Mutex;

static HOME_LOCK: Mutex<()> = Mutex::new(());

fn with_isolated_home<T>(f: impl FnOnce(&Path) -> T) -> T {
    let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let old_home = std::env::var_os("HOME");
    let home = tempfile::tempdir().expect("home tempdir");

    std::env::set_var("HOME", home.path());
    let result = f(home.path());

    if let Some(value) = old_home {
        std::env::set_var("HOME", value);
    } else {
        std::env::remove_var("HOME");
    }

    result
}

fn write_extension_fixture(root: &Path, id: &str) {
    let dir = root.join(id);
    fs::create_dir_all(&dir).expect("extension dir");
    fs::write(
        dir.join(format!("{}.json", id)),
        format!(
            r#"{{
  "name": "{} extension",
  "version": "1.0.0"
}}"#,
            id
        ),
    )
    .expect("extension manifest");
}

fn write_component_fixture(root: &Path, extensions: &[&str]) {
    let extension_json = extensions
        .iter()
        .map(|id| format!(r#"    "{}": {{}}"#, id))
        .collect::<Vec<_>>()
        .join(",\n");

    fs::write(
        root.join("homeboy.json"),
        format!(
            r#"{{
  "id": "multi-extension-component",
  "extensions": {{
{}
  }}
}}"#,
            extension_json
        ),
    )
    .expect("component config");
}

#[test]
fn install_for_component_installs_multiple_extensions() {
    with_isolated_home(|home| {
        let source = home.join("source");
        write_extension_fixture(&source, "alpha");
        write_extension_fixture(&source, "beta");

        let component_dir = home.join("component");
        fs::create_dir_all(&component_dir).expect("component dir");
        write_component_fixture(&component_dir, &["alpha", "beta"]);
        let component = component::discover_from_portable(&component_dir).expect("component");

        let result = extension::install_for_component(&component, &source.to_string_lossy())
            .expect("install should succeed");

        let installed_ids = result
            .installed
            .iter()
            .map(|entry| entry.extension_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(installed_ids, vec!["alpha", "beta"]);
        assert!(result.skipped.is_empty());
        assert!(home
            .join(".config/homeboy/extensions/alpha/alpha.json")
            .exists());
        assert!(home
            .join(".config/homeboy/extensions/beta/beta.json")
            .exists());
    });
}

#[test]
fn install_for_component_skips_already_installed_extensions() {
    with_isolated_home(|home| {
        let source = home.join("source");
        write_extension_fixture(&source, "alpha");
        write_extension_fixture(&source, "beta");

        let component_dir = home.join("component");
        fs::create_dir_all(&component_dir).expect("component dir");
        write_component_fixture(&component_dir, &["alpha", "beta"]);
        let component = component::discover_from_portable(&component_dir).expect("component");

        extension::install(&source.join("alpha").to_string_lossy(), Some("alpha"))
            .expect("pre-install alpha");

        let result = extension::install_for_component(&component, &source.to_string_lossy())
            .expect("install should succeed");

        let installed_ids = result
            .installed
            .iter()
            .map(|entry| entry.extension_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(installed_ids, vec!["beta"]);
        assert_eq!(result.skipped, vec!["alpha"]);
    });
}

#[test]
fn install_for_component_uses_path_based_portable_component_config() {
    with_isolated_home(|home| {
        let source = home.join("source");
        write_extension_fixture(&source, "alpha");
        write_extension_fixture(&source, "beta");

        let component_dir = home.join("component");
        fs::create_dir_all(&component_dir).expect("component dir");
        write_component_fixture(&component_dir, &["alpha", "beta"]);

        let component = component::discover_from_portable(&component_dir)
            .expect("component should resolve from portable path");
        let result = extension::install_for_component(&component, &source.to_string_lossy())
            .expect("install should succeed");

        assert_eq!(result.component_id, "multi-extension-component");
        assert_eq!(result.installed.len(), 2);
    });
}
