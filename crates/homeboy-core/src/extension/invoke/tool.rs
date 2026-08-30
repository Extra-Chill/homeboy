use homeboy_core::error::{Error, Result};
use homeboy_extension_contract::exec_context;

use crate::extension::catalog::load_extension;

/// Execute a tool from an extension's vendor directory.
pub fn exec_tool(extension_id: &str, component_id: Option<&str>, args: &[String]) -> Result<i32> {
    let extension = load_extension(extension_id)?;
    let extension_path = extension
        .extension_path
        .as_deref()
        .ok_or_else(|| Error::config_missing_key("extension_path", Some(extension_id.into())))?;

    let working_dir = if let Some(component_id) = component_id {
        homeboy_core::component::load(component_id)?.local_path
    } else {
        std::env::current_dir()
            .map(|path| path.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    };

    let path = format!(
        "{}/vendor/bin:{}/node_modules/.bin:{}",
        extension_path,
        extension_path,
        std::env::var("PATH").unwrap_or_default()
    );
    let env = [
        ("PATH", path.as_str()),
        (exec_context::EXTENSION_PATH, extension_path),
        (exec_context::EXTENSION_ID, extension_id),
    ];

    Ok(homeboy_core::server::execute_local_command_interactive(
        &args.join(" "),
        Some(&working_dir),
        Some(&env),
    ))
}
