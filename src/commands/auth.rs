use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::HashMap;

use homeboy::auth::{self, AuthStatus, LoginResult, LogoutResult};

use super::{CmdResult, GlobalArgs};
use crate::tty::{prompt, prompt_password};

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
#[serde(untagged)]
pub enum AuthOutput {
    Login(LoginResult),
    Logout(LogoutResult),
    Status(AuthStatus),
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
    let identifier = match identifier {
        Some(id) => id,
        None => prompt("Username/Email: ")?,
    };

    let password = match password {
        Some(pw) => pw,
        None => prompt_password("Password: ")?,
    };

    let mut credentials = HashMap::new();
    credentials.insert("identifier".to_string(), identifier);
    credentials.insert("password".to_string(), password);

    let result = auth::login(project_id, credentials)?;
    Ok((AuthOutput::Login(result), 0))
}

fn run_logout(project_id: &str) -> CmdResult<AuthOutput> {
    let result = auth::logout(project_id)?;
    Ok((AuthOutput::Logout(result), 0))
}

fn run_status(project_id: &str) -> CmdResult<AuthOutput> {
    let result = auth::status(project_id)?;
    Ok((AuthOutput::Status(result), 0))
}
