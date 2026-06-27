//! Best-effort run-local resource summaries.

use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ResourceSnapshot {
    load_average: Option<LoadAverage>,
    homeboy_rss_bytes: Option<u64>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunResourceSummary {
    pub label: Option<String>,
    pub pid: u32,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u128,
    pub platform: String,
    pub load_average_before: Option<LoadAverage>,
    pub load_average_after: Option<LoadAverage>,
    pub homeboy_rss_bytes_before: Option<u64>,
    pub homeboy_rss_bytes_after: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_children: Vec<ExtensionChildResourceSummary>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildProcessIdentity {
    pub root_pid: u32,
    pub command_label: String,
}

/// Peak resource sample (RSS + CPU) shared by child-resource reporting structs.
/// Flattened so the emitted JSON keys (`sampled_peak_rss_bytes`,
/// `sampled_peak_cpu_percent`) are identical to the previously inlined fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChildResourcePeakSample {
    pub sampled_peak_rss_bytes: Option<u64>,
    pub sampled_peak_cpu_percent: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionChildResourceSummary {
    #[serde(flatten)]
    pub child: ChildProcessIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u128,
    #[serde(flatten)]
    pub peak: ChildResourcePeakSample,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled_peak_at_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled_peak_child_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<ExtensionChildResourceSample>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionChildResourceSample {
    pub elapsed_ms: u128,
    pub timestamp: String,
    pub root_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub rss_bytes: u64,
    pub cpu_percent: f64,
    pub child_count: usize,
    pub processes: Vec<ExtensionChildProcessSample>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionChildProcessSample {
    pub pid: u32,
    pub parent_pid: u32,
    pub rss_bytes: u64,
    pub cpu_percent: f64,
    pub command: String,
}

trait ResourceProbe {
    fn snapshot(&self) -> ResourceSnapshot;
}

#[derive(Debug, Default)]
struct SystemResourceProbe;

impl ResourceProbe for SystemResourceProbe {
    fn snapshot(&self) -> ResourceSnapshot {
        let mut warnings = Vec::new();
        let load_average = system_load_average(&mut warnings);
        let homeboy_rss_bytes = homeboy_rss_bytes(&mut warnings);

        ResourceSnapshot {
            load_average,
            homeboy_rss_bytes,
            warnings,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResourceSummaryRun {
    label: Option<String>,
    pid: u32,
    started_at: DateTime<Utc>,
    started_instant: Instant,
    platform: String,
    before: ResourceSnapshot,
}

impl ResourceSummaryRun {
    pub fn start(label: Option<String>) -> Self {
        Self::start_with_probe(label, &SystemResourceProbe)
    }

    fn start_with_probe(label: Option<String>, probe: &impl ResourceProbe) -> Self {
        Self {
            label,
            pid: std::process::id(),
            started_at: Utc::now(),
            started_instant: Instant::now(),
            platform: std::env::consts::OS.to_string(),
            before: probe.snapshot(),
        }
    }

    pub fn finish(&self) -> RunResourceSummary {
        self.finish_with_probe(&SystemResourceProbe)
    }

    fn finish_with_probe(&self, probe: &impl ResourceProbe) -> RunResourceSummary {
        let after = probe.snapshot();
        RunResourceSummary::from_snapshots(
            self.label.clone(),
            self.pid,
            self.started_at.to_rfc3339(),
            Utc::now().to_rfc3339(),
            self.started_instant.elapsed().as_millis(),
            self.platform.clone(),
            self.before.clone(),
            after,
            Vec::new(),
        )
    }

    pub fn write_to_run_dir(&self, run_dir: &RunDir) -> Result<RunResourceSummary> {
        let mut summary = self.finish();
        summary.extension_children = read_extension_child_resources(run_dir);
        write_summary(run_dir, &summary)?;
        Ok(summary)
    }
}

impl RunResourceSummary {
    #[allow(clippy::too_many_arguments)]
    fn from_snapshots(
        label: Option<String>,
        pid: u32,
        started_at: String,
        finished_at: String,
        duration_ms: u128,
        platform: String,
        before: ResourceSnapshot,
        after: ResourceSnapshot,
        extension_children: Vec<ExtensionChildResourceSummary>,
    ) -> Self {
        let mut warnings = before.warnings;
        warnings.extend(after.warnings);
        warnings.sort();
        warnings.dedup();

        Self {
            label,
            pid,
            started_at,
            finished_at,
            duration_ms,
            platform,
            load_average_before: before.load_average,
            load_average_after: after.load_average,
            homeboy_rss_bytes_before: before.homeboy_rss_bytes,
            homeboy_rss_bytes_after: after.homeboy_rss_bytes,
            extension_children,
            warnings,
        }
    }
}

pub fn record_extension_child_resource(
    run_dir_path: &Path,
    summary: &ExtensionChildResourceSummary,
) -> Result<()> {
    let dir = run_dir_path.join(run_dir::files::EXTENSION_CHILDREN_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("create extension child resource dir".into()),
        )
    })?;

    let filename = format!(
        "{}-{}.json",
        summary.child.root_pid,
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let path = dir.join(filename);
    let json = serde_json::to_string_pretty(summary).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("serialize extension child resource".into()),
        )
    })?;
    std::fs::write(&path, json).map_err(|e| {
        Error::internal_io(e.to_string(), Some("write extension child resource".into()))
    })
}

pub fn read_extension_child_resources(run_dir: &RunDir) -> Vec<ExtensionChildResourceSummary> {
    let dir = run_dir.step_file(run_dir::files::EXTENSION_CHILDREN_DIR);
    let mut summaries = Vec::new();

    let Ok(entries) = std::fs::read_dir(&dir) else {
        return summaries;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(summary) = serde_json::from_str::<ExtensionChildResourceSummary>(&content) {
            summaries.push(summary);
        }
    }

    summaries.sort_by(|a, b| {
        a.started_at
            .cmp(&b.started_at)
            .then(a.child.root_pid.cmp(&b.child.root_pid))
    });
    summaries
}

fn write_summary(run_dir: &RunDir, summary: &RunResourceSummary) -> Result<()> {
    let path = run_dir.step_file(run_dir::files::RESOURCE_SUMMARY);
    let json = serde_json::to_string_pretty(summary).map_err(|e| {
        Error::internal_io(e.to_string(), Some("serialize resource summary".into()))
    })?;
    std::fs::write(&path, json)
        .map_err(|e| Error::internal_io(e.to_string(), Some("write resource summary".into())))
}

#[cfg(unix)]
fn system_load_average(warnings: &mut Vec<String>) -> Option<LoadAverage> {
    let mut values = [0.0_f64; 3];
    let count = unsafe { libc::getloadavg(values.as_mut_ptr(), values.len() as libc::c_int) };
    if count == 3 {
        Some(LoadAverage {
            one: values[0],
            five: values[1],
            fifteen: values[2],
        })
    } else {
        warnings.push("load_average_unsupported".to_string());
        None
    }
}

#[cfg(not(unix))]
fn system_load_average(warnings: &mut Vec<String>) -> Option<LoadAverage> {
    warnings.push("load_average_unsupported".to_string());
    None
}

#[cfg(unix)]
fn homeboy_rss_bytes(warnings: &mut Vec<String>) -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    if result != 0 {
        warnings.push("homeboy_rss_unsupported".to_string());
        return None;
    }

    let max_rss = unsafe { usage.assume_init().ru_maxrss };
    if max_rss < 0 {
        warnings.push("homeboy_rss_unsupported".to_string());
        return None;
    }

    #[cfg(target_os = "macos")]
    {
        Some(max_rss as u64)
    }

    #[cfg(not(target_os = "macos"))]
    {
        Some((max_rss as u64).saturating_mul(1024))
    }
}

#[cfg(not(unix))]
fn homeboy_rss_bytes(warnings: &mut Vec<String>) -> Option<u64> {
    warnings.push("homeboy_rss_unsupported".to_string());
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    #[derive(Debug)]
    struct FakeProbe {
        snapshot: ResourceSnapshot,
    }

    impl ResourceProbe for FakeProbe {
        fn snapshot(&self) -> ResourceSnapshot {
            self.snapshot.clone()
        }
    }

    fn snapshot(load_one: f64, rss: u64, warning: Option<&str>) -> ResourceSnapshot {
        ResourceSnapshot {
            load_average: Some(LoadAverage {
                one: load_one,
                five: load_one + 1.0,
                fifteen: load_one + 2.0,
            }),
            homeboy_rss_bytes: Some(rss),
            warnings: warning.into_iter().map(str::to_string).collect(),
        }
    }

    #[test]
    fn summary_combines_before_after_snapshots() {
        let started_at = "2026-04-30T00:00:00+00:00".to_string();
        let finished_at = "2026-04-30T00:00:00.025+00:00".to_string();
        let summary = RunResourceSummary::from_snapshots(
            Some("lint homeboy".to_string()),
            42,
            started_at,
            finished_at,
            25,
            "test-os".to_string(),
            snapshot(1.0, 100, Some("same_warning")),
            snapshot(2.0, 150, Some("same_warning")),
            Vec::new(),
        );

        assert_eq!(summary.label.as_deref(), Some("lint homeboy"));
        assert_eq!(summary.pid, 42);
        assert_eq!(summary.duration_ms, 25);
        assert_eq!(summary.platform, "test-os");
        assert_eq!(summary.load_average_before.unwrap().one, 1.0);
        assert_eq!(summary.load_average_after.unwrap().one, 2.0);
        assert_eq!(summary.homeboy_rss_bytes_before, Some(100));
        assert_eq!(summary.homeboy_rss_bytes_after, Some(150));
        assert_eq!(summary.warnings, vec!["same_warning".to_string()]);
    }

    #[test]
    fn start_and_finish_use_injected_probe_values() {
        let before_probe = FakeProbe {
            snapshot: snapshot(3.0, 300, None),
        };
        let after_probe = FakeProbe {
            snapshot: ResourceSnapshot {
                load_average: None,
                homeboy_rss_bytes: None,
                warnings: vec!["load_average_unsupported".to_string()],
            },
        };

        let run = ResourceSummaryRun::start_with_probe(Some("test".to_string()), &before_probe);
        let summary = run.finish_with_probe(&after_probe);

        assert_eq!(summary.label.as_deref(), Some("test"));
        assert_eq!(summary.load_average_before.unwrap().one, 3.0);
        assert!(summary.load_average_after.is_none());
        assert_eq!(summary.homeboy_rss_bytes_before, Some(300));
        assert_eq!(summary.homeboy_rss_bytes_after, None);
        assert_eq!(
            summary.warnings,
            vec!["load_average_unsupported".to_string()]
        );
    }

    #[test]
    fn writes_summary_to_run_dir_artifact() {
        with_isolated_home(|_| {
            let run_dir = RunDir::create().expect("run dir");
            let summary = RunResourceSummary::from_snapshots(
                Some("lint".to_string()),
                7,
                Utc::now().to_rfc3339(),
                Utc::now().to_rfc3339(),
                1,
                "test".to_string(),
                ResourceSnapshot {
                    load_average: None,
                    homeboy_rss_bytes: None,
                    warnings: vec!["load_average_unsupported".to_string()],
                },
                ResourceSnapshot {
                    load_average: None,
                    homeboy_rss_bytes: None,
                    warnings: vec!["homeboy_rss_unsupported".to_string()],
                },
                vec![ExtensionChildResourceSummary {
                    child: ChildProcessIdentity {
                        root_pid: 99,
                        command_label: "runner".to_string(),
                    },
                    phase: None,
                    started_at: Utc::now().to_rfc3339(),
                    finished_at: Utc::now().to_rfc3339(),
                    duration_ms: 10,
                    peak: ChildResourcePeakSample {
                        sampled_peak_rss_bytes: Some(2048),
                        sampled_peak_cpu_percent: Some(12.5),
                    },
                    sampled_peak_at_ms: Some(5),
                    sampled_peak_child_count: Some(1),
                    samples: Vec::new(),
                    warnings: Vec::new(),
                }],
            );

            write_summary(&run_dir, &summary).expect("write summary");
            let output = run_dir
                .read_step_output(run_dir::files::RESOURCE_SUMMARY)
                .expect("resource summary json");

            assert_eq!(output["label"], "lint");
            assert_eq!(output["pid"], 7);
            assert_eq!(output["warnings"].as_array().unwrap().len(), 2);
            assert_eq!(output["extension_children"][0]["root_pid"], 99);

            run_dir.cleanup();
        });
    }

    #[test]
    fn test_write_to_run_dir() {
        with_isolated_home(|_| {
            let run_dir = RunDir::create().expect("run dir");
            let resource_run = ResourceSummaryRun::start(Some("lint homeboy".to_string()));

            let summary = resource_run
                .write_to_run_dir(&run_dir)
                .expect("write resource summary");
            let output = run_dir
                .read_step_output(run_dir::files::RESOURCE_SUMMARY)
                .expect("resource summary json");

            assert_eq!(summary.label.as_deref(), Some("lint homeboy"));
            assert_eq!(output["label"], "lint homeboy");
            assert_eq!(output["pid"], std::process::id());
            assert!(output["duration_ms"].as_u64().is_some());
            assert_eq!(output["platform"], std::env::consts::OS);

            run_dir.cleanup();
        });
    }

    #[test]
    fn test_record_extension_child_resource() {
        with_isolated_home(|_| {
            let run_dir = RunDir::create().expect("run dir");
            let child = ExtensionChildResourceSummary {
                child: ChildProcessIdentity {
                    root_pid: 123,
                    command_label: "extension-runner".to_string(),
                },
                phase: Some("install".to_string()),
                started_at: "2026-04-30T00:00:00+00:00".to_string(),
                finished_at: "2026-04-30T00:00:00.100+00:00".to_string(),
                duration_ms: 100,
                peak: ChildResourcePeakSample {
                    sampled_peak_rss_bytes: Some(4096),
                    sampled_peak_cpu_percent: Some(1.5),
                },
                sampled_peak_at_ms: Some(50),
                sampled_peak_child_count: Some(0),
                samples: Vec::new(),
                warnings: vec!["probe_warning".to_string()],
            };

            record_extension_child_resource(run_dir.path(), &child).expect("record child resource");
            let resource_run = ResourceSummaryRun::start(Some("test fixture".to_string()));
            let summary = resource_run
                .write_to_run_dir(&run_dir)
                .expect("write resource summary");

            assert_eq!(summary.extension_children, vec![child]);
            assert_eq!(
                summary.extension_children[0].phase.as_deref(),
                Some("install")
            );
            let output = run_dir
                .read_step_output(run_dir::files::RESOURCE_SUMMARY)
                .expect("resource summary json");
            assert_eq!(
                output["extension_children"][0]["command_label"],
                "extension-runner"
            );
            assert_eq!(
                output["extension_children"][0]["sampled_peak_rss_bytes"],
                4096
            );

            run_dir.cleanup();
        });
    }
}
