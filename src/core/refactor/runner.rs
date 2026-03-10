use crate::component::Component;
use crate::error::Error;
use crate::extension;

pub fn resolve_lint_script(component: &Component) -> crate::Result<String> {
    let extensions = component.extensions.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "component",
            format!("Component '{}' has no extensions configured", component.id),
            None,
            None,
        )
        .with_hint(format!(
            "Add a extension: homeboy component set {} --extension <extension_id>",
            component.id
        ))
    })?;

    let extension_id = if extensions.contains_key("wordpress") {
        "wordpress"
    } else {
        extensions.keys().next().ok_or_else(|| {
            Error::validation_invalid_argument(
                "component",
                format!("Component '{}' has no extensions configured", component.id),
                None,
                None,
            )
            .with_hint(format!(
                "Add a extension: homeboy component set {} --extension <extension_id>",
                component.id
            ))
        })?
    };

    let manifest = extension::load_extension(extension_id)?;

    manifest.lint_script().map(|s| s.to_string()).ok_or_else(|| {
        Error::validation_invalid_argument(
            "extension",
            format!(
                "Extension '{}' does not have lint infrastructure configured (missing lint.extension_script)",
                extension_id
            ),
            None,
            None,
        )
    })
}

pub fn resolve_test_script(component: &Component) -> crate::Result<String> {
    let extension_id_owned: String;
    let extension_id: &str = if let Some(ref extensions) = component.extensions {
        if extensions.contains_key("wordpress") {
            "wordpress"
        } else if let Some(key) = extensions.keys().next() {
            key.as_str()
        } else if let Some(detected) = auto_detect_extension(component) {
            extension_id_owned = detected;
            &extension_id_owned
        } else {
            return Err(no_extensions_error(component));
        }
    } else if let Some(detected) = auto_detect_extension(component) {
        extension_id_owned = detected;
        &extension_id_owned
    } else {
        return Err(no_extensions_error(component));
    };

    let manifest = extension::load_extension(extension_id)?;

    manifest.test_script().map(|s| s.to_string()).ok_or_else(|| {
        Error::validation_invalid_argument(
            "extension",
            format!(
                "Extension '{}' does not have test infrastructure configured (missing test.extension_script)",
                extension_id
            ),
            None,
            None,
        )
    })
}

fn auto_detect_extension(component: &Component) -> Option<String> {
    let path = std::path::Path::new(&component.local_path);

    if path.join("wp-content").exists()
        || path.join("plugin.php").exists()
        || path.join("wordpress").exists()
        || path.join("phpcs.xml.dist").exists()
    {
        return Some("wordpress".to_string());
    }

    if path.join("Cargo.toml").exists() {
        return Some("rust".to_string());
    }

    if path.join("package.json").exists() {
        return Some("node".to_string());
    }

    None
}

fn no_extensions_error(component: &Component) -> crate::Error {
    Error::validation_invalid_argument(
        "component",
        format!("Component '{}' has no extensions configured", component.id),
        None,
        None,
    )
    .with_hint(format!(
        "Add a extension: homeboy component set {} --extension <extension_id>",
        component.id
    ))
}
