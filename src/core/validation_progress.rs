use std::path::PathBuf;
use std::time::Instant;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, Result};
use crate::core::observation::{merge_metadata, ActiveObservation, RunRecord};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationProgressLedger {
    pub schema: String,
    pub status: String,
    pub command_count: usize,
    pub completed_count: usize,
    pub failed_count: usize,
    pub active_command: Option<ValidationCommandSummary>,
    pub last_completed_command: Option<ValidationCommandSummary>,
    pub next_command: Option<ValidationCommandSummary>,
    pub commands: Vec<ValidationCommandRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationCommandRecord {
    pub index: usize,
    pub label: String,
    pub command: String,
    pub status: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub elapsed_ms: Option<u128>,
    pub exit_code: Option<i32>,
    pub stdout_artifact: Option<String>,
    pub stderr_artifact: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationCommandSummary {
    pub index: usize,
    pub label: String,
    pub command: String,
    pub status: String,
    pub exit_code: Option<i32>,
}

pub struct ValidationProgressRecorder<'a> {
    run_dir: &'a RunDir,
    observation: Option<&'a ActiveObservation>,
    ledger: ValidationProgressLedger,
    active_started: Option<Instant>,
}

impl ValidationProgressLedger {
    pub fn new(commands: Vec<(String, String)>) -> Self {
        let command_count = commands.len();
        let mut ledger = Self {
            schema: "homeboy.validation_progress.v1".to_string(),
            status: "pending".to_string(),
            command_count,
            completed_count: 0,
            failed_count: 0,
            active_command: None,
            last_completed_command: None,
            next_command: None,
            commands: commands
                .into_iter()
                .enumerate()
                .map(|(index, (label, command))| ValidationCommandRecord {
                    index,
                    label,
                    command,
                    status: "pending".to_string(),
                    started_at: None,
                    finished_at: None,
                    elapsed_ms: None,
                    exit_code: None,
                    stdout_artifact: None,
                    stderr_artifact: None,
                })
                .collect(),
        };
        ledger.recompute();
        ledger
    }

    pub fn read_from_run_dir(run_dir: &RunDir) -> Option<Self> {
        let value = run_dir.read_step_output(run_dir::files::VALIDATION_PROGRESS)?;
        serde_json::from_value(value).ok()
    }

    pub fn from_run(run: &RunRecord) -> Option<Self> {
        serde_json::from_value(run.metadata_json.get("validation_progress")?.clone()).ok()
    }

    pub fn resume_hints(&self) -> Vec<String> {
        let mut hints = Vec::new();
        if let Some(active) = &self.active_command {
            hints.push(format!(
                "Previous run stopped while command {} (`{}`) was active.",
                active.index + 1,
                active.label
            ));
        }
        if let Some(next) = &self.next_command {
            hints.push(format!(
                "Resume from command {}: {}",
                next.index + 1,
                next.command
            ));
        } else if self.failed_count == 0 && self.completed_count == self.command_count {
            hints.push("All recorded validation commands completed successfully.".to_string());
        } else {
            hints.push("No pending command is known from this validation manifest.".to_string());
        }
        hints
    }

    fn mark_started(&mut self, index: usize) {
        if let Some(command) = self.commands.get_mut(index) {
            command.status = "running".to_string();
            command.started_at = Some(Utc::now().to_rfc3339());
            command.finished_at = None;
            command.elapsed_ms = None;
            command.exit_code = None;
        }
        self.recompute();
    }

    fn mark_finished(
        &mut self,
        index: usize,
        exit_code: i32,
        elapsed_ms: u128,
        stdout_artifact: Option<String>,
        stderr_artifact: Option<String>,
    ) {
        if let Some(command) = self.commands.get_mut(index) {
            command.status = if exit_code == 0 { "passed" } else { "failed" }.to_string();
            command.finished_at = Some(Utc::now().to_rfc3339());
            command.elapsed_ms = Some(elapsed_ms);
            command.exit_code = Some(exit_code);
            command.stdout_artifact = stdout_artifact;
            command.stderr_artifact = stderr_artifact;
        }
        self.recompute();
    }

    fn recompute(&mut self) {
        self.completed_count = self
            .commands
            .iter()
            .filter(|command| command.status == "passed")
            .count();
        self.failed_count = self
            .commands
            .iter()
            .filter(|command| command.status == "failed")
            .count();
        self.active_command = self
            .commands
            .iter()
            .find(|command| command.status == "running")
            .map(ValidationCommandSummary::from);
        self.last_completed_command = self
            .commands
            .iter()
            .rev()
            .find(|command| matches!(command.status.as_str(), "passed" | "failed"))
            .map(ValidationCommandSummary::from);
        self.next_command = self
            .commands
            .iter()
            .find(|command| command.status == "pending")
            .map(ValidationCommandSummary::from);
        self.status = if self.failed_count > 0 {
            "failed".to_string()
        } else if self.active_command.is_some() {
            "running".to_string()
        } else if self.completed_count == self.command_count {
            "passed".to_string()
        } else {
            "pending".to_string()
        };
    }
}

impl From<&ValidationCommandRecord> for ValidationCommandSummary {
    fn from(command: &ValidationCommandRecord) -> Self {
        Self {
            index: command.index,
            label: command.label.clone(),
            command: command.command.clone(),
            status: command.status.clone(),
            exit_code: command.exit_code,
        }
    }
}

impl<'a> ValidationProgressRecorder<'a> {
    pub fn new(
        run_dir: &'a RunDir,
        observation: Option<&'a ActiveObservation>,
        commands: Vec<(String, String)>,
    ) -> Result<Self> {
        let recorder = Self {
            run_dir,
            observation,
            ledger: ValidationProgressLedger::new(commands),
            active_started: None,
        };
        recorder.persist()?;
        Ok(recorder)
    }

    pub fn start(&mut self, index: usize) -> Result<()> {
        self.active_started = Some(Instant::now());
        self.ledger.mark_started(index);
        self.persist()
    }

    pub fn finish(
        &mut self,
        index: usize,
        exit_code: i32,
        stdout_artifact: Option<String>,
        stderr_artifact: Option<String>,
    ) -> Result<()> {
        let elapsed_ms = self
            .active_started
            .take()
            .map(|started| started.elapsed().as_millis())
            .unwrap_or_default();
        self.ledger.mark_finished(
            index,
            exit_code,
            elapsed_ms,
            stdout_artifact,
            stderr_artifact,
        );
        self.persist()
    }

    pub fn ledger(&self) -> &ValidationProgressLedger {
        &self.ledger
    }

    fn persist(&self) -> Result<()> {
        let value = serde_json::to_value(&self.ledger).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize validation progress".to_string()),
            )
        })?;
        std::fs::write(
            self.run_dir.step_file(run_dir::files::VALIDATION_PROGRESS),
            serde_json::to_string_pretty(&value).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("write validation progress".to_string()),
                )
            })?,
        )
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("write validation progress".to_string()),
            )
        })?;

        if let Some(observation) = self.observation {
            let metadata = merge_metadata(
                observation.initial_metadata().clone(),
                serde_json::json!({ "validation_progress": value }),
            );
            observation
                .store()
                .update_run_metadata(observation.run_id(), metadata)?;
        }

        Ok(())
    }
}

pub fn validation_progress_metadata(run_dir: &RunDir) -> serde_json::Value {
    ValidationProgressLedger::read_from_run_dir(run_dir)
        .map(|ledger| serde_json::json!({ "validation_progress": ledger }))
        .unwrap_or_else(|| serde_json::json!({}))
}

pub fn write_command_artifact(
    run_dir: &RunDir,
    command_index: usize,
    stream: &str,
    contents: &str,
) -> Result<Option<String>> {
    if contents.is_empty() {
        return Ok(None);
    }
    let relative = PathBuf::from("validation-progress")
        .join(format!("command-{}-{stream}.log", command_index + 1));
    let path = run_dir.path().join(&relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("create validation progress artifact dir".to_string()),
            )
        })?;
    }
    std::fs::write(&path, contents).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("write validation progress artifact".to_string()),
        )
    })?;
    Ok(Some(relative.to_string_lossy().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::observation::{NewRunRecord, RunStatus};
    use crate::test_support::with_isolated_home;

    #[test]
    fn ledger_tracks_active_completed_and_next_commands() {
        let mut ledger = ValidationProgressLedger::new(vec![
            ("first".to_string(), "check one".to_string()),
            ("second".to_string(), "check two".to_string()),
        ]);

        ledger.mark_started(0);

        assert_eq!(ledger.status, "running");
        assert_eq!(ledger.active_command.as_ref().unwrap().command, "check one");
        assert_eq!(ledger.next_command.as_ref().unwrap().command, "check two");

        ledger.mark_finished(0, 0, 25, Some("stdout.log".to_string()), None);

        assert_eq!(ledger.status, "pending");
        assert_eq!(ledger.completed_count, 1);
        assert_eq!(
            ledger.last_completed_command.as_ref().unwrap().command,
            "check one"
        );
        assert_eq!(ledger.next_command.as_ref().unwrap().command, "check two");
        assert!(ledger
            .resume_hints()
            .iter()
            .any(|hint| hint.contains("Resume from command 2: check two")));
    }

    #[test]
    fn recorder_mirrors_progress_into_running_observation_metadata() {
        with_isolated_home(|_| {
            let run_dir = RunDir::create().expect("run dir");
            let observation = ActiveObservation::start(
                NewRunRecord::builder("test")
                    .component_id("fixture")
                    .command("homeboy test fixture")
                    .metadata(serde_json::json!({
                        "source": "test",
                        "run_dir": run_dir.path().to_string_lossy(),
                    }))
                    .build(),
            )
            .expect("observation");
            let run_id = observation.run_id().to_string();

            let mut recorder = ValidationProgressRecorder::new(
                &run_dir,
                Some(&observation),
                vec![
                    ("first".to_string(), "check one".to_string()),
                    ("second".to_string(), "check two".to_string()),
                ],
            )
            .expect("recorder");
            recorder.start(0).expect("start command");
            recorder.finish(0, 0, None, None).expect("finish command");

            let stored = observation
                .store()
                .get_run(&run_id)
                .expect("read run")
                .expect("run exists");
            let ledger = ValidationProgressLedger::from_run(&stored).expect("metadata ledger");
            assert_eq!(ledger.completed_count, 1);
            assert_eq!(ledger.next_command.as_ref().unwrap().command, "check two");

            observation.finish(RunStatus::Pass, None);
            run_dir.cleanup();
        });
    }
}
