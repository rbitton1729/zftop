mod app;
mod arcstats;
mod datasets;
mod meminfo;
mod pools;
mod ui;

use std::io;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::cursor::MoveTo;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers,
};
use crossterm::terminal;
use crossterm::ExecutableCommand;
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::Terminal;

use app::App;
use arcstats::ArcStats;
use meminfo::MemSource;
use pools::PoolsSource;

const DEFAULT_SOURCE: &str = "/proc/spl/kstat/zfs/arcstats";

fn main() -> Result<()> {
    let (source, meminfo_source, interval) = parse_args();
    let (arc_reader, mem_source, pools_source, pools_init_error) =
        build_sources(source.clone(), meminfo_source);

    let mut app = match App::new(
        arc_reader,
        mem_source,
        pools_source,
        pools_init_error,
        None, // datasets source — Task 14 wires the real one
        None, // datasets init error
    ) {
        Ok(app) => app,
        Err(e) if is_default_source(&source) => {
            eprintln!("zftop: ZFS is not found on this system");
            #[cfg(target_os = "linux")]
            eprintln!("  ({DEFAULT_SOURCE} does not exist)");
            #[cfg(target_os = "freebsd")]
            eprintln!("  (kstat.zfs.misc.arcstats sysctls are unavailable)");
            let _ = e;
            std::process::exit(1);
        }
        Err(e) => return Err(e.context(format!("failed to read {}", source.display()))),
    };

    // Set up terminal. We deliberately stay on the main screen (no
    // alternate-screen buffer) so the last rendered frame remains in
    // scrollback after `zftop` exits, the same way `top -n1` or `less`
    // leave their output visible.
    terminal::enable_raw_mode()?;
    io::stdout().execute(EnableMouseCapture)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    // Clear the screen at startup so zftop starts on a clean slate,
    // but we do NOT clear on exit — the last frame stays in scrollback.
    terminal.clear()?;

    // On Unix, catch SIGTSTP so we can cleanly leave raw mode before the
    // process is stopped. The terminal driver is in raw mode (ISIG off),
    // so Ctrl+Z from the keyboard arrives as a key event rather than a
    // signal — we also handle that path in `run`.
    #[cfg(unix)]
    let suspend_flag = {
        let flag = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(signal_hook::consts::SIGTSTP, Arc::clone(&flag))?;
        flag
    };

    #[cfg(unix)]
    let result = run(&mut terminal, &mut app, interval, suspend_flag);
    #[cfg(not(unix))]
    let result = run(&mut terminal, &mut app, interval);

    // Restore the terminal. Since we never left the main screen, the last
    // frame is already on screen — we just need to drop out of raw mode
    // and park the cursor below the drawn content so the shell prompt
    // starts on a fresh line.
    io::stdout().execute(DisableMouseCapture)?;
    terminal::disable_raw_mode()?;
    let size = terminal.backend().size().unwrap_or_default();
    io::stdout().execute(MoveTo(0, size.height.saturating_sub(1)))?;
    println!();

    result
}

/// Suspend the process like a normal job-control stop: leave raw mode so
/// the user gets their shell back, then raise SIGSTOP (which cannot be
/// caught). When the shell resumes us with `fg` (SIGCONT), we re-enable
/// raw mode and force a full redraw so the frame repaints from scratch.
#[cfg(unix)]
fn suspend_process(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    io::stdout().execute(DisableMouseCapture)?;
    terminal::disable_raw_mode()?;
    signal_hook::low_level::raise(signal_hook::consts::SIGSTOP)?;
    terminal::enable_raw_mode()?;
    io::stdout().execute(EnableMouseCapture)?;
    terminal.clear()?;
    Ok(())
}

type BuildSourcesResult = (
    Box<dyn FnMut() -> Result<ArcStats>>,
    Option<Box<dyn MemSource>>,
    Option<Box<dyn PoolsSource>>,
    Option<String>,
);

#[cfg(target_os = "linux")]
fn build_sources(source: PathBuf, meminfo_source: Option<PathBuf>) -> BuildSourcesResult {
    let arc_reader: Box<dyn FnMut() -> Result<ArcStats>> =
        Box::new(move || arcstats::linux::from_procfs_path(&source));
    let meminfo_path = meminfo_source.unwrap_or_else(|| PathBuf::from("/proc/meminfo"));
    let mem: Option<Box<dyn MemSource>> =
        Some(Box::new(meminfo::linux::LinuxMemSource::new(meminfo_path)));
    let (pools, pools_init_error) = build_pools_source();
    (arc_reader, mem, pools, pools_init_error)
}

#[cfg(target_os = "freebsd")]
fn build_sources(source: PathBuf, meminfo_source: Option<PathBuf>) -> BuildSourcesResult {
    if source != PathBuf::from(DEFAULT_SOURCE) || meminfo_source.is_some() {
        eprintln!("zftop: --source/--meminfo are Linux-only and ignored on FreeBSD");
    }
    let arc_reader: Box<dyn FnMut() -> Result<ArcStats>> =
        Box::new(|| arcstats::freebsd::from_sysctl());
    let mem: Option<Box<dyn MemSource>> = meminfo::freebsd::FreeBsdMemSource::new()
        .ok()
        .map(|s| Box::new(s) as Box<dyn MemSource>);
    let (pools, pools_init_error) = build_pools_source();
    (arc_reader, mem, pools, pools_init_error)
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn build_sources(_source: PathBuf, _meminfo_source: Option<PathBuf>) -> BuildSourcesResult {
    let arc_reader: Box<dyn FnMut() -> Result<ArcStats>> =
        Box::new(|| Err(anyhow::anyhow!("zftop only supports Linux and FreeBSD")));
    (arc_reader, None, None, None)
}

/// Attempt to construct a `LibzfsPoolsSource`. On failure (`libzfs_init`
/// returns null — typically "/dev/zfs not accessible"), returns
/// `(None, Some(error))` so `App` can render the "libzfs unavailable"
/// fallback without crashing.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn build_pools_source() -> (Option<Box<dyn PoolsSource>>, Option<String>) {
    match pools::libzfs::LibzfsPoolsSource::new() {
        Ok(src) => (Some(Box::new(src) as Box<dyn PoolsSource>), None),
        Err(e) => (None, Some(e.to_string())),
    }
}

fn is_default_source(source: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        source == Path::new(DEFAULT_SOURCE)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = source;
        true
    }
}

#[cfg(unix)]
fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    interval: Duration,
    suspend_flag: Arc<AtomicBool>,
) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        // Check whether we were asked to suspend via SIGTSTP since last tick.
        if suspend_flag.swap(false, Ordering::Relaxed) {
            suspend_process(terminal)?;
            continue;
        }

        // `event::poll` can return `ErrorKind::Interrupted` if a signal arrives
        // while we're waiting — treat that as a normal tick so we fall through
        // and re-check the suspend flag.
        match event::poll(interval) {
            Ok(true) => match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('z')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        suspend_process(terminal)?;
                        continue;
                    }
                    app.on_key(key);
                }
                Event::Mouse(mouse) => app.on_mouse(mouse),
                _ => {}
            },
            Ok(false) => {
                app.refresh().ok();
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

#[cfg(not(unix))]
fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    interval: Duration,
) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(interval)? {
            match event::read()? {
                Event::Key(key) => app.on_key(key),
                Event::Mouse(mouse) => app.on_mouse(mouse),
                _ => {}
            }
        } else {
            app.refresh().ok();
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

fn print_help() {
    let version = env!("CARGO_PKG_VERSION");
    println!("zftop {version} — a terminal dashboard for the Zettabyte File System");
    println!();
    println!("USAGE:");
    println!("    zftop [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("    -n, --interval <ms>     Polling interval in milliseconds [default: 1000]");
    println!("        --source <path>     Path to arcstats file [Linux only; default: /proc/spl/kstat/zfs/arcstats]");
    println!("        --meminfo <path>    Path to meminfo file [Linux only; default: /proc/meminfo]");
    println!("    -U, --upgrade           Print the command to upgrade to the latest release");
    println!("    -h, --help              Print this help message");
    println!("    -V, --version           Print version");
    println!();
    println!("CONTROLS:");
    println!("    q, Ctrl+C               Quit");
    println!("    r                       Force refresh");
    println!("    1, 2, 3                 Switch tab (Overview / Pools / ARC)");
    println!("    Tab, Shift+Tab          Cycle tabs forward / back");
    println!("    (Pools list)");
    println!("        ↑/↓, j/k            Select pool");
    println!("        Home, End           Jump to first / last");
    println!("        Enter               Drill into pool detail");
    println!("    (Pools detail)");
    println!("        Esc, Backspace      Return to list");
    println!();
    println!("On FreeBSD, --source and --meminfo are ignored; data is read via sysctl.");
    println!();
    println!("Copyright (c) 2026 Raphael Bitton. Licensed under GPLv3 or later.");
}

fn parse_args() -> (PathBuf, Option<PathBuf>, Duration) {
    let args: Vec<String> = std::env::args().collect();
    let mut source = PathBuf::from(DEFAULT_SOURCE);
    let mut meminfo_source = None;
    let mut interval = Duration::from_secs(1);
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("zftop {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-U" | "--upgrade" => {
                let url = "https://git.skylantix.com/rbitton/zftop/-/raw/main/install.sh";
                println!("To upgrade zftop, run:");
                println!();
                println!("  curl -fsSL {url} | sh");
                println!();
                println!("To pass options, download the script first:");
                println!();
                println!("  curl -fsSL {url} -o install.sh");
                println!("  less install.sh                        # inspect before running");
                println!("  sh install.sh --version 0.3.0          # pin a specific release");
                println!("  sh install.sh --dir ~/.local/bin       # custom install directory");
                println!("  sh install.sh --force                  # skip the ZFS-not-detected prompt");
                std::process::exit(0);
            }
            "--source" => {
                if let Some(path) = args.get(i + 1) {
                    source = PathBuf::from(path);
                    i += 1;
                }
            }
            "--meminfo" => {
                if let Some(path) = args.get(i + 1) {
                    meminfo_source = Some(PathBuf::from(path));
                    i += 1;
                }
            }
            "--interval" | "-n" => {
                if let Some(val) = args.get(i + 1) {
                    if let Ok(ms) = val.parse::<u64>() {
                        interval = Duration::from_millis(ms);
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    (source, meminfo_source, interval)
}
