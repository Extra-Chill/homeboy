use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;

use homeboy_core::config::ConfigManager;
use homeboy_core::http::ApiClient;

use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct ApiArgs {
    /// Project ID
    pub project_id: String,

    #[command(subcommand)]
    command: ApiCommand,
}

#[derive(Subcommand)]
enum ApiCommand {
    /// Make a GET request
    Get {
        /// API endpoint (e.g., /wp/v2/posts)
        endpoint: String,
    },
    /// Make a POST request
    Post {
        /// API endpoint
        endpoint: String,
        /// JSON body
        #[arg(long)]
        body: Option<String>,
    },
    /// Make a PUT request
    Put {
        /// API endpoint
        endpoint: String,
        /// JSON body
        #[arg(long)]
        body: Option<String>,
    },
    /// Make a PATCH request
    Patch {
        /// API endpoint
        endpoint: String,
        /// JSON body
        #[arg(long)]
        body: Option<String>,
    },
    /// Make a DELETE request
    Delete {
        /// API endpoint
        endpoint: String,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiOutput {
    pub project_id: String,
    pub method: String,
    pub endpoint: String,
    pub response: Value,
}

pub fn run(args: ApiArgs, _global: &GlobalArgs) -> CmdResult<ApiOutput> {
    let project = ConfigManager::load_project(&args.project_id)?;
    let client = ApiClient::new(&args.project_id, &project.api)?;

    let (method, endpoint, response) = match args.command {
        ApiCommand::Get { endpoint } => {
            let response = client.get(&endpoint)?;
            ("GET".to_string(), endpoint, response)
        }
        ApiCommand::Post { endpoint, body } => {
            let body_value = parse_body(body)?;
            let response = client.post(&endpoint, &body_value)?;
            ("POST".to_string(), endpoint, response)
        }
        ApiCommand::Put { endpoint, body } => {
            let body_value = parse_body(body)?;
            let response = client.put(&endpoint, &body_value)?;
            ("PUT".to_string(), endpoint, response)
        }
        ApiCommand::Patch { endpoint, body } => {
            let body_value = parse_body(body)?;
            let response = client.patch(&endpoint, &body_value)?;
            ("PATCH".to_string(), endpoint, response)
        }
        ApiCommand::Delete { endpoint } => {
            let response = client.delete(&endpoint)?;
            ("DELETE".to_string(), endpoint, response)
        }
    };

    Ok((
        ApiOutput {
            project_id: args.project_id,
            method,
            endpoint,
            response,
        },
        0,
    ))
}

fn parse_body(body: Option<String>) -> homeboy_core::Result<Value> {
    match body {
        Some(json_str) => serde_json::from_str(&json_str).map_err(|e| {
            homeboy_core::Error::other(format!("Invalid JSON body: {}", e))
        }),
        None => Ok(Value::Object(serde_json::Map::new())),
    }
}
