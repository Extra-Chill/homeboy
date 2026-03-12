use serde::Serialize;

use crate::error::Result;

use super::{load, pin, unpin, PinOptions, PinType};

#[derive(Debug, Clone, Serialize)]
pub struct ProjectPinListItem {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectPinChange {
    pub path: String,
    pub r#type: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectPinOutput {
    pub action: String,
    pub project_id: String,
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<ProjectPinListItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<ProjectPinChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<ProjectPinChange>,
}

pub fn list_pins(project_id: &str, pin_type: PinType) -> Result<ProjectPinOutput> {
    let project = load(project_id)?;

    let (items, type_string) = match pin_type {
        PinType::File => (
            project
                .remote_files
                .pinned_files
                .iter()
                .map(|file| ProjectPinListItem {
                    path: file.path.clone(),
                    label: file.label.clone(),
                    display_name: file.display_name().to_string(),
                    tail_lines: None,
                })
                .collect(),
            "file",
        ),
        PinType::Log => (
            project
                .remote_logs
                .pinned_logs
                .iter()
                .map(|log| ProjectPinListItem {
                    path: log.path.clone(),
                    label: log.label.clone(),
                    display_name: log.display_name().to_string(),
                    tail_lines: Some(log.tail_lines),
                })
                .collect(),
            "log",
        ),
    };

    Ok(ProjectPinOutput {
        action: "list".to_string(),
        project_id: project_id.to_string(),
        r#type: type_string.to_string(),
        items: Some(items),
        added: None,
        removed: None,
    })
}

pub fn add_pin(
    project_id: &str,
    pin_type: PinType,
    path: &str,
    options: PinOptions,
) -> Result<ProjectPinOutput> {
    let type_string = pin_type_name(pin_type).to_string();
    pin(project_id, pin_type, path, options)?;

    Ok(ProjectPinOutput {
        action: "add".to_string(),
        project_id: project_id.to_string(),
        r#type: type_string.clone(),
        items: None,
        added: Some(ProjectPinChange {
            path: path.to_string(),
            r#type: type_string,
        }),
        removed: None,
    })
}

pub fn remove_pin(project_id: &str, pin_type: PinType, path: &str) -> Result<ProjectPinOutput> {
    let type_string = pin_type_name(pin_type).to_string();
    unpin(project_id, pin_type, path)?;

    Ok(ProjectPinOutput {
        action: "remove".to_string(),
        project_id: project_id.to_string(),
        r#type: type_string.clone(),
        items: None,
        added: None,
        removed: Some(ProjectPinChange {
            path: path.to_string(),
            r#type: type_string,
        }),
    })
}

fn pin_type_name(pin_type: PinType) -> &'static str {
    match pin_type {
        PinType::File => "file",
        PinType::Log => "log",
    }
}
