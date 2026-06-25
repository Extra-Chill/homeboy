use std::fs;
use std::sync::{Mutex, MutexGuard, OnceLock};

use homeboy::commands::report::{compare_report_artifacts_from_args, ReportCompareArgs};
use homeboy::core::observation::{NewRunRecord, ObservationStore};

#[path = "support/mod.rs"]
mod support;

const OLD_MATRIX: &str = include_str!("fixtures/report_compare/matrix-old.json");
const NEW_MATRIX: &str = include_str!("fixtures/report_compare/matrix-new.json");

fn args(old: String, new: String) -> ReportCompareArgs {
    ReportCompareArgs {
        old,
        new,
        format: "markdown".to_string(),
    }
}

struct EnvGuard {
    home: Option<String>,
    xdg_data_home: Option<String>,
}

impl EnvGuard {
    fn isolate(home: &std::path::Path) -> Self {
        let guard = Self {
            home: std::env::var("HOME").ok(),
            xdg_data_home: std::env::var("XDG_DATA_HOME").ok(),
        };
        std::env::set_var("HOME", home);
        std::env::set_var("XDG_DATA_HOME", home.join("data"));
        homeboy::core::set_artifact_root_override(Some(home.join("artifacts")));
        guard
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match &self.xdg_data_home {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        homeboy::core::set_artifact_root_override(None);
    }
}

fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

#[test]
fn report_compare_summarizes_file_artifact_deltas() {
    let dir = support::temp_dir("report-compare-files");
    let old_path = dir.path().join("matrix-old.json");
    let new_path = dir.path().join("matrix-new.json");
    fs::write(&old_path, OLD_MATRIX).expect("old fixture");
    fs::write(&new_path, NEW_MATRIX).expect("new fixture");

    let report = compare_report_artifacts_from_args(&args(
        old_path.display().to_string(),
        new_path.display().to_string(),
    ))
    .expect("compare report");

    assert_eq!(report.total.old, 3);
    assert_eq!(report.total.new, 2);
    assert_eq!(report.total.delta, -1);
    assert_eq!(report.identities.resolved, 2);
    assert_eq!(report.identities.introduced, 1);
    assert_eq!(report.identities.persistent, 1);
    assert!(report.markdown.contains("Total findings:** 3 -> 2 (-1)"));
    assert!(report
        .kinds
        .iter()
        .any(|row| row.name == "generated_document_contains_core_html" && row.delta == -1));
}

#[test]
fn report_compare_accepts_run_artifact_refs() {
    let _lock = env_lock();
    let home = support::temp_dir("report-compare-run-artifacts");
    let _env = EnvGuard::isolate(home.path());
    let store = ObservationStore::open_initialized().expect("store");
    let old_run = store
        .start_run(
            NewRunRecord::builder("matrix")
                .command("old matrix")
                .build(),
        )
        .expect("old run");
    let new_run = store
        .start_run(
            NewRunRecord::builder("matrix")
                .command("new matrix")
                .build(),
        )
        .expect("new run");
    let old_path = home.path().join("old-report.json");
    let new_path = home.path().join("new-report.json");
    support::write_file(home.path(), "old-report.json", OLD_MATRIX);
    support::write_file(home.path(), "new-report.json", NEW_MATRIX);
    let old_artifact = store
        .record_artifact(&old_run.id, "matrix-report", &old_path)
        .expect("old artifact");
    let new_artifact = store
        .record_artifact(&new_run.id, "matrix-report", &new_path)
        .expect("new artifact");

    let report = compare_report_artifacts_from_args(&args(
        format!("{}:{}", old_run.id, old_artifact.id),
        format!("{}:{}", new_run.id, new_artifact.id),
    ))
    .expect("compare report");

    assert_eq!(report.total.delta, -1);
    assert_eq!(
        report.old.source,
        format!("{}:{}", old_run.id, old_artifact.id)
    );
    assert_eq!(
        report.new.source,
        format!("{}:{}", new_run.id, new_artifact.id)
    );

    let run_report = compare_report_artifacts_from_args(&args(old_run.id.clone(), new_run.id.clone()))
        .expect("compare report from run ids");
    assert_eq!(run_report.total.delta, -1);
}
