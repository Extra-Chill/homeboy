//! types — extracted from health.rs.

use serde::Serialize;
use super::super::SshClient;
use crate::project::Project;


#[derive(Debug, Default, Serialize, Clone)]
pub struct ServerHealth {
    /// Server uptime as a human-readable string (e.g. "10 days")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime: Option<String>,
    /// Load averages: 1min, 5min, 15min
    #[serde(skip_serializing_if = "Option::is_none")]
    pub load: Option<LoadAverage>,
    /// Disk usage for the primary filesystem
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk: Option<DiskUsage>,
    /// Memory usage
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryUsage>,
    /// Service statuses (only if project has services configured)
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceStatus>,
    /// Warning messages for thresholds exceeded
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
    /// Number of CPU cores (for contextualizing load)
    pub cpu_cores: u32,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct DiskUsage {
    /// Used space in human-readable form (e.g. "36G")
    pub used: String,
    /// Total space in human-readable form (e.g. "150G")
    pub total: String,
    /// Usage percentage (0-100)
    pub percent: u32,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct MemoryUsage {
    /// Used memory in human-readable form (e.g. "2.2G")
    pub used: String,
    /// Total memory in human-readable form (e.g. "7.6G")
    pub total: String,
    /// Usage percentage (0-100)
    pub percent: u32,
}

#[derive(Debug, Default, Serialize, Clone)]
pub struct ServiceStatus {
    pub name: String,
    pub active: bool,
    /// Raw status string from systemctl (e.g. "active", "inactive", "failed")
    pub status: String,
}
