//! Simulation provider: realistic-looking disks backed by sparse temp files.
//!
//! Used automatically when DBAN runs on a non-Linux host or without root,
//! and explicitly via `--demo`. Throughput is throttled per "disk" so the
//! progress UI behaves like real hardware instead of finishing instantly.

use std::fs::{self, OpenOptions};
use std::path::PathBuf;

use crate::device::{Bus, Disk, DiskProvider, LockReason, MediaKind};
use crate::CoreError;

const MIB: u64 = 1024 * 1024;

struct DemoSpec {
    name: &'static str,
    model: &'static str,
    serial: &'static str,
    size: u64,
    bus: Bus,
    kind: MediaKind,
    removable: bool,
    lock: Option<LockReason>,
    /// Simulated sustained throughput, bytes/sec.
    throttle: u64,
}

const SPECS: [DemoSpec; 5] = [
    DemoSpec {
        name: "sda",
        model: "Seagate Barracuda ST2000 (sim)",
        serial: "Z4Z0DEMO",
        size: 256 * MIB,
        bus: Bus::Sata,
        kind: MediaKind::Hdd,
        removable: false,
        lock: None,
        throttle: 14 * MIB,
    },
    DemoSpec {
        name: "sdb",
        model: "Samsung SSD 870 EVO (sim)",
        serial: "S62ADEMO",
        size: 256 * MIB,
        bus: Bus::Sata,
        kind: MediaKind::Ssd,
        removable: false,
        lock: None,
        throttle: 52 * MIB,
    },
    DemoSpec {
        name: "nvme0n1",
        model: "Samsung SSD 980 PRO (sim)",
        serial: "S5GXDEMO",
        size: 384 * MIB,
        bus: Bus::Nvme,
        kind: MediaKind::Ssd,
        removable: false,
        lock: None,
        throttle: 110 * MIB,
    },
    DemoSpec {
        name: "sdc",
        model: "SanDisk Ultra USB 3.0 (sim)",
        serial: "4C53DEMO",
        size: 128 * MIB,
        bus: Bus::Usb,
        kind: MediaKind::Ssd,
        removable: true,
        lock: None,
        throttle: 9 * MIB,
    },
    DemoSpec {
        name: "sdd",
        model: "DBAN boot medium (sim)",
        serial: "BOOTDEMO",
        size: 64 * MIB,
        bus: Bus::Usb,
        kind: MediaKind::Ssd,
        removable: true,
        lock: Some(LockReason::BootMedium),
        throttle: 9 * MIB,
    },
];

/// A [`DiskProvider`] serving simulated disks backed by sparse temp files.
pub struct DemoProvider {
    dir: PathBuf,
}

impl DemoProvider {
    /// Create the provider, ensuring its backing directory under the system
    /// temp dir exists.
    pub fn new() -> Result<Self, CoreError> {
        let dir = std::env::temp_dir().join("dban-demo");
        fs::create_dir_all(&dir)?;
        Ok(DemoProvider { dir })
    }

    /// Where the backing files live (useful for inspection and cleanup).
    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }
}

impl DiskProvider for DemoProvider {
    fn refresh(&mut self) -> Result<Vec<Disk>, CoreError> {
        let mut disks = Vec::with_capacity(SPECS.len());
        for spec in &SPECS {
            let path = self.dir.join(format!("{}.img", spec.name));
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&path)?;
            // Sparse on every mainstream filesystem; bytes appear as the
            // engine writes them.
            file.set_len(spec.size)?;
            disks.push(Disk {
                path,
                name: spec.name.to_string(),
                model: spec.model.to_string(),
                serial: spec.serial.to_string(),
                size_bytes: spec.size,
                bus: spec.bus,
                kind: spec.kind,
                removable: spec.removable,
                lock: spec.lock,
                simulated: true,
                throttle_bps: Some(spec.throttle),
            });
        }
        Ok(disks)
    }

    fn is_simulation(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_disks_enumerate() {
        let mut p = DemoProvider::new().unwrap();
        let disks = p.refresh().unwrap();
        assert_eq!(disks.len(), 5);
        assert!(p.is_simulation());
        // Exactly one disk simulates the locked boot medium.
        assert_eq!(disks.iter().filter(|d| d.is_locked()).count(), 1);
        // Backing files exist with the advertised size.
        for d in &disks {
            let meta = std::fs::metadata(&d.path).unwrap();
            assert_eq!(meta.len(), d.size_bytes);
            assert!(d.throttle_bps.is_some());
        }
    }
}
