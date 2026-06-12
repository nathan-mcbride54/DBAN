# DBAN

**DBAN — Disk Boot and Nuke.** A modern, from-scratch boot-and-nuke disk
eraser written in Rust: a tiny bootable image that brings up a clean terminal
UI, shows you the hardware and disks in the machine, and sanitizes the ones you
explicitly choose — by software overwrite (every major published standard) or
by drive-internal firmware erase (ATA Secure Erase, NVMe Format / Sanitize).

```
 DBAN  secure disk sanitization              8 cores │ 32 GB │ Linux 6.6
────────────────────────────────────────────────────────────────────────────
 ╭ Disks ───────────────────────────────────────────────────────────────────╮
 │     DEVICE     MODEL                       SIZE   BUS    TYPE  STATE       │
 │▌█  nvme0n1     Samsung SSD 980 PRO       1.0 TB   NVMe   SSD   WILL ERASE  │
 │    sda         Seagate Barracuda ST2000  2.0 TB   SATA   HDD   ready       │
 │ ▒  sdb         DBAN boot medium        32.0 GB   USB    SSD   boot medium │
 ╰───────────────────────────────────────────────────────────────────────────╯
 ╭ Method ──────────────────────────────────────────────────────────────────╮
 │ ‹ NIST 800-88 Clear ›   1/17   1 pass(es)    RECOMMENDED                   │
 │ Single verified zero overwrite. The modern industry baseline.             │
 │ verify [last pass]  rounds [1]  final blank [off]    1 write pass(es)      │
 ╰───────────────────────────────────────────────────────────────────────────╯
  up/dn move   spc select   </> method   v verify   +/- rounds   s START   q quit
```

The cursor row carries a `▌` bar; a `█` marks a disk that will be erased.
On a desktop terminal the UI is rendered in 24-bit color; on the bare Linux
console it falls back to a 16-color, ASCII-only theme automatically.

> ⚠️ **DBAN permanently destroys data. There is no undo.** Read
> [Safety design](#safety-design) before using it on real hardware.

---

## Why

The classic boot-and-nuke disk erasers are unmaintained, don't understand
NVMe, have dated UIs, and are written in C. DBAN is:

- **Written almost entirely in safe Rust.** The only `unsafe` blocks are a
  handful of audited, commented primitives: page-aligned buffers for
  `O_DIRECT`, the `getpid`/`reboot`/`sync` syscalls, and the Linux ioctl
  pass-through that issues ATA/NVMe firmware commands. Everything else — the
  overwrite engine, the algorithms, device discovery, the safety interlock, the
  whole UI — is safe Rust.
- **A purpose-built appliance.** On the ISO there is *no distro userland*: no
  shell, no BusyBox prompt, no package manager. The Linux kernel boots and
  launches the DBAN binary directly as PID 1. The entire userland is one
  program whose only jobs are showing hardware and wiping disks.
- **Fast to boot and light.** A single static musl binary in a minimal
  initramfs. No services to start.
- **Multi-threaded.** One worker thread per disk, so wiping four drives wipes
  them in parallel rather than in series.
- **Firmware-aware.** Beyond overwriting, DBAN can issue the drive's own purge
  commands — ATA Secure Erase and NVMe Format / Sanitize — the only software
  method that truly sanitizes flash. It probes each disk's capabilities
  (non-destructively) and only offers what the hardware supports.
- **Pretty.** A real TUI built on [ratatui](https://ratatui.rs): live per-disk
  progress bars, throughput, ETA, color-coded states, badges, and a 24-bit
  color palette — degrading automatically to a 16-color, ASCII-glyph theme on
  the bare Linux console. Every rendered glyph is exactly one column wide, so
  borders never drift regardless of terminal font (regression-tested).

### "Is this really an OS from scratch?"

Honest answer: **no, and that is the right call.** A from-scratch kernel would
have to reimplement AHCI, NVMe, USB-storage, SATA port multipliers, RAID/dm,
and filesystem detection before it could erase a single real disk — years of
work to *worse* hardware support than Linux gives you for free. So DBAN takes
the pragmatic "purpose-built OS" path: the Linux kernel for drivers, and
*nothing else* in userland but DBAN. The result boots in seconds and behaves
like a single-purpose appliance, which is what you actually want from a nuke
tool.

---

## Repository layout

```
crates/
  dban-core/      UI-free engine: overwrite algorithms, PRNG, firmware erase
                   (ATA/NVMe), device discovery, the wipe engine, the safety
                   interlock, reporting. Fully documented (deny(missing_docs)).
  dban/           ratatui TUI + the binary that runs as PID 1 on the ISO.
                   app.rs is a pure state machine; ui.rs renders it; theme.rs
                   holds the palette + glyphs.
    examples/dump.rs   Renders each screen to text for eyeballing.
    tests/             app_flow, firmware_flow, ui_integrity.
iso/
  Dockerfile       Two-stage build: static musl binary → hybrid ISO.
  build.sh         One command to produce dist/dban.iso (needs Docker).
  init             The shell shim that execs dban as PID 1.
  mkimage.sh       Assembles the BIOS+UEFI bootable image.
```

---

## Build & run

### Try the UI safely (no hardware touched)

DBAN ships a **simulation provider**: five realistic fake disks backed by
sparse temp files, throttled to believable speeds. It runs on Windows, macOS,
or Linux and never touches a real device.

```sh
cargo run -p dban -- --demo
```

Select a disk with `space`, pick a method with `<` / `>`, press `s`, and watch
a real (simulated) wipe with live progress. The demo SATA and NVMe disks
advertise firmware-erase support, so you can exercise that path too. Backing
files live under your temp dir in `dban-demo/`.

### Run the test suite

```sh
cargo test --all      # 53 tests: engine round-trips, PRNG, the safety gate,
                      # firmware capability + simulation, the full
                      # wipe-to-report flow, and UI render-integrity guards
                      # (every glyph one column wide; borders never drift)
cargo clippy --all-targets -- -D warnings
cargo doc --no-deps --workspace
```

### Build the bootable ISO

Requires Docker. Everything else happens inside the container.

```sh
./iso/build.sh        # → dist/dban.iso  (hybrid BIOS + UEFI)
```

Write it to a USB stick (this erases the stick):

```sh
# Linux
sudo dd if=dist/dban.iso of=/dev/sdX bs=4M conv=fsync status=progress
# Windows / macOS: use Rufus, balenaEtcher, or Ventoy.
```

Boot the target machine from it. DBAN comes up automatically.

---

## Sanitization methods

### Software overwrite (works on any disk)

| Method | Origin | Passes | Notes |
|---|---|---|---|
| **NIST 800-88 Clear** ★ | NIST SP 800-88 Rev.1 | 1 | Verified zero pass — the modern baseline |
| PRNG Stream | classic boot-and-nuke | 1+ | OS-seeded random, seed-verifiable |
| DoD 5220.22-M (E) | US DoD | 3 | zeros / ones / random |
| DoD 5220.22-M (ECE) | US DoD | 7 | E-sequence, random, E-sequence |
| Schneier | Applied Cryptography | 7 | ones, zeros, 5× random |
| Gutmann | Gutmann 1996 | 35 | The classic; overkill on modern drives |
| VSITR | German BSI | 7 | alternating, finishing 0xAA |
| RCMP TSSIT OPS-II | RCMP | 7 | alternating + verified random |
| HMG IS5 Enhanced | UK NCSC | 3 | zeros / ones / random |
| AFSSI-5020 | US Air Force | 3 | zeros / ones / random |
| Quick Zero Fill | — | 1 | fast unverified blank |

Each overwrite method can be repeated for N **rounds**, given an optional
**final blanking** zero pass, and **verified** (off / last pass / every pass).
Random passes are verified without buffering: the engine records each pass's
64-bit seed and regenerates the identical xoshiro256++ stream on read-back.

### Firmware purge (drive-internal; capability-gated)

| Method | Bus | What it does |
|---|---|---|
| ATA Secure Erase | SATA | `SECURITY ERASE UNIT` — erases all user + reallocated sectors |
| ATA Enhanced Secure Erase | SATA | adds a vendor pattern; rotates the media key on SEDs |
| NVMe Format (user) | NVMe | `Format NVM` with user-data erase (SES=1) |
| NVMe Format (crypto) | NVMe | `Format NVM` discarding the media key (SES=2) — instant |
| NVMe Sanitize (block) | NVMe | strongest NVMe purge across the whole subsystem |
| NVMe Sanitize (crypto) | NVMe | crypto-erase across the whole subsystem |

DBAN probes each disk's support **non-destructively** (ATA `IDENTIFY`, NVMe
Identify Controller) and only targets disks the chosen command can actually
run; others in the selection are shown as `skip (n/a)` and left untouched. On
real hardware these commands report no progress, so the UI shows an
indeterminate pulse; the report records exactly which command ran.

### A word on SSDs

Overwrite-based erasure was designed for magnetic media. Flash translation
layers (wear-leveling, over-provisioning) mean a software overwrite *cannot
reach every physical cell* on an SSD. DBAN warns whenever you select flash
media and points you at the firmware-purge methods above, which are the correct
NIST "Purge" for SSDs.

---

## Safety design

A boot-and-nuke tool that can be triggered carelessly is dangerous. DBAN is
built so that **no code path can start a wipe unattended**:

1. **No dangerous CLI surface.** There are no flags that select disks or start
   a job. The only way to wipe is through the interactive ceremony.
2. **A type-enforced interlock.** Both engine entry points — `spawn_wipe`
   (overwrite) and `spawn_firmware` (ATA/NVMe purge) — require an `ArmToken`.
   That token has a private constructor; the *only* way to mint one is to walk
   the full `SafetyGate` state machine to completion. You cannot call the
   destructive paths without it; the compiler enforces this.
3. **A selection-specific confirmation phrase.** You must type, exactly,
   `ERASE N DISKS` where N is your live selection count. The phrase changes
   with the selection so it can never become muscle memory.
4. **An abortable countdown.** After the phrase, a 5-second countdown runs
   during which *any key aborts*.
5. **Locked disks can never be selected.** Anything mounted, in use as swap,
   held by RAID/device-mapper, or detected as the DBAN boot medium itself is
   marked locked and rejected — both at selection time and again inside the
   engine.

Every disk's partition-ownership check is careful about the classic
`/dev/sda` vs `/dev/sdaa` and `/dev/nvme0n1` vs `/dev/nvme0n10` prefix traps
(see `linux::belongs_to` and its tests).

---

## Erasure reports

After a session, press `w` to write a JSON erasure report: tool version, host
hardware, and per-disk records (model, serial, method id/name, whether it was a
firmware erase, passes completed, status, duration, throughput, and any
verification failure with its byte offset). On the appliance it lands in
`/tmp`; hosted, in the working directory.

---

## Roadmap

Done:

- ✅ **ATA `SECURITY ERASE UNIT` / NVMe `Format` + `SANITIZE`** (true flash
  purge) — capability-probed and wired into the UI; see [Firmware
  purge](#firmware-purge-drive-internal-capability-gated).

Next, in priority order:

- Opal/TCG crypto-erase for self-encrypting drives.
- HPA/DCO detection and removal so hidden sectors are also cleared.
- Detached report signing (Ed25519) for tamper-evident compliance artifacts.
- ARM64 ISO target.

The firmware commands are written against the documented Linux ioctl ABIs
(`SG_IO` ATA pass-through, `NVME_IOCTL_ADMIN_CMD`) and compiled in CI; because
they can only be meaningfully exercised against real hardware, the full
end-to-end path is covered by the simulation provider in tests, and capability
probing is non-destructive.

---

## License

MIT. See [LICENSE](LICENSE) — including the safety notice. **There is no undo.**
