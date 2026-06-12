//! Disk model and the provider abstraction.
//!
//! Real hardware is enumerated by the `linux` module (sysfs; Linux-only). The
//! [`demo`](crate::demo) provider serves simulated disks backed by temp files
//! so the full UI and engine can run anywhere — including CI — without touching
//! real storage.

use std::path::PathBuf;

use serde::Serialize;

use crate::CoreError;

/// The transport a disk is attached by. Used for display and to route
/// firmware-erase commands (ATA vs. NVMe).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Bus {
    /// Serial ATA.
    Sata,
    /// NVM Express.
    Nvme,
    /// USB mass storage.
    Usb,
    /// VirtIO block (virtual machines).
    Virtio,
    /// SD/MMC card.
    Mmc,
    /// Simulated demo disk.
    Demo,
    /// Bus could not be determined.
    Unknown,
}

impl Bus {
    /// Short display label, e.g. `"SATA"`.
    pub fn label(&self) -> &'static str {
        match self {
            Bus::Sata => "SATA",
            Bus::Nvme => "NVMe",
            Bus::Usb => "USB",
            Bus::Virtio => "VirtIO",
            Bus::Mmc => "MMC",
            Bus::Demo => "DEMO",
            Bus::Unknown => "?",
        }
    }
}

/// Physical media type, which drives the SSD over-provisioning advisory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum MediaKind {
    /// Rotational hard disk.
    Hdd,
    /// Solid-state / flash.
    Ssd,
}

impl MediaKind {
    /// Short display label, e.g. `"SSD"`.
    pub fn label(&self) -> &'static str {
        match self {
            MediaKind::Hdd => "HDD",
            MediaKind::Ssd => "SSD",
        }
    }
}

/// Why a disk can never be selected for wiping in this session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum LockReason {
    /// The disk (or one of its partitions) is mounted.
    Mounted,
    /// A partition is in use as swap.
    SwapActive,
    /// The disk appears to hold the running DBAN boot medium.
    BootMedium,
    /// Another kernel subsystem holds the device (RAID member, dm, ...).
    InUse,
}

impl LockReason {
    /// Short display label, e.g. `"mounted"`.
    pub fn label(&self) -> &'static str {
        match self {
            LockReason::Mounted => "mounted",
            LockReason::SwapActive => "active swap",
            LockReason::BootMedium => "boot medium",
            LockReason::InUse => "in use",
        }
    }
}

/// A storage device, real or simulated, as presented to the UI and engine.
#[derive(Clone, Debug, Serialize)]
pub struct Disk {
    /// Path the engine opens for writing (`/dev/sda`, or a temp file in demo mode).
    pub path: PathBuf,
    /// Kernel name (`sda`, `nvme0n1`) or demo identifier.
    pub name: String,
    /// Model string reported by the device.
    pub model: String,
    /// Serial number (or WWID) reported by the device.
    pub serial: String,
    /// Capacity in bytes.
    pub size_bytes: u64,
    /// Transport the disk is attached by.
    pub bus: Bus,
    /// Rotational vs. solid-state.
    pub kind: MediaKind,
    /// Whether the device is removable (USB stick, card).
    pub removable: bool,
    /// `Some(reason)` makes the disk permanently unselectable this session.
    pub lock: Option<LockReason>,
    /// True for a simulated (temp-file-backed) disk. The engine and firmware
    /// modules use this — never the bus label — to decide between real device
    /// I/O / ioctl and simulation, since demo disks carry realistic bus labels.
    pub simulated: bool,
    /// Demo-mode rate limit in bytes/sec, so simulated wipes progress at a
    /// believable, watchable pace. Always `None` for real hardware.
    #[serde(skip)]
    pub throttle_bps: Option<u64>,
}

impl Disk {
    /// True when the disk is locked and can never be selected this session.
    pub fn is_locked(&self) -> bool {
        self.lock.is_some()
    }

    /// Capacity formatted in vendor-style decimal units (e.g. `"500 GB"`).
    pub fn size_human(&self) -> String {
        human_bytes(self.size_bytes)
    }
}

/// Vendor-style decimal units (a "500 GB" drive shows as 500 GB).
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{value:.0} {}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

/// Source of disks. Implementations: sysfs (Linux) and demo (anywhere).
pub trait DiskProvider: Send {
    /// Re-enumerate attached disks.
    fn refresh(&mut self) -> Result<Vec<Disk>, CoreError>;
    /// True when the disks are simulated files, not hardware.
    fn is_simulation(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_formatting() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(1000), "1.00 KB");
        assert_eq!(human_bytes(500_107_862_016), "500 GB");
        assert_eq!(human_bytes(4_000_787_030_016), "4.00 TB");
        assert_eq!(human_bytes(64 * 1024 * 1024), "67.1 MB");
    }
}
