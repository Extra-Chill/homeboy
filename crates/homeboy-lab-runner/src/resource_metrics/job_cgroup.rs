//! Runner jobs run in the shared kernel-enforced job cgroup primitive,
//! `homeboy_core::job_cgroup` (#18418, #18449).

pub(crate) use homeboy_core::job_cgroup::*;

#[cfg(all(test, target_os = "linux"))]
#[path = "job_cgroup_tests.rs"]
mod tests;
