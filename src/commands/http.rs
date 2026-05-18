use clap::{Args, Subcommand};

use homeboy::core::http_request::{self, HttpRequestInput, HttpRequestOutput};

use super::{parse_key_val, CmdResult, GlobalArgs};

#[derive(Args)]
pub struct HttpArgs {
    #[command(subcommand)]
    command: HttpCommand,
}

#[derive(Subcommand)]
enum HttpCommand {
    /// Make a GET request to a full URL
    Get(RequestArgs),
    /// Make an arbitrary HTTP request to a full URL
    Request {
        /// HTTP method
        method: String,

        #[command(flatten)]
        args: RequestArgs,
    },
}

#[derive(Args)]
struct RequestArgs {
    /// Full URL to request
    url: String,

    /// Optional proxy URL, e.g. socks5://127.0.0.1:8080
    #[arg(long)]
    proxy: Option<String>,

    /// Auth profile from `homeboy auth profile ...`
    #[arg(long)]
    auth_profile: Option<String>,

    /// Header in `Name: value` format; repeatable
    #[arg(long = "header")]
    headers: Vec<String>,

    /// JSON request body
    #[arg(long)]
    json: Option<String>,

    /// Form field as key=value; repeatable
    #[arg(long = "form", value_parser = parse_key_val)]
    form: Vec<(String, String)>,
}

pub fn run(args: HttpArgs, _global: &GlobalArgs) -> CmdResult<HttpRequestOutput> {
    let input = match args.command {
        HttpCommand::Get(args) => build_input("GET", args),
        HttpCommand::Request { method, args } => build_input(&method, args),
    };

    let output = http_request::run(input)?;
    Ok((output, 0))
}

fn build_input(method: &str, args: RequestArgs) -> HttpRequestInput {
    HttpRequestInput {
        method: method.to_string(),
        url: args.url,
        proxy_url: args.proxy,
        auth_profile: args.auth_profile,
        headers: args.headers,
        json_body: args.json,
        form_body: args.form,
    }
}
