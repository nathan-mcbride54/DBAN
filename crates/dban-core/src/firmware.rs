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
        }
    }

    /// Which bus this command belongs to.
    pub fn bus(&self) -> Bus {
        match self {
            FirmwareMethod::AtaSecureErase { .. } => Bus::Sata,
            FirmwareMethod::NvmeFormat { .. } | FirmwareMethod::NvmeSanitize { .. } => Bus::Nvme,
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
        match disk.bus {
            Bus::Sata => linux::detect_ata(&disk.path).unwrap_or_default(),
            Bus::Nvme => linux::detect_nvme(&disk.path).unwrap_or_default(),
            _ => FirmwareSupport::default(),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        FirmwareSupport::default()
    }
}

/// Synthesize plausible capabilities for the simulated demo disks so every
/// firmware method appears at least once in the picker.
fn demo_support(disk: &Disk) -> FirmwareSupport {
    match disk.bus {
        Bus::Sata => FirmwareSupport {
            ata_secure_erase: true,
            ata_enhanced_erase: true,
            ata_frozen: false,
            ..Default::default()
        },
        Bus::Nvme => FirmwareSupport {
            nvme_format_user: true,
            nvme_format_crypto: true,
            nvme_sanitize_block: true,
            nvme_sanitize_crypto: true,
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

    use super::{FirmwareError, FirmwareSupport};

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
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), SG_IO, &mut hdr) };
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
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), NVME_IOCTL_ADMIN_CMD, cmd) };
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
        let sata = demo_support(&disk(Bus::Sata, "x".into(), 0));
        assert!(sata.ata_secure_erase && sata.ata_enhanced_erase);
        assert!(sata.methods().len() == 2);

        let nvme = demo_support(&disk(Bus::Nvme, "x".into(), 0));
        assert!(nvme.nvme_format_crypto && nvme.nvme_sanitize_block);
        assert_eq!(nvme.methods().len(), 4);

        let usb = demo_support(&disk(Bus::Usb, "x".into(), 0));
        assert!(!usb.any());
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
