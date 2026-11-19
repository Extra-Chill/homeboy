//! Without Linux cgroup v2 a job never has its own cgroup, so runner jobs keep
//! the sampled process-tree guard and step jobs keep their process-tree stop.

use std::io;
use std::path::Path;
use std::process::Command;

pub fn initialize(_jobs_memory_max: u64) -> Result<&'static Path, String> {
    Err("job cgroups require Linux cgroup v2".to_string())
}

pub fn jobs_dir() -> Option<&'static Path> {
    None
}

pub struct JobCgroup;

impl JobCgroup {
    pub fn create(
        _job_id: Option<&str>,
        _process_count_limit: u64,
        _concurrency: u64,
    ) -> io::Result<Option<Self>> {
        Ok(None)
    }

    pub fn create_unlimited(_job_id: &str) -> io::Result<Option<Self>> {
        Ok(None)
    }

    pub fn of_process(_pid: u32) -> io::Result<Option<Self>> {
        Ok(None)
    }

    pub fn join_before_exec(&self, _command: &mut Command) {}

    pub fn kill(&self) {}

    pub fn stop(&self) -> io::Result<()> {
        Ok(())
    }
}
