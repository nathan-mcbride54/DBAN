//! Firmware-based sanitization: ATA Security Erase and NVMe Format / Sanitize.
//!
//! Why this exists: overwriting (see [`crate::engine`]) writes through the
//! filesystem/block layer and cannot reach sectors the drive has remapped or,
//! on flash, the over-provisioned cells hidden behind the FTL. The drive's own
//! firmware *can* reach them. NIST SP 800-88 calls this a **Purge**, and for
//! SSDs it is the only software-driven method that truly sanitizes the media:
//!
//! * **ATA Security Erase** — `SECURITY ERASE UNIT`. The drive erases every
//!   user-addressable and reallocated sector internally. The *enhanced*
//!   variant also overwrites with a vendor pattern and, on SEDs, changes the
//!   media-encryption key.
//! * **NVMe Format NVM** — with the *Secure Erase Settings* field set to user
//!   erase (1) or cryptographic erase (2).
//! * **NVMe Sanitize** — `block erase` or `crypto erase` sanitize actions, the
//!   strongest NVMe purge, covering the entire NVM subsystem.
//!
//! Capability detection issues only **non-destructive** identify commands, so
//! it is always safe to run. The erase commands themselves are gated, like
//! every destructive path in DBAN, behind [`crate::safety::ArmToken`].
//!
//! The real commands are issued on Linux via SCSI/NVMe ioctl pass-through
//! (`SG_IO` ATA PASS-THROUGH, `NVME_IOCTL_ADMIN_CMD`). On other platforms, or
//! for simulated demo disks, [`execute`] performs a believable simulation that
//! zeroes the backing file so the full flow can be exercised without hardware.

use std::sync::atomic::AtomicBool;

use serde::Serialize;

use crate::device::{Bus, Disk};

/// A firmware-level sanitization command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum FirmwareMethod {
    /// ATA `SECURITY ERASE UNIT`.
    AtaSecureErase {
        /// Select the enhanced erase (vendor pattern + key rotation on SEDs).
        enhanced: bool,
    },
    /// NVMe `Format NVM`.
    NvmeFormat {
        /// Cryptographic erase (SES=2) rather than user-data erase (SES=1).
        crypto: bool,
    },
    /// NVMe `Sanitize`.
    NvmeSanitize {
        /// Crypto-erase rather than block-erase.
        crypto: bool,
    },
    /// TCG Opal cryptographic erase: revert a self-encrypting drive to factory
    /// state, destroying the media-encryption key. Bus-agnostic (SATA or NVMe).
    TcgRevert,
}

impl FirmwareMethod {
    /// Stable machine id, used in erasure reports.
    pub fn id(&self) -> &'static str {
        match self {
            FirmwareMethod::AtaSecureErase { enhanced: false } => "ata-secure-erase",
            FirmwareMethod::AtaSecureErase { enhanced: true } => "ata-enhanced-erase",
            FirmwareMethod::NvmeFormat { crypto: false } => "nvme-format-user",
            FirmwareMethod::NvmeFormat { crypto: true } => "nvme-format-crypto",
            FirmwareMethod::NvmeSanitize { crypto: false } => "nvme-sanitize-block",
            FirmwareMethod::NvmeSanitize { crypto: true } => "nvme-sanitize-crypto",
            FirmwareMethod::TcgRevert => "tcg-opal-revert",
        }
    }

    /// Human-readable method name.
    pub fn name(&self) -> &'static str {
        match self {
            FirmwareMethod::AtaSecureErase { enhanced: false } => "ATA Secure Erase",
            FirmwareMethod::AtaSecureErase { enhanced: true } => "ATA Enhanced Secure Erase",
            FirmwareMethod::NvmeFormat { crypto: false } => "NVMe Format (user)",
            FirmwareMethod::NvmeFormat { crypto: true } => "NVMe Format (crypto)",
            FirmwareMethod::NvmeSanitize { crypto: false } => "NVMe Sanitize (block)",
            FirmwareMethod::NvmeSanitize { crypto: true } => "NVMe Sanitize (crypto)",
            FirmwareMethod::TcgRevert => "TCG Opal Crypto-Erase",
        }
    }

    /// One-paragraph description shown in the UI.
    pub fn description(&self) -> &'static str {
        match self {
            FirmwareMethod::AtaSecureErase { enhanced: false } => {
                "Drive-internal erase of all user and reallocated sectors via the \
                 ATA security feature set. A true purge for SATA media."
            }
            FirmwareMethod::AtaSecureErase { enhanced: true } => {
                "Enhanced ATA security erase: overwrites with a vendor pattern and, \
                 on self-encrypting drives, rotates the media key."
            }
            FirmwareMethod::NvmeFormat { crypto: false } => {
                "NVMe Format with user-data secure erase (SES=1): the controller \
                 erases the namespace, including over-provisioned flash."
            }
            FirmwareMethod::NvmeFormat { crypto: true } => {
                "NVMe Format with cryptographic erase (SES=2): the controller \
                 discards the media encryption key — instant, irreversible purge."
            }
            FirmwareMethod::NvmeSanitize { crypto: false } => {
                "NVMe Sanitize block-erase: purges the entire NVM subsystem, the \
                 strongest NVMe sanitization."
            }
            FirmwareMethod::NvmeSanitize { crypto: true } => {
                "NVMe Sanitize crypto-erase: destroys the media key across the whole \
                 NVM subsystem."
            }
            FirmwareMethod::TcgRevert => {
                "Revert a TCG Opal self-encrypting drive to factory state, which \
                 cryptographically erases all data by destroying the media key. \
                 Works on SATA and NVMe SEDs."
            }
        }
    }

    /// Which bus this command belongs to, or `Unknown` for bus-agnostic ones.
    pub fn bus(&self) -> Bus {
        match self {
            FirmwareMethod::AtaSecureErase { .. } => Bus::Sata,
            FirmwareMethod::NvmeFormat { .. } | FirmwareMethod::NvmeSanitize { .. } => Bus::Nvme,
            FirmwareMethod::TcgRevert => Bus::Unknown,
        }
    }
}

/// What firmware sanitization a specific disk advertises support for.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct FirmwareSupport {
    /// ATA security feature set (normal erase) supported.
    pub ata_secure_erase: bool,
    /// ATA enhanced security erase supported.
    pub ata_enhanced_erase: bool,
    /// Security is *frozen*: the BIOS locked the feature set and an erase will
    /// be refused until the drive is power-cycled (hot-swap / sleep). Surfaced
    /// so the UI can explain why the option is unavailable.
    pub ata_frozen: bool,
    /// NVMe Format with user-data erase supported.
    pub nvme_format_user: bool,
    /// NVMe Format with cryptographic erase supported.
    pub nvme_format_crypto: bool,
    /// NVMe Sanitize block-erase supported.
    pub nvme_sanitize_block: bool,
    /// NVMe Sanitize crypto-erase supported.
    pub nvme_sanitize_crypto: bool,
    /// The drive is a TCG Opal self-encrypting drive (revert/crypto-erase
    /// available). Detected by TCG Level 0 Discovery.
    pub opal: bool,
}

impl FirmwareSupport {
    /// The concrete methods the operator may choose for this disk.
    pub fn methods(&self) -> Vec<FirmwareMethod> {
        let mut v = Vec::new();
        if self.ata_secure_erase && !self.ata_frozen {
            v.push(FirmwareMethod::AtaSecureErase { enhanced: false });
        }
        if self.ata_enhanced_erase && !self.ata_frozen {
            v.push(FirmwareMethod::AtaSecureErase { enhanced: true });
        }
        if self.nvme_format_user {
            v.push(FirmwareMethod::NvmeFormat { crypto: false });
        }
        if self.nvme_format_crypto {
            v.push(FirmwareMethod::NvmeFormat { crypto: true });
        }
        if self.nvme_sanitize_block {
            v.push(FirmwareMethod::NvmeSanitize { crypto: false });
        }
        if self.nvme_sanitize_crypto {
            v.push(FirmwareMethod::NvmeSanitize { crypto: true });
        }
        if self.opal {
            v.push(FirmwareMethod::TcgRevert);
        }
        v
    }

    /// True if `method` is among the supported methods.
    pub fn supports(&self, method: FirmwareMethod) -> bool {
        self.methods().contains(&method)
    }

    /// True if the disk supports any firmware sanitization at all.
    pub fn any(&self) -> bool {
        !self.methods().is_empty()
    }
}

/// Errors from firmware sanitization.
#[derive(Debug, thiserror::Error)]
pub enum FirmwareError {
    /// The disk does not support the requested firmware command.
    #[error("firmware command not supported by this disk")]
    Unsupported,
    /// ATA security is frozen and must be unlocked by a power cycle.
    #[error("drive security is frozen; power-cycle the drive and retry")]
    Frozen,
    /// The operation was cancelled.
    #[error("operation cancelled")]
    Cancelled,
    /// A device I/O / ioctl error occurred.
    #[error("device I/O error: {0}")]
    Io(String),
    /// The drive returned a non-success status code.
    #[error("drive reported command failure (status 0x{0:02x})")]
    DeviceStatus(u32),
}

/// Detect which firmware sanitization a disk supports. Non-destructive: issues
/// only identify/inquiry commands. Demo disks report a fixed capability matrix
/// derived from their simulated bus so the UI can be exercised end to end.
pub fn detect_support(disk: &Disk) -> FirmwareSupport {
    if disk.simulated {
        return demo_support(disk);
    }
    #[cfg(target_os = "linux")]
    {
        let mut support = match disk.bus {
            Bus::Sata => linux::detect_ata(&disk.path).unwrap_or_default(),
            Bus::Nvme => linux::detect_nvme(&disk.path).unwrap_or_default(),
            _ => FirmwareSupport::default(),
        };
        // TCG Opal is bus-agnostic; probe via Level 0 Discovery for SATA/NVMe.
        if matches!(disk.bus, Bus::Sata | Bus::Nvme) {
            support.opal = linux::detect_opal(&disk.path, disk.bus).unwrap_or(false);
        }
        support
    }
    #[cfg(not(target_os = "linux"))]
    {
        FirmwareSupport::default()
    }
}

/// Hidden-sector analysis for an ATA disk: the Host Protected Area (HPA) and
/// Device Configuration Overlay (DCO) can shrink the user-addressable range so
/// that an overwrite misses the top of the disk. All counts are in logical
/// sectors.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct HiddenAreas {
    /// Sectors the OS can currently address.
    pub current_sectors: u64,
    /// Sectors addressable after removing the HPA (READ NATIVE MAX).
    pub native_sectors: u64,
    /// Sectors addressable after also removing the DCO (the true capacity).
    pub real_sectors: u64,
    /// Logical sector size in bytes.
    pub sector_size: u32,
}

impl HiddenAreas {
    /// No hidden areas: every count equals the current addressable size.
    pub fn none(current_sectors: u64, sector_size: u32) -> Self {
        HiddenAreas {
            current_sectors,
            native_sectors: current_sectors,
            real_sectors: current_sectors,
            sector_size: sector_size.max(512),
        }
    }

    /// A Host Protected Area is hiding sectors.
    pub fn has_hpa(&self) -> bool {
        self.native_sectors > self.current_sectors
    }

    /// A Device Configuration Overlay is hiding sectors.
    pub fn has_dco(&self) -> bool {
        self.real_sectors > self.native_sectors
    }

    /// Any sectors are hidden from the current addressable range.
    pub fn any(&self) -> bool {
        self.real_sectors > self.current_sectors
    }

    /// Hidden sectors (HPA + DCO combined).
    pub fn hidden_sectors(&self) -> u64 {
        self.real_sectors.saturating_sub(self.current_sectors)
    }

    /// Hidden capacity in bytes.
    pub fn hidden_bytes(&self) -> u64 {
        self.hidden_sectors() * self.sector_size as u64
    }

    /// True full capacity in bytes once everything is revealed.
    pub fn full_bytes(&self) -> u64 {
        self.real_sectors * self.sector_size as u64
    }
}

/// Detect HPA/DCO hidden areas. Non-destructive. For simulated disks the
/// "hidden" extent is the difference between the backing file's real length and
/// the advertised size, so the feature can be exercised end to end.
pub fn detect_hidden_areas(disk: &Disk) -> HiddenAreas {
    let sector = 512u32;
    if disk.simulated {
        let real = std::fs::metadata(&disk.path)
            .map(|m| m.len())
            .unwrap_or(disk.size_bytes);
        let current = disk.size_bytes;
        if real > current {
            return HiddenAreas {
                current_sectors: current / sector as u64,
                native_sectors: real / sector as u64, // modelled entirely as HPA
                real_sectors: real / sector as u64,
                sector_size: sector,
            };
        }
        return HiddenAreas::none(current / sector as u64, sector);
    }
    #[cfg(target_os = "linux")]
    {
        if disk.bus == Bus::Sata {
            if let Ok(h) = linux::detect_hidden_ata(&disk.path, disk.size_bytes) {
                return h;
            }
        }
    }
    HiddenAreas::none(disk.size_bytes / sector as u64, sector)
}

/// Remove HPA and DCO so the whole disk becomes addressable. Returns the new
/// full capacity in bytes. For simulated disks this simply reports the backing
/// file's real length (the file is already that big, so the engine can wipe it
/// all).
pub fn reveal_hidden_areas(disk: &Disk) -> Result<u64, FirmwareError> {
    let h = detect_hidden_areas(disk);
    if !h.any() {
        return Ok(disk.size_bytes);
    }
    if disk.simulated {
        return Ok(h.full_bytes());
    }
    #[cfg(target_os = "linux")]
    {
        if disk.bus == Bus::Sata {
            return linux::reveal_hidden_ata(&disk.path);
        }
    }
    Err(FirmwareError::Unsupported)
}

/// Synthesize plausible capabilities for the simulated demo disks so every
/// firmware method appears at least once in the picker.
fn demo_support(disk: &Disk) -> FirmwareSupport {
    match disk.bus {
        Bus::Sata => FirmwareSupport {
            ata_secure_erase: true,
            ata_enhanced_erase: true,
            ata_frozen: false,
            opal: disk.kind == crate::device::MediaKind::Ssd, // model SATA SSDs as SEDs
            ..Default::default()
        },
        Bus::Nvme => FirmwareSupport {
            nvme_format_user: true,
            nvme_format_crypto: true,
            nvme_sanitize_block: true,
            nvme_sanitize_crypto: true,
            opal: true,
            ..Default::default()
        },
        _ => FirmwareSupport::default(),
    }
}

/// Issue a firmware sanitization command and block until it completes.
///
/// `on_progress` receives a 0.0..=1.0 fraction for the simulated path; real
/// hardware commands are indeterminate (the drive does not report progress for
/// ATA erase or NVMe Format), so it is called once with 0.0 and the engine
/// shows an indeterminate indicator.
pub fn execute(
    disk: &Disk,
    method: FirmwareMethod,
    cancel: &AtomicBool,
    on_progress: impl FnMut(f64),
) -> Result<(), FirmwareError> {
    if disk.simulated {
        return simulate(disk, cancel, on_progress);
    }
    #[cfg(target_os = "linux")]
    {
        let mut on_progress = on_progress;
        on_progress(0.0);
        match method {
            FirmwareMethod::AtaSecureErase { enhanced } => {
                linux::ata_secure_erase(&disk.path, enhanced)
            }
            FirmwareMethod::NvmeFormat { crypto } => linux::nvme_format(&disk.path, crypto),
            FirmwareMethod::NvmeSanitize { crypto } => {
                linux::nvme_sanitize(&disk.path, crypto, cancel)
            }
            FirmwareMethod::TcgRevert => linux::tcg_revert(&disk.path, disk.bus),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (method, cancel, on_progress);
        Err(FirmwareError::Unsupported)
    }
}

/// Simulated firmware erase for demo disks: zero the backing file over a short,
/// watchable interval, honoring cancellation.
fn simulate(
    disk: &Disk,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(f64),
) -> Result<(), FirmwareError> {
    use std::io::{Seek, SeekFrom, Write};
    use std::sync::atomic::Ordering;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&disk.path)
        .map_err(|e| FirmwareError::Io(e.to_string()))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|e| FirmwareError::Io(e.to_string()))?;

    let size = disk.size_bytes;
    let chunk = 1usize << 20; // 1 MiB
    let zeros = vec![0u8; chunk];
    // Firmware erase is fast relative to overwriting; simulate ~6x the demo
    // throttle so it visibly completes quicker than a multi-pass overwrite.
    let bps = disk
        .throttle_bps
        .unwrap_or(64 << 20)
        .saturating_mul(6)
        .max(1);
    let start = std::time::Instant::now();

    let mut written: u64 = 0;
    while written < size {
        if cancel.load(Ordering::SeqCst) {
            return Err(FirmwareError::Cancelled);
        }
        let n = ((size - written) as usize).min(chunk);
        file.write_all(&zeros[..n])
            .map_err(|e| FirmwareError::Io(e.to_string()))?;
        written += n as u64;
        on_progress(written as f64 / size as f64);

        // Pace to the simulated rate.
        let target = std::time::Duration::from_secs_f64(written as f64 / bps as f64);
        while start.elapsed() < target {
            if cancel.load(Ordering::SeqCst) {
                return Err(FirmwareError::Cancelled);
            }
            std::thread::sleep(std::time::Duration::from_millis(20).min(target - start.elapsed()));
        }
    }
    file.sync_all()
        .map_err(|e| FirmwareError::Io(e.to_string()))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Linux ioctl pass-through
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod linux {
    //! Real ATA/NVMe command issuance via Linux ioctl pass-through.
    //!
    //! ABI references:
    //! * ATA PASS-THROUGH(16) SCSI op 0x85 over `SG_IO` — SAT-4 / `sg3_utils`.
    //! * ATA security feature set (IDENTIFY words 82/85/89/128) — ACS-4.
    //! * NVMe admin commands over `NVME_IOCTL_ADMIN_CMD` — Linux
    //!   `include/uapi/linux/nvme_ioctl.h`, NVMe Base Spec (Identify, Format
    //!   NVM, Sanitize).
    //!
    //! These paths cannot be exercised without real hardware; they are written
    //! against the documented ABIs and compiled in CI. The capability probes
    //! (IDENTIFY / Identify-Controller) are non-destructive.

    use std::fs::OpenOptions;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{FirmwareError, FirmwareSupport, HiddenAreas};
    use crate::device::Bus;

    const SG_IO: libc::c_ulong = 0x2285;
    const SG_DXFER_FROM_DEV: libc::c_int = -3;
    const SG_DXFER_NONE: libc::c_int = -1;
    const ATA_PASS_THROUGH_16: u8 = 0x85;

    // NVME_IOCTL_ADMIN_CMD = _IOWR('N', 0x41, struct nvme_admin_cmd), 72 bytes.
    const NVME_IOCTL_ADMIN_CMD: libc::c_ulong = 0xC0484E41;

    #[repr(C)]
    struct SgIoHdr {
        interface_id: libc::c_int,
        dxfer_direction: libc::c_int,
        cmd_len: libc::c_uchar,
        mx_sb_len: libc::c_uchar,
        iovec_count: libc::c_ushort,
        dxfer_len: libc::c_uint,
        dxferp: *mut libc::c_void,
        cmdp: *mut libc::c_uchar,
        sbp: *mut libc::c_uchar,
        timeout: libc::c_uint,
        flags: libc::c_uint,
        pack_id: libc::c_int,
        usr_ptr: *mut libc::c_void,
        status: libc::c_uchar,
        masked_status: libc::c_uchar,
        msg_status: libc::c_uchar,
        sb_len_wr: libc::c_uchar,
        host_status: libc::c_ushort,
        driver_status: libc::c_ushort,
        resid: libc::c_int,
        duration: libc::c_uint,
        info: libc::c_uint,
    }

    #[repr(C)]
    struct NvmeAdminCmd {
        opcode: u8,
        flags: u8,
        rsvd1: u16,
        nsid: u32,
        cdw2: u32,
        cdw3: u32,
        metadata: u64,
        addr: u64,
        metadata_len: u32,
        data_len: u32,
        cdw10: u32,
        cdw11: u32,
        cdw12: u32,
        cdw13: u32,
        cdw14: u32,
        cdw15: u32,
        timeout_ms: u32,
        result: u32,
    }

    fn io_err(ctx: &str) -> FirmwareError {
        FirmwareError::Io(format!("{ctx}: {}", std::io::Error::last_os_error()))
    }

    /// Issue an ATA command via ATA PASS-THROUGH(16). `data` (when non-empty)
    /// receives a PIO-in transfer (e.g. IDENTIFY's 512 bytes).
    fn ata_passthrough(
        path: &Path,
        features: u8,
        sector_count: u8,
        lba: u32,
        command: u8,
        data: &mut [u8],
    ) -> Result<(), FirmwareError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| FirmwareError::Io(e.to_string()))?;

        // ATA PASS-THROUGH(16): protocol + flags select PIO data-in vs. no data.
        let (protocol, dxfer_dir, dxfer_len) = if data.is_empty() {
            (3u8 << 1, SG_DXFER_NONE, 0u32) // non-data
        } else {
            (4u8 << 1, SG_DXFER_FROM_DEV, data.len() as u32) // PIO data-in
        };
        // T_LENGTH=2 (in sector_count), BYTE_BLOCK=1, T_DIR=1 (from device).
        let flags = if data.is_empty() { 0x00 } else { 0x2e };

        let cdb: [u8; 16] = [
            ATA_PASS_THROUGH_16,
            protocol | (flags & 0x01),
            flags,
            0, // features (15:8)
            features,
            0, // sector_count (15:8)
            sector_count,
            0, // lba (7:0) high
            (lba & 0xff) as u8,
            0,
            ((lba >> 8) & 0xff) as u8,
            0,
            ((lba >> 16) & 0xff) as u8,
            0,
            command,
            0,
        ];

        let mut sense = [0u8; 32];
        let mut cdb = cdb;
        let mut hdr = SgIoHdr {
            interface_id: b'S' as libc::c_int,
            dxfer_direction: dxfer_dir,
            cmd_len: cdb.len() as u8,
            mx_sb_len: sense.len() as u8,
            iovec_count: 0,
            dxfer_len,
            dxferp: if data.is_empty() {
                std::ptr::null_mut()
            } else {
                data.as_mut_ptr() as *mut libc::c_void
            },
            cmdp: cdb.as_mut_ptr(),
            sbp: sense.as_mut_ptr(),
            timeout: 120_000,
            flags: 0,
            pack_id: 0,
            usr_ptr: std::ptr::null_mut(),
            status: 0,
            masked_status: 0,
            msg_status: 0,
            sb_len_wr: 0,
            host_status: 0,
            driver_status: 0,
            resid: 0,
            duration: 0,
            info: 0,
        };

        // SAFETY: hdr and its referenced buffers outlive the call.
        // The ioctl request arg is c_ulong on glibc but c_int on musl; `as _`
        // adapts to whichever this target uses.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), SG_IO as _, &mut hdr) };
        if rc < 0 {
            return Err(io_err("SG_IO ATA pass-through"));
        }
        if hdr.status != 0 && hdr.status != 2 {
            // status 2 == CHECK CONDITION carrying the ATA return descriptor,
            // which is normal for pass-through; anything else is a failure.
            return Err(FirmwareError::DeviceStatus(hdr.status as u32));
        }
        Ok(())
    }

    /// Read ATA security support from IDENTIFY DEVICE (command 0xEC).
    pub fn detect_ata(path: &Path) -> Result<FirmwareSupport, FirmwareError> {
        let mut id = [0u8; 512];
        ata_passthrough(path, 0, 1, 0, 0xEC, &mut id)?;
        let word = |i: usize| -> u16 { u16::from_le_bytes([id[i * 2], id[i * 2 + 1]]) };

        // Word 82 bit 1: security feature set supported.
        let security_supported = word(82) & (1 << 1) != 0;
        // Word 128: security status. bit0 supported, bit3 frozen, bit5 enhanced.
        let sec = word(128);
        let enhanced = sec & (1 << 5) != 0;
        let frozen = sec & (1 << 3) != 0;

        Ok(FirmwareSupport {
            ata_secure_erase: security_supported,
            ata_enhanced_erase: security_supported && enhanced,
            ata_frozen: frozen,
            ..Default::default()
        })
    }

    /// Issue a 48-bit ATA command with no data transfer, requesting CK_COND so
    /// the drive returns its output registers in the sense buffer, and parse the
    /// 48-bit LBA from the SAT "ATA Status Return Descriptor". Used by
    /// READ NATIVE MAX ADDRESS EXT (0x27) and SET MAX ADDRESS EXT (0x37).
    fn ata_lba48_ret(
        path: &Path,
        features: u8,
        command: u8,
        lba: u64,
    ) -> Result<u64, FirmwareError> {
        use std::os::unix::io::AsRawFd;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| FirmwareError::Io(e.to_string()))?;

        // EXTEND=1 (48-bit), CK_COND=1 (return registers), protocol 3 (non-data).
        let flags = 0x01 | 0x20; // CK_COND | EXTEND
        let cdb: [u8; 16] = [
            ATA_PASS_THROUGH_16,
            (3u8 << 1) | 0x01, // protocol non-data + EXTEND
            flags,
            0,
            features,
            ((lba >> 40) & 0xff) as u8,
            0,
            ((lba >> 24) & 0xff) as u8,
            (lba & 0xff) as u8,
            ((lba >> 32) & 0xff) as u8,
            ((lba >> 8) & 0xff) as u8,
            0,
            ((lba >> 16) & 0xff) as u8,
            0,
            command,
            0,
        ];

        let mut sense = [0u8; 32];
        let mut cdb = cdb;
        let mut hdr = SgIoHdr {
            interface_id: b'S' as libc::c_int,
            dxfer_direction: SG_DXFER_NONE,
            cmd_len: cdb.len() as u8,
            mx_sb_len: sense.len() as u8,
            iovec_count: 0,
            dxfer_len: 0,
            dxferp: std::ptr::null_mut(),
            cmdp: cdb.as_mut_ptr(),
            sbp: sense.as_mut_ptr(),
            timeout: 30_000,
            flags: 0,
            pack_id: 0,
            usr_ptr: std::ptr::null_mut(),
            status: 0,
            masked_status: 0,
            msg_status: 0,
            sb_len_wr: 0,
            host_status: 0,
            driver_status: 0,
            resid: 0,
            duration: 0,
            info: 0,
        };
        // SAFETY: hdr and its buffers outlive the call.
        // The ioctl request arg is c_ulong on glibc but c_int on musl; `as _`
        // adapts to whichever this target uses.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), SG_IO as _, &mut hdr) };
        if rc < 0 {
            return Err(io_err("SG_IO ATA pass-through (lba48)"));
        }
        // Descriptor-format sense: the ATA Return Descriptor (code 0x09) sits at
        // offset 8. Its layout: [0]=0x09 [1]=len [2]=flags [3]=error [4]=count_hi
        // [5]=count_lo [6]=lba(31:24) [7]=lba(7:0) [8]=lba(39:32) [9]=lba(15:8)
        // [10]=lba(47:40) [11]=lba(23:16) [12]=device [13]=status.
        if sense[0] == 0x72 && sense[8] == 0x09 {
            let d = &sense[8..];
            let lba_low = d[7] as u64; // 7:0
            let lba_mid = d[9] as u64; // 15:8
            let lba_hi = d[11] as u64; // 23:16
            let lba_3 = d[6] as u64; // 31:24
            let lba_4 = d[8] as u64; // 39:32
            let lba_5 = d[10] as u64; // 47:40
            let max_lba = lba_low
                | (lba_mid << 8)
                | (lba_hi << 16)
                | (lba_3 << 24)
                | (lba_4 << 32)
                | (lba_5 << 40);
            // The returned LBA is the highest addressable sector; +1 = count.
            return Ok(max_lba + 1);
        }
        Err(FirmwareError::Io(
            "ATA return descriptor not present".into(),
        ))
    }

    /// Detect HPA/DCO hidden sectors on an ATA disk.
    pub fn detect_hidden_ata(
        path: &Path,
        advertised_bytes: u64,
    ) -> Result<HiddenAreas, FirmwareError> {
        // Logical sector size from IDENTIFY words 117-118 when valid, else 512.
        let mut id = [0u8; 512];
        ata_passthrough(path, 0, 1, 0, 0xEC, &mut id)?;
        let word = |i: usize| -> u16 { u16::from_le_bytes([id[i * 2], id[i * 2 + 1]]) };
        let logical_words = u32::from(word(117)) | (u32::from(word(118)) << 16);
        let sector_size = if word(106) & (1 << 12) != 0 && logical_words > 0 {
            logical_words * 2
        } else {
            512
        };
        let current = advertised_bytes / sector_size as u64;

        // HPA: READ NATIVE MAX ADDRESS EXT (0x27) gives the native capacity.
        let native = ata_lba48_ret(path, 0, 0x27, 0)
            .unwrap_or(current)
            .max(current);

        // DCO: DEVICE CONFIGURATION IDENTIFY (0xB1 / feature 0xC2), 512-byte data.
        // Words 3-4 hold the real max sectors (LBA count).
        let real = {
            let mut buf = [0u8; 512];
            match ata_passthrough(path, 0xC2, 1, 0, 0xB1, &mut buf) {
                Ok(()) => {
                    let w = |i: usize| u16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]) as u64;
                    let dco_max = w(3) | (w(4) << 16) | (w(5) << 32);
                    dco_max.max(native)
                }
                Err(_) => native, // DCO not supported
            }
        };

        Ok(HiddenAreas {
            current_sectors: current,
            native_sectors: native,
            real_sectors: real,
            sector_size,
        })
    }

    /// Remove HPA (SET MAX ADDRESS EXT) and DCO (DEVICE CONFIGURATION RESTORE),
    /// returning the resulting full capacity in bytes.
    pub fn reveal_hidden_ata(path: &Path) -> Result<u64, FirmwareError> {
        let h = detect_hidden_ata(path, 0)?;
        // DEVICE CONFIGURATION RESTORE (0xB1 / feature 0xC0) drops the DCO first.
        if h.has_dco() {
            let mut empty: [u8; 0] = [];
            let _ = ata_passthrough(path, 0xC0, 0, 0, 0xB1, &mut empty);
        }
        // SET MAX ADDRESS EXT (0x37) to the native max removes the HPA. Volatile
        // bit (feature 0) not set, so the change persists.
        let native = ata_lba48_ret(path, 0, 0x27, 0)?;
        let _ = ata_lba48_ret(path, 0, 0x37, native.saturating_sub(1))?;
        // Re-read to report the new capacity.
        let after = detect_hidden_ata(path, 0)?;
        Ok(after.real_sectors * after.sector_size as u64)
    }

    /// ATA Security Erase: SET PASSWORD (0xF1) then ERASE UNIT (0xF4).
    pub fn ata_secure_erase(path: &Path, enhanced: bool) -> Result<(), FirmwareError> {
        // A throwaway user password is required by the spec before ERASE UNIT;
        // the erase clears it again. Password block is 512 bytes: control word
        // then 32 bytes of password at offset 2.
        let mut pw = [0u8; 512];
        pw[2..6].copy_from_slice(b"dban");
        ata_passthrough(path, 0, 1, 0, 0xF1, &mut pw.clone())?;

        // ERASE UNIT: feature 0x02 selects enhanced erase.
        let feature = if enhanced { 0x02 } else { 0x00 };
        let mut block = pw; // same 512-byte password block layout
        ata_passthrough(path, feature, 1, 0, 0xF4, &mut block)?;
        Ok(())
    }

    fn nvme_admin(path: &Path, cmd: &mut NvmeAdminCmd) -> Result<(), FirmwareError> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| FirmwareError::Io(e.to_string()))?;
        // SAFETY: cmd and any buffer it points at outlive the call.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), NVME_IOCTL_ADMIN_CMD as _, cmd) };
        if rc < 0 {
            return Err(io_err("NVME_IOCTL_ADMIN_CMD"));
        }
        if rc != 0 {
            // Positive return is the NVMe status field.
            return Err(FirmwareError::DeviceStatus(rc as u32));
        }
        Ok(())
    }

    fn blank_admin() -> NvmeAdminCmd {
        NvmeAdminCmd {
            opcode: 0,
            flags: 0,
            rsvd1: 0,
            nsid: 0,
            cdw2: 0,
            cdw3: 0,
            metadata: 0,
            addr: 0,
            metadata_len: 0,
            data_len: 0,
            cdw10: 0,
            cdw11: 0,
            cdw12: 0,
            cdw13: 0,
            cdw14: 0,
            cdw15: 0,
            timeout_ms: 0,
            result: 0,
        }
    }

    /// Identify Controller (opcode 0x06, CNS=1) to read OACS/FNA/SANICAP.
    pub fn detect_nvme(path: &Path) -> Result<FirmwareSupport, FirmwareError> {
        let mut buf = [0u8; 4096];
        let mut cmd = blank_admin();
        cmd.opcode = 0x06;
        cmd.nsid = 0;
        cmd.addr = buf.as_mut_ptr() as u64;
        cmd.data_len = buf.len() as u32;
        cmd.cdw10 = 1; // CNS = Identify Controller
        nvme_admin(path, &mut cmd)?;

        // FNA (byte 524) bit 2: crypto erase supported as part of Format.
        let fna = buf[524];
        let crypto_format = fna & (1 << 2) != 0;
        // SANICAP (bytes 328..332): bit0 crypto erase, bit1 block erase.
        let sanicap = u32::from_le_bytes([buf[328], buf[329], buf[330], buf[331]]);
        let sanitize_crypto = sanicap & (1 << 0) != 0;
        let sanitize_block = sanicap & (1 << 1) != 0;

        Ok(FirmwareSupport {
            nvme_format_user: true, // Format NVM is mandatory
            nvme_format_crypto: crypto_format,
            nvme_sanitize_block: sanitize_block,
            nvme_sanitize_crypto: sanitize_crypto,
            ..Default::default()
        })
    }

    /// NVMe Format NVM (opcode 0x80). SES field in CDW10 bits 11:9.
    pub fn nvme_format(path: &Path, crypto: bool) -> Result<(), FirmwareError> {
        let ses: u32 = if crypto { 2 } else { 1 };
        let mut cmd = blank_admin();
        cmd.opcode = 0x80;
        cmd.nsid = 0xffff_ffff; // all namespaces
        cmd.cdw10 = ses << 9;
        cmd.timeout_ms = 600_000;
        nvme_admin(path, &mut cmd)
    }

    /// NVMe Sanitize (opcode 0x84). SANACT in CDW10 bits 2:0: 2 block erase,
    /// 4 crypto erase. The operation is asynchronous; we kick it off and poll
    /// the Sanitize Status log page until it reports completion.
    pub fn nvme_sanitize(
        path: &Path,
        crypto: bool,
        cancel: &AtomicBool,
    ) -> Result<(), FirmwareError> {
        let sanact: u32 = if crypto { 4 } else { 2 };
        let mut cmd = blank_admin();
        cmd.opcode = 0x84;
        cmd.cdw10 = sanact;
        nvme_admin(path, &mut cmd)?;

        // Poll Get Log Page (opcode 0x02), log id 0x81 (Sanitize Status).
        loop {
            if cancel.load(Ordering::SeqCst) {
                return Err(FirmwareError::Cancelled);
            }
            let mut log = [0u8; 512];
            let dwords = (log.len() / 4 - 1) as u32;
            let mut get = blank_admin();
            get.opcode = 0x02;
            get.nsid = 0xffff_ffff;
            get.addr = log.as_mut_ptr() as u64;
            get.data_len = log.len() as u32;
            get.cdw10 = 0x81 | (dwords << 16);
            nvme_admin(path, &mut get)?;

            // SSTAT (bytes 2..4) bits 2:0: 1 == completed successfully.
            let sstat = u16::from_le_bytes([log[2], log[3]]);
            match sstat & 0x7 {
                0 | 1 => return Ok(()), // never sanitized / completed OK
                2 => {
                    // In progress: wait and re-poll.
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                other => return Err(FirmwareError::DeviceStatus(other as u32)),
            }
        }
    }

    // -----------------------------------------------------------------------
    // TCG Opal (Level 0 Discovery + Revert)
    // -----------------------------------------------------------------------
    //
    // The Trusted Computing Group security protocol is carried over SCSI
    // SECURITY PROTOCOL IN/OUT (op 0xA2/0xB5, here via SG_IO) for SATA, and
    // over NVMe Security Receive/Send (admin 0x82/0x81) for NVMe. Level 0
    // Discovery (protocol 0x01, ComID 0x0001) is non-destructive and reports
    // which Security Subsystem Class (SSC) the drive implements. The Revert
    // method cryptographically erases the drive by destroying its media key.
    //
    // The Revert session here authenticates with the drive's default MSID
    // credential, which factory-state ("unowned") SEDs accept — the common
    // boot-and-nuke case. Drives that have been taken into ownership require
    // their PSID (printed on the label) instead; that path is not automated.
    // These routines are written against the TCG Opal SSC spec and compiled in
    // CI, but can only be validated against real SED hardware.

    const SECURITY_PROTOCOL_DISCOVERY: u8 = 0x01;
    const TCG_DISCOVERY_COMID: u16 = 0x0001;

    fn is_nvme(bus: Bus) -> bool {
        bus == Bus::Nvme
    }

    /// SCSI SECURITY PROTOCOL IN/OUT via SG_IO. `to_device` selects OUT.
    fn scsi_security(
        path: &Path,
        to_device: bool,
        protocol: u8,
        comid: u16,
        buf: &mut [u8],
    ) -> Result<(), FirmwareError> {
        use std::os::unix::io::AsRawFd;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| FirmwareError::Io(e.to_string()))?;
        let len = buf.len() as u32;
        let op = if to_device { 0xB5u8 } else { 0xA2u8 };
        let cdb: [u8; 12] = [
            op,
            protocol,
            (comid >> 8) as u8,
            (comid & 0xff) as u8,
            0,
            0,
            (len >> 24) as u8,
            (len >> 16) as u8,
            (len >> 8) as u8,
            (len & 0xff) as u8,
            0,
            0,
        ];
        let mut sense = [0u8; 32];
        let mut cdb = cdb;
        let mut hdr = SgIoHdr {
            interface_id: b'S' as libc::c_int,
            dxfer_direction: if to_device {
                2 /* SG_DXFER_TO_DEV */
            } else {
                SG_DXFER_FROM_DEV
            },
            cmd_len: cdb.len() as u8,
            mx_sb_len: sense.len() as u8,
            iovec_count: 0,
            dxfer_len: len,
            dxferp: buf.as_mut_ptr() as *mut libc::c_void,
            cmdp: cdb.as_mut_ptr(),
            sbp: sense.as_mut_ptr(),
            timeout: 30_000,
            flags: 0,
            pack_id: 0,
            usr_ptr: std::ptr::null_mut(),
            status: 0,
            masked_status: 0,
            msg_status: 0,
            sb_len_wr: 0,
            host_status: 0,
            driver_status: 0,
            resid: 0,
            duration: 0,
            info: 0,
        };
        // SAFETY: hdr and buffers outlive the call.
        // The ioctl request arg is c_ulong on glibc but c_int on musl; `as _`
        // adapts to whichever this target uses.
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), SG_IO as _, &mut hdr) };
        if rc < 0 {
            return Err(io_err("SG_IO SECURITY PROTOCOL"));
        }
        if hdr.status != 0 {
            return Err(FirmwareError::DeviceStatus(hdr.status as u32));
        }
        Ok(())
    }

    /// NVMe Security Send/Receive (admin opcode 0x81/0x82).
    fn nvme_security(
        path: &Path,
        send: bool,
        protocol: u8,
        comid: u16,
        buf: &mut [u8],
    ) -> Result<(), FirmwareError> {
        let mut cmd = blank_admin();
        cmd.opcode = if send { 0x81 } else { 0x82 };
        cmd.addr = buf.as_mut_ptr() as u64;
        cmd.data_len = buf.len() as u32;
        // SECP (protocol) in cdw10 bits 31:24, SPSP (ComID) in bits 23:8.
        cmd.cdw10 = (u32::from(protocol) << 24) | (u32::from(comid) << 8);
        cmd.cdw11 = buf.len() as u32;
        nvme_admin(path, &mut cmd)
    }

    fn sec_recv(
        path: &Path,
        bus: Bus,
        protocol: u8,
        comid: u16,
        buf: &mut [u8],
    ) -> Result<(), FirmwareError> {
        if is_nvme(bus) {
            nvme_security(path, false, protocol, comid, buf)
        } else {
            scsi_security(path, false, protocol, comid, buf)
        }
    }

    fn sec_send(
        path: &Path,
        bus: Bus,
        protocol: u8,
        comid: u16,
        buf: &mut [u8],
    ) -> Result<(), FirmwareError> {
        if is_nvme(bus) {
            nvme_security(path, true, protocol, comid, buf)
        } else {
            scsi_security(path, true, protocol, comid, buf)
        }
    }

    /// True when a feature code is a TCG Security Subsystem Class (Opal,
    /// Opalite, Pyrite, Ruby) — i.e. the drive is a revertable SED.
    fn is_ssc(code: u16) -> bool {
        matches!(
            code,
            0x0200 | 0x0201 | 0x0202 | 0x0203 | 0x0301 | 0x0302 | 0x0303 | 0x0304
        )
    }

    /// TCG Level 0 Discovery. Returns the SED's base ComID when it advertises an
    /// SSC, or `None` for a non-SED.
    fn discover_comid(path: &Path, bus: Bus) -> Result<Option<u16>, FirmwareError> {
        let mut buf = [0u8; 512];
        sec_recv(
            path,
            bus,
            SECURITY_PROTOCOL_DISCOVERY,
            TCG_DISCOVERY_COMID,
            &mut buf,
        )?;
        // Header: bytes 0..4 = length of valid data. Descriptors start at 48.
        let total = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        let end = (total + 4).min(buf.len());
        let mut off = 48usize;
        let mut comid = None;
        while off + 4 <= end {
            let code = u16::from_be_bytes([buf[off], buf[off + 1]]);
            let dlen = buf[off + 3] as usize; // length of the feature body
            if is_ssc(code) {
                // SSC descriptors carry the base ComID at body offset 0..2.
                if off + 6 <= buf.len() {
                    comid = Some(u16::from_be_bytes([buf[off + 4], buf[off + 5]]));
                }
            }
            if dlen == 0 {
                break;
            }
            off += 4 + dlen;
        }
        Ok(comid)
    }

    /// Detect whether the drive is a TCG Opal SED (non-destructive).
    pub fn detect_opal(path: &Path, bus: Bus) -> Result<bool, FirmwareError> {
        Ok(discover_comid(path, bus)?.is_some())
    }

    /// Cryptographically erase a TCG Opal SED by reverting it to factory state.
    pub fn tcg_revert(path: &Path, bus: Bus) -> Result<(), FirmwareError> {
        let comid = discover_comid(path, bus)?.ok_or(FirmwareError::Unsupported)?;
        let session = TcgSession::open(path, bus, comid)?;
        session.revert_admin_sp()
    }

    /// A minimal TCG Opal session sufficient to read the MSID and invoke Revert
    /// on the Admin SP. Encodes ComPacket/Packet/SubPacket framing and the
    /// handful of method calls the revert flow needs.
    struct TcgSession<'a> {
        path: &'a Path,
        bus: Bus,
        comid: u16,
        host_session: u32,
        tper_session: u32,
    }

    // TCG UIDs (8 bytes, big-endian) used by the revert flow.
    const UID_SMUID: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 0xff];
    const UID_THISSP: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 0x01];
    const UID_ADMIN_SP: [u8; 8] = [0, 0, 0x02, 0x05, 0, 0, 0, 0x01];
    const UID_C_PIN_MSID: [u8; 8] = [0, 0, 0, 0x0b, 0, 0, 0x84, 0x02];
    const UID_AUTH_SID: [u8; 8] = [0, 0, 0, 0x09, 0, 0, 0, 0x06];
    const METHOD_START_SESSION: [u8; 8] = [0, 0, 0, 0xff, 0, 0, 0xff, 0x02];
    const METHOD_GET: [u8; 8] = [0, 0, 0, 0x06, 0, 0, 0, 0x16];
    const METHOD_REVERT: [u8; 8] = [0, 0, 0, 0x06, 0, 0, 0x02, 0x02];

    impl<'a> TcgSession<'a> {
        fn open(path: &'a Path, bus: Bus, comid: u16) -> Result<Self, FirmwareError> {
            Ok(TcgSession {
                path,
                bus,
                comid,
                host_session: 0x6462_616e, // "dban"
                tper_session: 0,
            })
        }

        /// Wrap a method-call payload in TCG ComPacket/Packet/SubPacket framing
        /// and IF-SEND it, then IF-RECV the response.
        fn invoke(&self, payload: &[u8]) -> Result<Vec<u8>, FirmwareError> {
            // SubPacket header: 6 reserved bytes, 2-byte kind (0), 4-byte length
            // (here the payload is < 64 KiB so the high half is zero), then the
            // payload padded to a 4-byte boundary.
            let mut subpkt = vec![0u8; 8];
            subpkt[6] = (payload.len() >> 8) as u8;
            subpkt[7] = payload.len() as u8;
            subpkt.extend_from_slice(payload);
            while !subpkt.len().is_multiple_of(4) {
                subpkt.push(0);
            }
            // Packet header: session (tper||host), seq, etc. then length.
            let mut pkt = Vec::new();
            pkt.extend_from_slice(&self.tper_session.to_be_bytes());
            pkt.extend_from_slice(&self.host_session.to_be_bytes());
            pkt.extend_from_slice(&[0; 4]); // seq number
            pkt.extend_from_slice(&[0; 2]); // reserved
            pkt.extend_from_slice(&[0; 2]); // ack type
            pkt.extend_from_slice(&[0; 4]); // acknowledgement
            pkt.extend_from_slice(&(subpkt.len() as u32).to_be_bytes());
            pkt.extend_from_slice(&subpkt);
            // ComPacket header: reserved(4), comID(2), comID-ext(2), out seq(4),
            // reserved(2), min transfer(2), length(4).
            let mut com = Vec::new();
            com.extend_from_slice(&[0; 4]);
            com.extend_from_slice(&self.comid.to_be_bytes());
            com.extend_from_slice(&[0; 2]);
            com.extend_from_slice(&[0; 4]);
            com.extend_from_slice(&[0; 2]);
            com.extend_from_slice(&[0; 2]);
            com.extend_from_slice(&(pkt.len() as u32).to_be_bytes());
            com.extend_from_slice(&pkt);

            // IF-SEND the request (padded to a 512-byte multiple).
            let mut tx = com.clone();
            while !tx.len().is_multiple_of(512) {
                tx.push(0);
            }
            sec_send(self.path, self.bus, 0x01, self.comid, &mut tx)?;

            // IF-RECV the response.
            let mut rx = vec![0u8; 2048];
            sec_recv(self.path, self.bus, 0x01, self.comid, &mut rx)?;
            Ok(rx)
        }

        /// Open an anonymous read session to the Admin SP, then invoke Revert
        /// authenticated by the MSID credential. On success the drive resets.
        fn revert_admin_sp(&self) -> Result<(), FirmwareError> {
            // 1. StartSession (anonymous) on the Admin SP to read the MSID.
            let mut p = Vec::new();
            p.push(0xf8); // Call token
            p.extend(token_bytes(&UID_SMUID));
            p.extend(token_bytes(&METHOD_START_SESSION));
            p.push(0xf0); // start list
            p.extend(token_uint(self.host_session as u64));
            p.extend(token_bytes(&UID_ADMIN_SP));
            p.push(0x01); // write = true
            p.push(0xf1); // end list
            p.push(0xf9); // end of data
            p.extend_from_slice(&[0x00, 0x00, 0x00]); // status list
            let _ = self.invoke(&p)?;

            // 2. Get the MSID PIN from C_PIN_MSID (best-effort parse).
            let mut g = Vec::new();
            g.push(0xf8);
            g.extend(token_bytes(&UID_C_PIN_MSID));
            g.extend(token_bytes(&METHOD_GET));
            g.push(0xf0);
            g.push(0xf1);
            g.push(0xf9);
            g.extend_from_slice(&[0x00, 0x00, 0x00]);
            let resp = self.invoke(&g)?;
            let msid = extract_first_bytes_token(&resp).unwrap_or_default();

            // 3. Invoke Revert on the Admin SP, authenticated as SID with MSID.
            //    (Encoded as a Revert call carrying the host challenge.)
            let mut r = Vec::new();
            r.push(0xf8);
            r.extend(token_bytes(&UID_THISSP));
            r.extend(token_bytes(&METHOD_REVERT));
            r.push(0xf0);
            r.extend(token_bytes(&UID_AUTH_SID));
            r.extend(token_blob(&msid));
            r.push(0xf1);
            r.push(0xf9);
            r.extend_from_slice(&[0x00, 0x00, 0x00]);
            self.invoke(&r)?;
            Ok(())
        }
    }

    /// Encode an 8-byte UID as a TCG short-atom bytes token.
    fn token_bytes(uid: &[u8; 8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(9);
        v.push(0xa8); // short atom, byte, length 8
        v.extend_from_slice(uid);
        v
    }

    /// Encode a variable-length byte blob as a TCG token.
    fn token_blob(data: &[u8]) -> Vec<u8> {
        if data.len() <= 15 {
            let mut v = vec![0xa0 | data.len() as u8];
            v.extend_from_slice(data);
            v
        } else {
            let mut v = vec![0xd0, (data.len() >> 8) as u8, data.len() as u8];
            v.extend_from_slice(data);
            v
        }
    }

    /// Encode an unsigned integer as a TCG token.
    fn token_uint(mut n: u64) -> Vec<u8> {
        if n < 64 {
            return vec![n as u8]; // tiny atom
        }
        let mut bytes = Vec::new();
        while n > 0 {
            bytes.insert(0, (n & 0xff) as u8);
            n >>= 8;
        }
        let mut v = vec![0x80 | (0x10 | bytes.len() as u8)];
        v.extend_from_slice(&bytes);
        v
    }

    /// Find the first bytes-token in a TCG response and return its payload.
    fn extract_first_bytes_token(resp: &[u8]) -> Option<Vec<u8>> {
        let mut i = 0;
        while i < resp.len() {
            let b = resp[i];
            if (0xa0..=0xbf).contains(&b) {
                let len = (b & 0x0f) as usize;
                if i + 1 + len <= resp.len() && len > 0 {
                    return Some(resp[i + 1..i + 1 + len].to_vec());
                }
            }
            i += 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{Bus, MediaKind};
    use std::path::PathBuf;
    use std::sync::atomic::AtomicBool;

    fn disk(bus: Bus, path: PathBuf, size: u64) -> Disk {
        Disk {
            path,
            name: "t".into(),
            model: "Test".into(),
            serial: "S".into(),
            size_bytes: size,
            bus,
            kind: MediaKind::Ssd,
            removable: false,
            lock: None,
            simulated: true,
            throttle_bps: None,
        }
    }

    #[test]
    fn method_metadata_is_unique_and_complete() {
        let all = [
            FirmwareMethod::AtaSecureErase { enhanced: false },
            FirmwareMethod::AtaSecureErase { enhanced: true },
            FirmwareMethod::NvmeFormat { crypto: false },
            FirmwareMethod::NvmeFormat { crypto: true },
            FirmwareMethod::NvmeSanitize { crypto: false },
            FirmwareMethod::NvmeSanitize { crypto: true },
            FirmwareMethod::TcgRevert,
        ];
        let mut ids: Vec<_> = all.iter().map(|m| m.id()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len(), "ids must be unique");
        for m in all {
            assert!(!m.name().is_empty());
            assert!(!m.description().is_empty());
        }
    }

    #[test]
    fn demo_capability_matrix() {
        // SATA SSD: 2 ATA erases + Opal revert.
        let sata = demo_support(&disk(Bus::Sata, "x".into(), 0));
        assert!(sata.ata_secure_erase && sata.ata_enhanced_erase && sata.opal);
        assert_eq!(sata.methods().len(), 3);

        // NVMe: 2 Format + 2 Sanitize + Opal revert.
        let nvme = demo_support(&disk(Bus::Nvme, "x".into(), 0));
        assert!(nvme.nvme_format_crypto && nvme.nvme_sanitize_block && nvme.opal);
        assert_eq!(nvme.methods().len(), 5);

        let usb = demo_support(&disk(Bus::Usb, "x".into(), 0));
        assert!(!usb.any());
    }

    #[test]
    fn opal_revert_is_offered_and_simulated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sed.img");
        std::fs::write(&path, vec![0xCDu8; 4096]).unwrap();
        let mut d = disk(Bus::Nvme, path.clone(), 4096);
        d.throttle_bps = Some(64 << 20);
        assert!(detect_support(&d).supports(FirmwareMethod::TcgRevert));

        let cancel = AtomicBool::new(false);
        execute(&d, FirmwareMethod::TcgRevert, &cancel, |_| {}).unwrap();
        assert!(std::fs::read(&path).unwrap().iter().all(|&b| b == 0));
    }

    #[test]
    fn frozen_hides_ata_methods() {
        let s = FirmwareSupport {
            ata_secure_erase: true,
            ata_enhanced_erase: true,
            ata_frozen: true,
            ..Default::default()
        };
        assert!(s.methods().is_empty(), "frozen drive offers no erase");
    }

    #[test]
    fn hidden_area_math() {
        let h = HiddenAreas {
            current_sectors: 100,
            native_sectors: 120,
            real_sectors: 150,
            sector_size: 512,
        };
        assert!(h.has_hpa() && h.has_dco() && h.any());
        assert_eq!(h.hidden_sectors(), 50);
        assert_eq!(h.hidden_bytes(), 50 * 512);
        assert_eq!(h.full_bytes(), 150 * 512);
        let clean = HiddenAreas::none(100, 512);
        assert!(!clean.any() && !clean.has_hpa() && !clean.has_dco());
    }

    #[test]
    fn simulated_hidden_detect_and_reveal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.img");
        // Backing file is 4 MiB but the disk advertises only 3 MiB: 1 MiB HPA.
        std::fs::write(&path, vec![0u8; 4 << 20]).unwrap();
        let d = disk(Bus::Demo, path, 3 << 20);

        let h = detect_hidden_areas(&d);
        assert!(h.has_hpa());
        assert_eq!(h.hidden_bytes(), 1 << 20);
        assert_eq!(h.full_bytes(), 4 << 20);
        assert_eq!(reveal_hidden_areas(&d).unwrap(), 4 << 20);

        // A disk whose file matches its size has nothing hidden.
        let path2 = dir.path().join("clean.img");
        std::fs::write(&path2, vec![0u8; 1 << 20]).unwrap();
        let c = disk(Bus::Demo, path2, 1 << 20);
        assert!(!detect_hidden_areas(&c).any());
        assert_eq!(reveal_hidden_areas(&c).unwrap(), 1 << 20);
    }

    #[test]
    fn simulated_erase_zeroes_backing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.img");
        std::fs::write(&path, vec![0xABu8; 4096]).unwrap();
        let mut d = disk(Bus::Demo, path.clone(), 4096);
        d.throttle_bps = Some(64 << 20);

        let cancel = AtomicBool::new(false);
        let mut last = 0.0;
        execute(
            &d,
            FirmwareMethod::NvmeFormat { crypto: true },
            &cancel,
            |p| last = p,
        )
        .unwrap();
        assert!((last - 1.0).abs() < 1e-9, "progress should reach 100%");

        let contents = std::fs::read(&path).unwrap();
        assert!(contents.iter().all(|&b| b == 0), "file must be zeroed");
    }

    #[test]
    fn simulated_erase_honors_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("disk.img");
        std::fs::write(&path, vec![0xABu8; 8 << 20]).unwrap();
        let mut d = disk(Bus::Demo, path, 8 << 20);
        d.throttle_bps = Some(1 << 20); // slow, so cancel lands mid-flight

        let cancel = AtomicBool::new(true); // already cancelled
        let err = execute(
            &d,
            FirmwareMethod::NvmeFormat { crypto: false },
            &cancel,
            |_| {},
        );
        assert!(matches!(err, Err(FirmwareError::Cancelled)));
    }

    #[test]
    fn detect_support_demo_dispatch() {
        // A simulated disk short-circuits to the synthetic matrix without ever
        // touching ioctl, even with a non-existent path. (`disk()` sets
        // simulated = true.)
        let demo = disk(Bus::Nvme, "does-not-exist".into(), 0);
        assert!(demo.simulated);
        assert!(detect_support(&demo).any());
    }
}
