//! DBAN entry point.
//!
//! Two roles, chosen at runtime:
//! * **Hosted** (`dban` or `dban --demo` from a normal shell): runs in the
//!   existing terminal, restores it on exit, returns to the shell.
//! * **Appliance / init** (PID 1 on the live ISO): owns the machine. There is
//!   no shell to return to, so "quit" means reboot or power off, performed via
//!   the `reboot(2)` syscall after flushing disks.

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event};
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::crossterm::{cursor, execute};
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;

use dban::app::{App, ExitAction};
use dban::theme::Theme;
use dban::ui;

use dban_core::device::DiskProvider;

const TICK: Duration = Duration::from_millis(100);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let force_demo = args.iter().any(|a| a == "--demo");
    let force_real = args.iter().any(|a| a == "--real");
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return;
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("dban {}", dban_core::VERSION);
        return;
    }

    let pid1 = is_pid1();
    let (provider, sim) = choose_provider(force_demo, force_real, pid1);
    if let Err(e) = run(provider, pid1) {
        // Never leave the terminal in raw mode on the way out.
        let _ = restore_terminal();
        eprintln!("dban: fatal error: {e}");
        if sim {
            std::process::exit(1);
        }
        // As init, dropping dead would panic the kernel; hang visibly instead.
        if pid1 {
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        std::process::exit(1);
    }
}

fn print_usage() {
    println!(
        "dban {} — secure disk sanitization\n\n\
         USAGE:\n    dban [--demo | --real]\n\n\
         OPTIONS:\n\
         \x20   --demo      Force the simulation provider (safe temp-file disks).\n\
         \x20   --real      Force real hardware discovery (Linux, needs root).\n\
         \x20   --version   Print version and exit.\n\
         \x20   --help      Show this help.\n\n\
         With no flag, DBAN uses real hardware when run as root on Linux,\n\
         and the simulation provider otherwise.",
        dban_core::VERSION
    );
}

/// PID 1 on Linux is the init process — our appliance role.
fn is_pid1() -> bool {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: getpid is always safe.
        unsafe { libc::getpid() == 1 }
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

fn choose_provider(
    force_demo: bool,
    force_real: bool,
    pid1: bool,
) -> (Box<dyn DiskProvider>, bool) {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: geteuid is always safe.
        let is_root = unsafe { libc::geteuid() == 0 };
        let want_real = force_real || pid1 || (is_root && !force_demo);
        if want_real {
            return (Box::new(dban_core::linux::SysfsProvider::new()), false);
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (force_real, pid1);
    }
    let _ = force_demo;
    match dban_core::demo::DemoProvider::new() {
        Ok(p) => (Box::new(p), true),
        Err(e) => {
            eprintln!("dban: could not initialize simulation provider: {e}");
            std::process::exit(1);
        }
    }
}

fn run(provider: Box<dyn DiskProvider>, pid1: bool) -> io::Result<()> {
    let theme = Theme::detect();
    let mut terminal = setup_terminal()?;
    let mut app = App::new(provider, pid1);
    let mut last = Instant::now();

    let result = (|| -> io::Result<()> {
        loop {
            terminal.draw(|f| ui::draw(f, &app, &theme))?;

            // Block for input up to the next tick, so the UI animates even
            // when idle but doesn't spin the CPU.
            let timeout = TICK.saturating_sub(last.elapsed());
            if event::poll(timeout)? {
                match event::read()? {
                    Event::Key(key) if key.kind == event::KeyEventKind::Press => app.on_key(key),
                    Event::Resize(_, _) => { /* redrawn next loop */ }
                    _ => {}
                }
            }
            if last.elapsed() >= TICK {
                app.on_tick(last.elapsed());
                last = Instant::now();
            }
            if let Some(action) = app.exit {
                return finish(action, pid1);
            }
        }
    })();

    restore_terminal()?;
    result
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, cursor::Hide)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

fn restore_terminal() -> io::Result<()> {
    let mut stdout = io::stdout();
    execute!(stdout, LeaveAlternateScreen, cursor::Show)?;
    disable_raw_mode()
}

fn finish(action: ExitAction, pid1: bool) -> io::Result<()> {
    restore_terminal()?;
    if pid1 {
        // Flush every filesystem buffer before handing control to the kernel.
        sync_disks();
        match action {
            ExitAction::Reboot => power(Power::Reboot),
            ExitAction::PowerOff | ExitAction::Quit => power(Power::Off),
        }
    }
    Ok(())
}

#[allow(dead_code)]
enum Power {
    Reboot,
    Off,
}

fn sync_disks() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: sync takes no arguments and cannot fail.
        unsafe { libc::sync() };
    }
}

fn power(_mode: Power) -> ! {
    #[cfg(target_os = "linux")]
    {
        let cmd = match _mode {
            Power::Reboot => libc::LINUX_REBOOT_CMD_RESTART,
            Power::Off => libc::LINUX_REBOOT_CMD_POWER_OFF,
        };
        // SAFETY: documented reboot(2) usage; only reached as PID 1.
        unsafe {
            libc::sync();
            libc::reboot(cmd);
        }
    }
    // If the syscall returns (lacking privilege) or off-Linux, hang quietly
    // rather than panicking the kernel.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
