/// Environment variable names for extension execution context.
/// Extensions receive these variables when executed via `homeboy extension run`.
/// Version of the exec context protocol. Extensions can check this for compatibility.
pub const VERSION: &str = "HOMEBOY_EXEC_CONTEXT_VERSION";
/// ID of the extension being executed.
pub const EXTENSION_ID: &str = "HOMEBOY_EXTENSION_ID";
/// JSON-serialized settings (merged from app, project, and component levels).
pub const SETTINGS_JSON: &str = "HOMEBOY_SETTINGS_JSON";
/// Project ID (only set when extension requires project context).
pub const PROJECT_ID: &str = "HOMEBOY_PROJECT_ID";
/// Component ID (only set when extension requires component context).
pub const COMPONENT_ID: &str = "HOMEBOY_COMPONENT_ID";
/// Filesystem path to the extension directory.
pub const EXTENSION_PATH: &str = "HOMEBOY_EXTENSION_PATH";
/// Filesystem path to the project directory (base_path).
pub const PROJECT_PATH: &str = "HOMEBOY_PROJECT_PATH";
/// Filesystem path to the component directory.
pub const COMPONENT_PATH: &str = "HOMEBOY_COMPONENT_PATH";
/// Filesystem path to shared extension script libraries in the active runtime.
pub const SHARED_LIB_DIR: &str = "HOMEBOY_SHARED_LIB_DIR";
/// Filesystem path to agent runtime packages in the active runtime.
pub const AGENT_RUNTIMES_DIR: &str = "HOMEBOY_AGENT_RUNTIMES_DIR";

/// Run only specific steps (comma-separated). Scripts should skip steps not in this list.
pub const STEP: &str = "HOMEBOY_STEP";
/// Skip specific steps (comma-separated). Scripts should skip steps in this list.
pub const SKIP: &str = "HOMEBOY_SKIP";

/// Current version of the exec context protocol.
pub const CURRENT_VERSION: &str = "2";
