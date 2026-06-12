//! Host hardware summary shown in the UI header and embedded in reports.

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
pub struct SystemInfo {
    pub cpu_model: String,
    pub cpu_cores: usize,
    pub mem_total_bytes: u64,
    pub kernel: String,
}

impl SystemInfo {
    pub fn mem_human(&self) -> String {
        if self.mem_total_bytes == 0 {
            "—".to_string()
        } else {
            crate::device::human_bytes(self.mem_total_bytes)
        }
    }
}

pub fn collect() -> SystemInfo {
    let cpu_cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    #[cfg(target_os = "linux")]
    {
        SystemInfo {
            cpu_model: linux_cpu_model().unwrap_or_else(|| "unknown CPU".to_string()),
            cpu_cores,
            mem_total_bytes: linux_mem_total().unwrap_or(0),
            kernel: linux_kernel().unwrap_or_else(|| "linux".to_string()),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        SystemInfo {
            cpu_model: format!("{} host", std::env::consts::OS),
            cpu_cores,
            mem_total_bytes: 0,
            kernel: format!("{} (simulation host)", std::env::consts::OS),
        }
    }
}

#[cfg(target_os = "linux")]
fn linux_cpu_model() -> Option<String> {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in cpuinfo.lines() {
        // x86: "model name"; many ARM SoCs: "Hardware" / "Processor".
        if line.starts_with("model name") || line.starts_with("Processor") {
            return line.split(':').nth(1).map(|s| s.trim().to_string());
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn linux_mem_total() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kib: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
            return Some(kib * 1024);
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn linux_kernel() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| format!("Linux {}", s.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_never_panics() {
        let info = collect();
        assert!(info.cpu_cores >= 1);
        assert!(!info.kernel.is_empty());
        assert!(!info.cpu_model.is_empty());
    }
}
