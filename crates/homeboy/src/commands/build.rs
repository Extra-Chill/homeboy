use clap::Args;
use serde::Serialize;
use std::process::Command;
use homeboy_core::config::ConfigManager;
use homeboy_core::output::{print_success, print_error};

#[derive(Args)]
pub struct BuildArgs {
    /// Component ID
    pub component_id: String,
    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: BuildArgs) {
    let component = match ConfigManager::load_component(&args.component_id) {
        Ok(c) => c,
        Err(e) => {
            if args.json { print_error(e.code(), &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    let build_cmd = match &component.build_command {
        Some(cmd) => cmd,
        None => {
            let msg = format!("Component '{}' has no build_command configured", args.component_id);
            if args.json { print_error("NO_BUILD_COMMAND", &msg); }
            else { eprintln!("Error: {}", msg); }
            return;
        }
    };

    if !args.json {
        println!("Building {}...", component.name);
    }

    let output = match Command::new("sh")
        .args(["-c", build_cmd])
        .current_dir(&component.local_path)
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            if args.json { print_error("BUILD_ERROR", &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if args.json {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct BuildResult {
            component_id: String,
            success: bool,
            build_command: String,
            stdout: String,
            stderr: String,
        }

        if output.status.success() {
            print_success(BuildResult {
                component_id: args.component_id,
                success: true,
                build_command: build_cmd.to_string(),
                stdout,
                stderr,
            });
        } else {
            print_error("BUILD_FAILED", &stderr);
        }
    } else {
        if !stdout.is_empty() {
            print!("{}", stdout);
        }
        if !stderr.is_empty() {
            eprint!("{}", stderr);
        }

        if output.status.success() {
            println!("Build complete.");
        } else {
            eprintln!("Build failed.");
        }
    }
}
