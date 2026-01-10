use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::fs;
use homeboy_core::config::ConfigManager;
use homeboy_core::output::{print_success, print_error};
use homeboy_core::version::{parse_version, default_pattern_for_file, increment_version};

#[derive(Args)]
pub struct VersionArgs {
    #[command(subcommand)]
    command: VersionCommand,
}

#[derive(Subcommand)]
enum VersionCommand {
    /// Show current version of a component
    Show {
        /// Component ID
        component_id: String,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Bump version of a component
    Bump {
        /// Component ID
        component_id: String,
        /// Version bump type
        bump_type: BumpType,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, ValueEnum)]
enum BumpType {
    Patch,
    Minor,
    Major,
}

impl BumpType {
    fn as_str(&self) -> &'static str {
        match self {
            BumpType::Patch => "patch",
            BumpType::Minor => "minor",
            BumpType::Major => "major",
        }
    }
}

pub fn run(args: VersionArgs) {
    match args.command {
        VersionCommand::Show { component_id, json } => show(&component_id, json),
        VersionCommand::Bump { component_id, bump_type, json } => bump(&component_id, bump_type, json),
    }
}

fn get_version_config(component_id: &str, json: bool) -> Option<(String, String, Option<String>)> {
    let component = match ConfigManager::load_component(component_id) {
        Ok(c) => c,
        Err(e) => {
            if json { print_error(e.code(), &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return None;
        }
    };

    let version_file = match &component.version_file {
        Some(f) => f.clone(),
        None => {
            let msg = format!("Component '{}' has no version_file configured", component_id);
            if json { print_error("NO_VERSION_FILE", &msg); }
            else { eprintln!("Error: {}", msg); }
            return None;
        }
    };

    let full_path = if version_file.starts_with('/') {
        version_file.clone()
    } else {
        format!("{}/{}", component.local_path, version_file)
    };

    Some((full_path, version_file, component.version_pattern))
}

fn show(component_id: &str, json: bool) {
    let (full_path, version_file, custom_pattern) = match get_version_config(component_id, json) {
        Some(c) => c,
        None => return,
    };

    let content = match fs::read_to_string(&full_path) {
        Ok(c) => c,
        Err(e) => {
            if json { print_error("READ_ERROR", &e.to_string()); }
            else { eprintln!("Error reading {}: {}", full_path, e); }
            return;
        }
    };

    let pattern = custom_pattern
        .as_deref()
        .unwrap_or_else(|| default_pattern_for_file(&version_file));

    let version = match parse_version(&content, pattern) {
        Some(v) => v,
        None => {
            let msg = format!("Could not parse version from {} using pattern: {}", version_file, pattern);
            if json { print_error("PARSE_ERROR", &msg); }
            else { eprintln!("Error: {}", msg); }
            return;
        }
    };

    if json {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct ShowResult {
            component_id: String,
            version: String,
            version_file: String,
        }

        print_success(ShowResult {
            component_id: component_id.to_string(),
            version,
            version_file,
        });
    } else {
        println!("{}", version);
    }
}

fn bump(component_id: &str, bump_type: BumpType, json: bool) {
    let (full_path, version_file, custom_pattern) = match get_version_config(component_id, json) {
        Some(c) => c,
        None => return,
    };

    let content = match fs::read_to_string(&full_path) {
        Ok(c) => c,
        Err(e) => {
            if json { print_error("READ_ERROR", &e.to_string()); }
            else { eprintln!("Error reading {}: {}", full_path, e); }
            return;
        }
    };

    let pattern = custom_pattern
        .as_deref()
        .unwrap_or_else(|| default_pattern_for_file(&version_file));

    let old_version = match parse_version(&content, pattern) {
        Some(v) => v,
        None => {
            let msg = format!("Could not parse version from {} using pattern: {}", version_file, pattern);
            if json { print_error("PARSE_ERROR", &msg); }
            else { eprintln!("Error: {}", msg); }
            return;
        }
    };

    let new_version = match increment_version(&old_version, bump_type.as_str()) {
        Some(v) => v,
        None => {
            let msg = format!("Invalid version format: {}", old_version);
            if json { print_error("INVALID_VERSION", &msg); }
            else { eprintln!("Error: {}", msg); }
            return;
        }
    };

    // Replace version in content
    let new_content = content.replace(&old_version, &new_version);

    // Write back
    if let Err(e) = fs::write(&full_path, &new_content) {
        if json { print_error("WRITE_ERROR", &e.to_string()); }
        else { eprintln!("Error writing {}: {}", full_path, e); }
        return;
    }

    if json {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct BumpResult {
            component_id: String,
            old_version: String,
            new_version: String,
            version_file: String,
        }

        print_success(BumpResult {
            component_id: component_id.to_string(),
            old_version,
            new_version,
            version_file,
        });
    } else {
        println!("{} → {}", old_version, new_version);
    }
}
