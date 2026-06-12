//! Erasure reports — the auditable artifact of a wipe session.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::engine::JobReport;
use crate::sysinfo::SystemInfo;

/// The auditable record of a whole wipe session: tool, host, and every job.
#[derive(Clone, Debug, Serialize)]
pub struct SessionReport {
    /// Tool name (`"scour"`).
    pub tool: String,
    /// Tool version string.
    pub tool_version: String,
    /// Unix timestamp the report was created.
    pub created_unix: u64,
    /// True when the session ran against simulated disks.
    pub simulation: bool,
    /// Host hardware summary.
    pub host: SystemInfo,
    /// Per-disk job records.
    pub jobs: Vec<JobReport>,
}

impl SessionReport {
    /// Assemble a session report from the host info and finished jobs.
    pub fn new(host: SystemInfo, simulation: bool, jobs: Vec<JobReport>) -> Self {
        SessionReport {
            tool: "scour".to_string(),
            tool_version: crate::VERSION.to_string(),
            created_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            simulation,
            host,
            jobs,
        }
    }

    /// Serialize the report as pretty-printed JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("report serialization cannot fail")
    }

    /// Write `scour-report-<unix-ts>.json` into `dir`, returning the full path.
    pub fn save_json(&self, dir: &Path) -> io::Result<PathBuf> {
        let path = dir.join(format!("scour-report-{}.json", self.created_unix));
        std::fs::write(&path, self.to_json())?;
        Ok(path)
    }

    /// True when every job in the session completed successfully.
    pub fn all_succeeded(&self) -> bool {
        self.jobs
            .iter()
            .all(|j| j.status == crate::engine::JobStatus::Success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algorithm::VerifyMode;
    use crate::engine::JobStatus;

    fn dummy_job(status: JobStatus) -> JobReport {
        JobReport {
            disk_name: "sda".into(),
            disk_model: "Test".into(),
            disk_serial: "S1".into(),
            disk_size_bytes: 1024,
            method_id: "nist-clear".into(),
            method_name: "NIST 800-88 Clear".into(),
            firmware: false,
            rounds: 1,
            verify: VerifyMode::LastPass,
            pass_count: 1,
            passes_completed: 1,
            status,
            error: None,
            started_unix: 1,
            finished_unix: 2,
            duration_secs: 1.0,
            bytes_written: 1024,
            bytes_verified: 1024,
            avg_write_mib_s: 1.0,
        }
    }

    #[test]
    fn json_round_trip_structure() {
        let report = SessionReport::new(
            crate::sysinfo::collect(),
            true,
            vec![dummy_job(JobStatus::Success)],
        );
        let json = report.to_json();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["tool"], "scour");
        assert_eq!(value["simulation"], true);
        assert_eq!(value["jobs"][0]["status"], "Success");
        assert_eq!(value["jobs"][0]["disk_name"], "sda");
    }

    #[test]
    fn save_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let report = SessionReport::new(crate::sysinfo::collect(), true, vec![]);
        let path = report.save_json(dir.path()).unwrap();
        assert!(path.exists());
        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("scour"));
    }

    #[test]
    fn success_aggregation() {
        let host = crate::sysinfo::collect();
        let ok = SessionReport::new(host.clone(), true, vec![dummy_job(JobStatus::Success)]);
        assert!(ok.all_succeeded());
        let bad = SessionReport::new(host, true, vec![dummy_job(JobStatus::VerifyFailed)]);
        assert!(!bad.all_succeeded());
    }
}
