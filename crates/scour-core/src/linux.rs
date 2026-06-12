//! Real-hardware disk discovery via sysfs (Linux only).
//!
//! Every disk gets a conservative lock analysis before it is ever shown as
//! wipeable:
//! * any mounted partition (or the whole-disk device itself) → `Mounted`
//! * any active swap partition → `SwapActive`
//! * kernel holders (dm/RAID/LVM members) → `InUse`
//! * an ISO9660 signature whose volume label starts with `SCOUR` → `BootMedium`
//!   (that's the stick the live system booted from)

use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::device::{Bus, Disk, DiskProvider, LockReason, MediaKind};
use crate::CoreError;

/// Volume label prefix stamped onto the live ISO by `iso/build.sh`.
pub const BOOT_LABEL_PREFIX: &str = "SCOUR";

pub struct SysfsProvider {
    root: PathBuf,
}

impl SysfsProvider {
    pub fn new() -> Self {
        SysfsProvider {
            root: PathBuf::from("/sys/block"),
        }
    }
}

impl Default for SysfsProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl DiskProvider for SysfsProvider {
    fn refresh(&mut self) -> Result<Vec<Disk>, CoreError> {
        let mounted = mounted_device_paths();
        let swaps = swap_device_paths();
        let mut disks = Vec::new();

        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if should_skip(&name) {
                continue;
            }
            let size_bytes = read_u64(&sys_path(&name, "size")).unwrap_or(0) * 512;
            if size_bytes == 0 {
                continue; // empty card readers, absent media
            }
            let dev_path = PathBuf::from(format!("/dev/{name}"));
            let rotational = read_u64(&sys_path(&name, "queue/rotational")).unwrap_or(0) == 1;

            disks.push(Disk {
                model: read_string(&sys_path(&name, "device/model"))
                    .unwrap_or_else(|| "unknown model".to_string()),
                serial: read_string(&sys_path(&name, "device/serial"))
                    .or_else(|| read_string(&sys_path(&name, "device/wwid")))
                    .unwrap_or_else(|| "—".to_string()),
                bus: detect_bus(&name),
                kind: if rotational {
                    MediaKind::Hdd
                } else {
                    MediaKind::Ssd
                },
                removable: read_u64(&sys_path(&name, "removable")).unwrap_or(0) == 1,
                lock: detect_lock(&name, &dev_path, &mounted, &swaps),
                size_bytes,
                name,
                path: dev_path,
                throttle_bps: None,
            });
        }
        disks.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(disks)
    }

    fn is_simulation(&self) -> bool {
        false
    }
}

fn sys_path(name: &str, leaf: &str) -> PathBuf {
    PathBuf::from(format!("/sys/block/{name}/{leaf}"))
}

/// Pseudo/stacked devices that must never appear in the wipe list.
fn should_skip(name: &str) -> bool {
    const SKIP: [&str; 8] = ["loop", "ram", "zram", "dm-", "md", "sr", "fd", "nbd"];
    SKIP.iter().any(|p| name.starts_with(p))
}

fn read_string(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn read_u64(path: &Path) -> Option<u64> {
    read_string(path)?.parse().ok()
}

fn detect_bus(name: &str) -> Bus {
    if name.starts_with("nvme") {
        return Bus::Nvme;
    }
    if name.starts_with("mmcblk") {
        return Bus::Mmc;
    }
    if name.starts_with("vd") {
        return Bus::Virtio;
    }
    // The sysfs symlink walks the full device topology.
    if let Ok(link) = fs::read_link(format!("/sys/block/{name}")) {
        let link = link.to_string_lossy();
        if link.contains("/usb") {
            return Bus::Usb;
        }
        if link.contains("/virtio") {
            return Bus::Virtio;
        }
        if link.contains("/ata") {
            return Bus::Sata;
        }
    }
    Bus::Unknown
}

/// Device paths (e.g. `/dev/sda1`) currently mounted.
fn mounted_device_paths() -> HashSet<String> {
    let mut set = HashSet::new();
    if let Ok(mounts) = fs::read_to_string("/proc/mounts") {
        for line in mounts.lines() {
            if let Some(dev) = line.split_whitespace().next() {
                if dev.starts_with("/dev/") {
                    set.insert(dev.to_string());
                }
            }
        }
    }
    set
}

/// Device paths in use as swap.
fn swap_device_paths() -> HashSet<String> {
    let mut set = HashSet::new();
    if let Ok(swaps) = fs::read_to_string("/proc/swaps") {
        for line in swaps.lines().skip(1) {
            if let Some(dev) = line.split_whitespace().next() {
                if dev.starts_with("/dev/") {
                    set.insert(dev.to_string());
                }
            }
        }
    }
    set
}

/// True when `dev` (e.g. `/dev/sda1`, `/dev/nvme0n1p2`) is the disk `base`
/// (`/dev/sda`) or one of its partitions. Careful about `/dev/sda` vs
/// `/dev/sdaa`: the remainder must be a partition suffix, i.e. `p?[0-9]+`.
fn belongs_to(dev: &str, base: &str) -> bool {
    if dev == base {
        return true;
    }
    let Some(rest) = dev.strip_prefix(base) else {
        return false;
    };
    // When the base name ends in a digit (nvme0n1, mmcblk0) the kernel always
    // inserts a 'p' before the partition number — otherwise /dev/nvme0n10
    // (an eleventh namespace) would masquerade as a partition of nvme0n1.
    let base_ends_in_digit = base.bytes().last().is_some_and(|b| b.is_ascii_digit());
    let digits = if base_ends_in_digit {
        match rest.strip_prefix('p') {
            Some(d) => d,
            None => return false,
        }
    } else {
        rest
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

fn detect_lock(
    name: &str,
    dev_path: &Path,
    mounted: &HashSet<String>,
    swaps: &HashSet<String>,
) -> Option<LockReason> {
    let base = dev_path.to_string_lossy();
    if mounted.iter().any(|m| belongs_to(m, &base)) {
        return Some(LockReason::Mounted);
    }
    if swaps.iter().any(|s| belongs_to(s, &base)) {
        return Some(LockReason::SwapActive);
    }
    // Kernel holders: device-mapper, MD-RAID members, ...
    if let Ok(mut holders) = fs::read_dir(format!("/sys/block/{name}/holders")) {
        if holders.next().is_some() {
            return Some(LockReason::InUse);
        }
    }
    if is_scour_boot_medium(dev_path) {
        return Some(LockReason::BootMedium);
    }
    None
}

/// Detect the live boot stick: a hybrid ISO carries the ISO9660 Primary
/// Volume Descriptor at byte 32768 of the *device*; its volume id sits at
/// offset 40 within the descriptor.
fn is_scour_boot_medium(dev_path: &Path) -> bool {
    let Ok(mut f) = fs::File::open(dev_path) else {
        return false;
    };
    if f.seek(SeekFrom::Start(32768)).is_err() {
        return false;
    }
    let mut descriptor = [0u8; 2048];
    if f.read_exact(&mut descriptor).is_err() {
        return false;
    }
    // Type 1 descriptor with the "CD001" standard identifier.
    if descriptor[0] != 1 || &descriptor[1..6] != b"CD001" {
        return false;
    }
    let label = String::from_utf8_lossy(&descriptor[40..72]);
    label.trim_start().starts_with(BOOT_LABEL_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partition_ownership() {
        assert!(belongs_to("/dev/sda", "/dev/sda"));
        assert!(belongs_to("/dev/sda1", "/dev/sda"));
        assert!(belongs_to("/dev/sda12", "/dev/sda"));
        assert!(belongs_to("/dev/nvme0n1p2", "/dev/nvme0n1"));
        assert!(belongs_to("/dev/mmcblk0p1", "/dev/mmcblk0"));
        // The classic prefix trap:
        assert!(!belongs_to("/dev/sdaa", "/dev/sda"));
        assert!(!belongs_to("/dev/sdaa1", "/dev/sda"));
        assert!(!belongs_to("/dev/sdb1", "/dev/sda"));
        assert!(!belongs_to("/dev/nvme0n10", "/dev/nvme0n1"));
    }

    #[test]
    fn pseudo_devices_are_skipped() {
        for name in [
            "loop0", "ram0", "zram0", "dm-0", "md127", "sr0", "fd0", "nbd3",
        ] {
            assert!(should_skip(name), "{name} must be skipped");
        }
        for name in ["sda", "sdb", "nvme0n1", "vda", "mmcblk0"] {
            assert!(!should_skip(name), "{name} must not be skipped");
        }
    }

    #[test]
    fn bus_detection_by_name() {
        assert_eq!(detect_bus("nvme0n1"), Bus::Nvme);
        assert_eq!(detect_bus("mmcblk0"), Bus::Mmc);
        assert_eq!(detect_bus("vda"), Bus::Virtio);
    }
}
