use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{self, BufRead, Write};

use homeboy::config::ConfigManager;
use homeboy::http::ApiClient;
use homeboy::keychain;

use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    command: AuthCommand,
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Authenticate with a project's API
    Login {
        /// Project ID
        #[arg(long)]
        project: String,

        /// Username or email
        #[arg(long)]
        identifier: Option<String>,

        /// Password (or read from stdin)
        #[arg(long)]
        password: Option<String>,
    },

    /// Clear stored authentication for a project
    Logout {
        /// Project ID
        #[arg(long)]
        project: String,
    },

    /// Show authentication status for a project
    Status {
        /// Project ID
        #[arg(long)]
        project: String,
    },
}

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "camelCase")]
pub enum AuthOutput {
    Login { project_id: String, success: bool },
    Logout { project_id: String },
    Status { project_id: String, authenticated: bool },
}

pub fn run(args: AuthArgs, _global: &GlobalArgs) -> CmdResult<AuthOutput> {
    match args.command {
        AuthCommand::Login {
            project,
            identifier,
            password,
        } => run_login(&project, identifier, password),
        AuthCommand::Logout { project } => run_logout(&project),
        AuthCommand::Status { project } => run_status(&project),
    }
}

fn run_login(
    project_id: &str,
    identifier: Option<String>,
    password: Option<String>,
) -> CmdResult<AuthOutput> {
    let project = ConfigManager::load_project(project_id)?;
    let client = ApiClient::new(project_id, &project.api)?;

    // Get credentials - prompt if not provided
    let identifier = match identifier {
        Some(id) => id,
        None => prompt("Username/Email: ")?,
    };

    let password = match password {
        Some(pw) => pw,
        None => prompt_password("Password: ")?,
    };

    // Generate device ID if we don't have one stored
    let device_id = keychain::get(project_id, "device_id")?
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    // Store device ID for future use
    keychain::store(project_id, "device_id", &device_id)?;

    // Build credentials map
    let mut credentials = HashMap::new();
    credentials.insert("identifier".to_string(), identifier);
    credentials.insert("password".to_string(), password);
    credentials.insert("device_id".to_string(), device_id);

    // Execute login flow
    client.login(&credentials)?;

    Ok((
        AuthOutput::Login {
            project_id: project_id.to_string(),
            success: true,
        },
        0,
    ))
}

fn run_logout(project_id: &str) -> CmdResult<AuthOutput> {
    let project = ConfigManager::load_project(project_id)?;
    let client = ApiClient::new(project_id, &project.api)?;

    client.logout()?;

    Ok((
        AuthOutput::Logout {
            project_id: project_id.to_string(),
        },
        0,
    ))
}

fn run_status(project_id: &str) -> CmdResult<AuthOutput> {
    let project = ConfigManager::load_project(project_id)?;
    let client = ApiClient::new(project_id, &project.api)?;

    let authenticated = client.is_authenticated();

    Ok((
        AuthOutput::Status {
            project_id: project_id.to_string(),
            authenticated,
        },
        0,
    ))
}

fn prompt(message: &str) -> homeboy::Result<String> {
    eprint!("{}", message);
    io::stderr().flush().ok();

    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).map_err(|e| {
        homeboy::Error::new(
            homeboy::ErrorCode::InternalIoError,
            format!("Failed to read input: {}", e),
            serde_json::Value::Null,
        )
    })?;

    Ok(line.trim().to_string())
}

fn prompt_password(message: &str) -> homeboy::Result<String> {
    eprint!("{}", message);
    io::stderr().flush().ok();

    // For now, just read from stdin (could use rpassword for hidden input)
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).map_err(|e| {
        homeboy::Error::new(
            homeboy::ErrorCode::InternalIoError,
            format!("Failed to read input: {}", e),
            serde_json::Value::Null,
        )
    })?;

    Ok(line.trim().to_string())
}
