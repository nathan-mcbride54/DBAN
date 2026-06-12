//! Firmware-erase flow tests, driven through the public app state machine.
//! Uses a provider of tiny simulated disks (SATA / NVMe / USB) so the demo
//! firmware path runs to completion in milliseconds.

use std::time::Duration;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use scour::app::{App, MethodChoice, Screen};

use scour_core::device::{Bus, Disk, DiskProvider, MediaKind};
use scour_core::firmware::FirmwareMethod;
use scour_core::CoreError;

struct FwProvider {
    dir: tempfile::TempDir,
}

impl FwProvider {
    fn new() -> Self {
        FwProvider {
            dir: tempfile::tempdir().unwrap(),
        }
    }
}

impl DiskProvider for FwProvider {
    fn refresh(&mut self) -> Result<Vec<Disk>, CoreError> {
        let mut disks = Vec::new();
        for (name, bus, kind) in [
            ("sda", Bus::Sata, MediaKind::Ssd),
            ("nvme0n1", Bus::Nvme, MediaKind::Ssd),
            ("sdc", Bus::Usb, MediaKind::Ssd),
        ] {
            let path = self.dir.path().join(format!("{name}.img"));
            let f = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(&path)
                .unwrap();
            f.set_len(64 * 1024).unwrap();
            disks.push(Disk {
                path,
                name: name.to_string(),
                model: format!("Sim {name}"),
                serial: format!("S-{name}"),
                size_bytes: 64 * 1024,
                bus,
                kind,
                removable: bus == Bus::Usb,
                lock: None,
                simulated: true,
                throttle_bps: None, // finish ASAP
            });
        }
        Ok(disks)
    }
    fn is_simulation(&self) -> bool {
        true
    }
}

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}
fn special(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn app() -> App {
    App::new(Box::new(FwProvider::new()), false)
}

/// Point the method picker at a specific firmware method.
fn select_method(app: &mut App, want: FirmwareMethod) {
    let idx = app
        .methods
        .iter()
        .position(|m| *m == MethodChoice::Firmware(want))
        .expect("method present");
    app.method_idx = idx;
}

/// Move the cursor to the named disk and toggle it on.
fn toggle(app: &mut App, name: &str) {
    let idx = app.disks.iter().position(|d| d.name == name).unwrap();
    app.cursor = idx;
    app.on_key(key(' '));
}

#[test]
fn firmware_methods_are_offered() {
    let app = app();
    let fw = app.methods.iter().filter(|m| m.is_firmware()).count();
    assert_eq!(fw, 6, "all six firmware methods should be in the picker");
}

#[test]
fn capability_is_detected_per_bus() {
    let app = app();
    let sata = app.supports.get("sda").unwrap();
    let nvme = app.supports.get("nvme0n1").unwrap();
    let usb = app.supports.get("sdc").unwrap();
    assert!(sata.ata_secure_erase, "SATA disk should support ATA erase");
    assert!(nvme.nvme_format_user, "NVMe disk should support Format");
    assert!(!usb.any(), "USB disk should advertise no firmware erase");
}

#[test]
fn nvme_format_runs_to_completion() {
    let mut app = app();
    select_method(&mut app, FirmwareMethod::NvmeFormat { crypto: true });
    toggle(&mut app, "nvme0n1");
    assert_eq!(app.target_disks().len(), 1);

    app.on_key(key('s'));
    assert_eq!(app.screen, Screen::Confirm);
    for c in "ERASE 1 DISK".chars() {
        app.on_key(key(c));
    }
    app.on_key(special(KeyCode::Enter));
    app.on_tick(Duration::from_millis(6000)); // burn the countdown
    assert_eq!(app.screen, Screen::Wiping);

    let mut guard = 0;
    while app.screen != Screen::Summary {
        app.on_tick(Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(5));
        guard += 1;
        assert!(guard < 2000, "firmware erase did not complete");
    }
    let report = app.session_report.as_ref().unwrap();
    assert_eq!(report.jobs.len(), 1);
    assert!(report.all_succeeded());
    let job = &report.jobs[0];
    assert!(job.firmware, "report should be flagged as a firmware erase");
    assert_eq!(job.method_id, "nvme-format-crypto");
}

#[test]
fn unsupported_disk_is_skipped_not_erased() {
    let mut app = app();
    // ATA erase chosen, but only the USB disk is selected — it cannot do it.
    select_method(&mut app, FirmwareMethod::AtaSecureErase { enhanced: false });
    toggle(&mut app, "sdc"); // USB: no firmware support
    assert_eq!(app.selected_disks().len(), 1);
    assert_eq!(
        app.target_disks().len(),
        0,
        "USB disk is not a valid target"
    );

    app.on_key(key('s'));
    assert_eq!(
        app.screen,
        Screen::Disks,
        "must not arm when no selected disk supports the method"
    );
}

#[test]
fn mixed_selection_targets_only_capable_disks() {
    let mut app = app();
    select_method(&mut app, FirmwareMethod::AtaSecureErase { enhanced: false });
    toggle(&mut app, "sda"); // SATA: supported
    toggle(&mut app, "sdc"); // USB: not supported
    assert_eq!(app.selected_disks().len(), 2);
    let targets = app.target_disks();
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].name, "sda");
}
