# Scour

**A modern, Rust, boot-and-nuke disk eraser.** Scour is a spiritual successor
to DBAN: a tiny bootable image that brings up a clean terminal UI, shows you
the hardware and disks in the machine, and securely overwrites the ones you
explicitly choose — using every major published sanitization standard.

```
 SCOUR  secure disk sanitization                         8c · 32 GB · Linux 6.6
────────────────────────────────────────────────────────────────────────────
 ╭ Disks ───────────────────────────────────────────────────────────────────╮
 │     DEVICE      MODEL                        SIZE   BUS   TYPE  STATE       │
 │ ▶ [*] nvme0n1   Samsung SSD 980 PRO        1.0 TB  NVMe  SSD   WILL ERASE  │
 │   [ ] sda       Seagate Barracuda ST2000   2.0 TB  SATA  HDD   ready       │
 │    x  sdb       SCOUR boot medium         32.0 GB  USB   SSD   boot medium │
 ╰────────────────────────────────────────────────────────────────────────────╯
 ╭ Method ──────────────────────────────────────────────────────────────────╮
 │ ◀ NIST 800-88 Clear ▶   1 pass(es)   ★ recommended                        │
 │ Single verified zero overwrite. The modern industry baseline.            │
 │ verify [last pass]   rounds [1]   final blank [off]   → 1 total pass      │
 ╰────────────────────────────────────────────────────────────────────────────╯
  ↑↓ move   space select   ◀▶ method   v verify   +/- rounds   s START   q quit
```

> ⚠️ **Scour permanently destroys data. There is no undo.** Read
> [Safety design](#safety-design) before using it on real hardware.

---

## Why another DBAN

DBAN is unmaintained, doesn't understand NVMe, has a dated UI, and its codebase
is C. Scour is:

- **Written entirely in safe Rust.** The only `unsafe` blocks are a handful of
  audited, commented FFI/allocation primitives (aligned buffers for `O_DIRECT`,
  `getpid`/`reboot` syscalls). Everything else — the engine, the algorithms,
  the safety interlock, the UI — is safe Rust.
- **A purpose-built appliance.** On the ISO there is *no distro userland*: no
  shell, no BusyBox prompt, no package manager. The Linux kernel boots and
  launches the Scour binary directly as PID 1. The entire userland is one
  program whose only jobs are showing hardware and wiping disks.
- **Fast to boot and light.** A single static musl binary in a minimal
  initramfs. No services to start.
- **Multi-threaded.** One worker thread per disk, so wiping four drives wipes
  them in parallel rather than in series.
- **Pretty.** A real TUI built on [ratatui](https://ratatui.rs): live
  per-disk progress bars, throughput, ETA, color-coded states — degrading
  gracefully to a 16-color, ASCII-glyph mode on the bare Linux console.

### "Is this really an OS from scratch?"

Honest answer: **no, and that's the right call.** A from-scratch kernel would
have to reimplement AHCI, NVMe, USB-storage, SATA port multipliers, RAID/dm,
and filesystem detection before it could erase a single real disk — years of
work to *worse* hardware support than Linux gives you for free. So Scour takes
the pragmatic "purpose-built OS" path: the Linux kernel for drivers, and
*nothing else* in userland but Scour. The result boots in seconds and behaves
like a single-purpose appliance, which is what you actually want from a nuke
tool.

---

## Repository layout

```
crates/
  scour-core/      UI-free engine: algorithms, PRNG, device discovery,
                   the wipe engine, the safety interlock, reporting.
  scour/           ratatui TUI + the binary that runs as PID 1 on the ISO.
iso/
  Dockerfile       Two-stage build: static musl binary → hybrid ISO.
  build.sh         One command to produce dist/scour.iso (needs Docker).
  init             The 12-line shell shim that execs scour as PID 1.
  mkimage.sh       Assembles the BIOS+UEFI bootable image.
```

---

## Build & run

### Try the UI safely (no hardware touched)

Scour ships a **simulation provider**: five realistic fake disks backed by
sparse temp files, throttled to believable speeds. It runs on Windows, macOS,
or Linux and never touches a real device.

```sh
cargo run -p scour -- --demo
```

Select a disk with `space`, pick a method with `◀ ▶`, press `s`, and watch a
real (simulated) multi-pass wipe with live progress. Backing files live under
your temp dir in `scour-demo/`.

### Run the test suite

```sh
cargo test            # 39 tests: engine round-trips, PRNG, safety gate,
                      # UI render smoke tests, full wipe-to-report flow
cargo clippy --all-targets
```

### Build the bootable ISO

Requires Docker. Everything else happens inside the container.

```sh
./iso/build.sh        # → dist/scour.iso  (hybrid BIOS + UEFI)
```

Write it to a USB stick (this erases the stick):

```sh
# Linux
sudo dd if=dist/scour.iso of=/dev/sdX bs=4M conv=fsync status=progress
# Windows / macOS: use Rufus, balenaEtcher, or Ventoy.
```

Boot the target machine from it. Scour comes up automatically.

---

## Sanitization methods

| Method | Origin | Passes | Notes |
|---|---|---|---|
| **NIST 800-88 Clear** ★ | NIST SP 800-88 Rev.1 | 1 | Verified zero pass — the modern baseline |
| PRNG Stream | DBAN heritage | 1+ | OS-seeded random, seed-verifiable |
| DoD 5220.22-M (E) | US DoD | 3 | zeros / ones / random |
| DoD 5220.22-M (ECE) | US DoD | 7 | E-sequence, random, E-sequence |
| Schneier | Applied Cryptography | 7 | ones, zeros, 5× random |
| Gutmann | Gutmann 1996 | 35 | The classic; overkill on modern drives |
| VSITR | German BSI | 7 | alternating, finishing 0xAA |
| RCMP TSSIT OPS-II | RCMP | 7 | alternating + verified random |
| HMG IS5 Enhanced | UK NCSC | 3 | zeros / ones / random |
| AFSSI-5020 | US Air Force | 3 | zeros / ones / random |
| Quick Zero Fill | — | 1 | fast unverified blank |

Each method can be repeated for N **rounds**, given an optional **final
blanking** zero pass, and **verified** (off / last pass / every pass). Random
passes are verified without buffering: the engine records each pass's 64-bit
seed and regenerates the identical xoshiro256++ stream on read-back.

### A word on SSDs

Overwrite-based erasure was designed for magnetic media. Flash translation
layers (wear-leveling, over-provisioning) mean a software overwrite *cannot
reach every physical cell* on an SSD. Scour shows a warning whenever you select
flash media. For a true NIST "Purge" on SSDs, the firmware `SANITIZE` /
crypto-erase commands are the right tool — a documented item on the roadmap
below.

---

## Safety design

A boot-and-nuke tool that can be triggered carelessly is dangerous. Scour is
built so that **no code path can start a wipe unattended**:

1. **No dangerous CLI surface.** There are no flags that select disks or start
   a job. The only way to wipe is through the interactive ceremony.
2. **A type-enforced interlock.** The engine's `spawn_wipe` requires an
   `ArmToken`. That token has a private constructor — the *only* way to mint
   one is to walk the full `SafetyGate` state machine to completion. You cannot
   call the wipe engine without it; the compiler enforces this.
3. **A selection-specific confirmation phrase.** You must type, exactly,
   `ERASE N DISKS` where N is your live selection count. The phrase changes
   with the selection so it can never become muscle memory.
4. **An abortable countdown.** After the phrase, a 5-second countdown runs
   during which *any key aborts*.
5. **Locked disks can never be selected.** Anything mounted, in use as swap,
   held by RAID/device-mapper, or detected as the Scour boot medium itself is
   marked locked and rejected — both at selection time and again inside the
   engine.

Every disk's partition-ownership check is careful about the classic
`/dev/sda` vs `/dev/sdaa` and `/dev/nvme0n1` vs `/dev/nvme0n10` prefix traps
(see `linux::belongs_to` and its tests).

---

## Erasure reports

After a session, press `w` to write a JSON erasure report: tool version, host
hardware, and per-disk records (model, serial, method, passes completed,
status, duration, throughput, and any verification failure with its byte
offset). On the appliance it lands in `/tmp`; hosted, in the working directory.

---

## Roadmap

These are deliberately *not* faked in the current UI — they require issuing
firmware commands and are tracked as real future work:

- ATA `SECURITY ERASE UNIT` / NVMe `Format` + `SANITIZE` (true flash purge).
- Opal/TCG crypto-erase for self-encrypting drives.
- HPA/DCO detection and removal so hidden sectors are also cleared.
- Detached report signing (Ed25519) for tamper-evident compliance artifacts.
- ARM64 ISO target.

---

## License

MIT. See [LICENSE](LICENSE) — including the safety notice. **There is no undo.**
