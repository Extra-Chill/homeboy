use serde::Serialize;

use crate::error::{Error, Result};

use super::{load, pin, save, unpin, PinOptions, Project};

pub struct PinUpdateOptions {
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectPinListItem {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectPinChange {
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProjectPinOutput {
    pub action: String,
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<ProjectPinListItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added: Option<ProjectPinChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub removed: Option<ProjectPinChange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated: Option<ProjectPinListItem>,
}

pub fn list_pins(project_id: &str) -> Result<ProjectPinOutput> {
    let project = load(project_id)?;

    Ok(list_pins_output(project_id, &project))
}

fn list_pins_output(project_id: &str, project: &Project) -> ProjectPinOutput {
    ProjectPinOutput {
        action: "list".to_string(),
        project_id: project_id.to_string(),
        items: Some(
            project
                .remote_files
                .pinned_files
                .iter()
                .map(|file| ProjectPinListItem {
                    path: file.path.clone(),
                    label: file.label.clone(),
                    display_name: file.display_name().to_string(),
                })
                .collect(),
        ),
        added: None,
        removed: None,
        updated: None,
    }
}

pub fn add_pin(project_id: &str, path: &str, options: PinOptions) -> Result<ProjectPinOutput> {
    pin(project_id, path, options)?;

    Ok(change_pin_output("add", project_id, path))
}

fn change_pin_output(action: &str, project_id: &str, path: &str) -> ProjectPinOutput {
    let change = ProjectPinChange {
        path: path.to_string(),
    };

    ProjectPinOutput {
        action: action.to_string(),
        project_id: project_id.to_string(),
        items: None,
        added: if action == "add" {
            Some(change.clone())
        } else {
            None
        },
        removed: if action == "remove" {
            Some(change)
        } else {
            None
        },
        updated: None,
    }
}

pub fn remove_pin(project_id: &str, path: &str) -> Result<ProjectPinOutput> {
    unpin(project_id, path)?;

    Ok(change_pin_output("remove", project_id, path))
}

pub fn update_pin(
    project_id: &str,
    path: &str,
    options: PinUpdateOptions,
) -> Result<ProjectPinOutput> {
    let mut project = load(project_id)?;
    let updated = update_pin_in_project(&mut project, path, options)?;
    save(&project)?;

    Ok(ProjectPinOutput {
        action: "update".to_string(),
        project_id: project_id.to_string(),
        items: None,
        added: None,
        removed: None,
        updated: Some(updated),
    })
}

pub fn rename_pin(project_id: &str, old_path: &str, new_path: &str) -> Result<ProjectPinOutput> {
    let mut project = load(project_id)?;
    let updated = rename_pin_in_project(&mut project, old_path, new_path)?;
    save(&project)?;

    Ok(ProjectPinOutput {
        action: "rename".to_string(),
        project_id: project_id.to_string(),
        items: None,
        added: Some(ProjectPinChange {
            path: new_path.to_string(),
        }),
        removed: Some(ProjectPinChange {
            path: old_path.to_string(),
        }),
        updated: Some(updated),
    })
}

fn update_pin_in_project(
    project: &mut Project,
    path: &str,
    options: PinUpdateOptions,
) -> Result<ProjectPinListItem> {
    let Some(label) = options.label else {
        return Err(Error::validation_invalid_argument(
            "options",
            "Provide --label to update a pin",
            Some(project.id.clone()),
            Some(vec![path.to_string()]),
        ));
    };

    let index = project
        .remote_files
        .pinned_files
        .iter()
        .position(|file| file.path == path)
        .ok_or_else(|| pin_not_found(project, path))?;
    let file = &mut project.remote_files.pinned_files[index];
    file.label = Some(label);

    Ok(ProjectPinListItem {
        path: file.path.clone(),
        label: file.label.clone(),
        display_name: file.display_name().to_string(),
    })
}

fn rename_pin_in_project(
    project: &mut Project,
    old_path: &str,
    new_path: &str,
) -> Result<ProjectPinListItem> {
    if old_path == new_path {
        return Err(Error::validation_invalid_argument(
            "new_path",
            "New pin path must differ from the current path",
            Some(project.id.clone()),
            Some(vec![old_path.to_string()]),
        ));
    }

    if project
        .remote_files
        .pinned_files
        .iter()
        .any(|file| file.path == new_path)
    {
        return Err(Error::validation_invalid_argument(
            "new_path",
            "File is already pinned",
            Some(project.id.clone()),
            Some(vec![new_path.to_string()]),
        ));
    }

    let index = project
        .remote_files
        .pinned_files
        .iter()
        .position(|file| file.path == old_path)
        .ok_or_else(|| pin_not_found(project, old_path))?;
    project.remote_files.pinned_files[index].path = new_path.to_string();
    let file = &project.remote_files.pinned_files[index];

    Ok(ProjectPinListItem {
        path: file.path.clone(),
        label: file.label.clone(),
        display_name: file.display_name().to_string(),
    })
}

fn pin_not_found(project: &Project, path: &str) -> Error {
    Error::validation_invalid_argument(
        "path",
        "file is not pinned",
        Some(project.id.clone()),
        Some(vec![path.to_string()]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{PinnedRemoteFile, RemoteFileConfig};

    fn project() -> Project {
        Project {
            id: "site".to_string(),
            remote_files: RemoteFileConfig {
                pinned_files: vec![
                    PinnedRemoteFile {
                        path: "wp-config.php".to_string(),
                        label: Some("Config".to_string()),
                    },
                    PinnedRemoteFile {
                        path: "index.php".to_string(),
                        label: None,
                    },
                ],
            },
            ..Default::default()
        }
    }

    #[test]
    fn list_pins_reports_display_name_falling_back_to_path() {
        let items = list_pins_output("site", &project())
            .items
            .expect("list items");

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].display_name, "Config");
        assert_eq!(items[1].display_name, "index.php");
    }

    #[test]
    fn update_pin_sets_label_and_failure_leaves_pins_intact() {
        let mut project = project();

        let updated = update_pin_in_project(
            &mut project,
            "index.php",
            PinUpdateOptions {
                label: Some("Front controller".to_string()),
            },
        )
        .expect("update file pin");
        assert_eq!(updated.display_name, "Front controller");
        assert_eq!(
            project.remote_files.pinned_files[1].label.as_deref(),
            Some("Front controller")
        );

        let before = project.clone();
        let err = update_pin_in_project(
            &mut project,
            "missing.php",
            PinUpdateOptions {
                label: Some("Missing".to_string()),
            },
        )
        .expect_err("missing pin update should fail");
        assert!(err.message.contains("file is not pinned"));
        assert_eq!(
            project.remote_files.pinned_files,
            before.remote_files.pinned_files
        );
    }

    #[test]
    fn rename_pin_keeps_label_and_rejects_duplicate_targets() {
        let mut project = project();

        let updated = rename_pin_in_project(&mut project, "wp-config.php", "wp-config-local.php")
            .expect("rename file pin");
        assert_eq!(updated.path, "wp-config-local.php");
        assert_eq!(updated.label.as_deref(), Some("Config"));
        assert_eq!(
            project.remote_files.pinned_files[0].path,
            "wp-config-local.php"
        );

        let before = project.clone();
        let err = rename_pin_in_project(&mut project, "wp-config-local.php", "index.php")
            .expect_err("duplicate rename should fail");
        assert!(err.message.contains("File is already pinned"));
        assert_eq!(
            project.remote_files.pinned_files,
            before.remote_files.pinned_files
        );
    }
}
