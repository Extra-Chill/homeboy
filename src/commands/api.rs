use clap::{Args, Subcommand};
use homeboy::api;

use super::CmdResult;

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

pub fn run(args: ApiArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<api::ApiOutput> {
    let input = build_api_json(&args);
    api::run(&input)
}

fn build_api_json(args: &ApiArgs) -> String {
    let (method, endpoint, body) = match &args.command {
        ApiCommand::Get { endpoint } => ("GET", endpoint.clone(), None),
        ApiCommand::Post { endpoint, body } => ("POST", endpoint.clone(), body.clone()),
        ApiCommand::Put { endpoint, body } => ("PUT", endpoint.clone(), body.clone()),
        ApiCommand::Patch { endpoint, body } => ("PATCH", endpoint.clone(), body.clone()),
        ApiCommand::Delete { endpoint } => ("DELETE", endpoint.clone(), None),
    };

    let body_value: Option<serde_json::Value> =
        body.as_ref().and_then(|b| serde_json::from_str(b).ok());

    serde_json::json!({
        "projectId": args.project_id,
        "method": method,
        "endpoint": endpoint,
        "body": body_value,
    })
    .to_string()
}
