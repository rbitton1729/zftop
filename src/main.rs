mod app;
mod arcstats;
mod meminfo;
mod ui;

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;
use arcstats::ArcStats;
use meminfo::MemSource;

const DEFAULT_SOURCE: &str = "/proc/spl/kstat/zfs/arcstats";

fn main() -> Result<()> {
    let (source, meminfo_source, interval) = parse_args();
    let (arc_reader, mem_source) = build_sources(source.clone(), meminfo_source);

    let mut app = match App::new(arc_reader, mem_source) {
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

    // Set up terminal
    terminal::enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, &mut app, interval);

    // Restore terminal no matter what
    terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    result
}

#[cfg(target_os = "linux")]
fn build_sources(
    source: PathBuf,
    meminfo_source: Option<PathBuf>,
) -> (
    Box<dyn FnMut() -> Result<ArcStats>>,
    Option<Box<dyn MemSource>>,
) {
    let arc_reader: Box<dyn FnMut() -> Result<ArcStats>> =
        Box::new(move || arcstats::linux::from_procfs_path(&source));
    let meminfo_path = meminfo_source.unwrap_or_else(|| PathBuf::from("/proc/meminfo"));
    let mem: Option<Box<dyn MemSource>> =
        Some(Box::new(meminfo::linux::LinuxMemSource::new(meminfo_path)));
    (arc_reader, mem)
}

#[cfg(target_os = "freebsd")]
fn build_sources(
    source: PathBuf,
    meminfo_source: Option<PathBuf>,
) -> (
    Box<dyn FnMut() -> Result<ArcStats>>,
    Option<Box<dyn MemSource>>,
) {
    if source != PathBuf::from(DEFAULT_SOURCE) || meminfo_source.is_some() {
        eprintln!("zftop: --source/--meminfo are Linux-only and ignored on FreeBSD");
    }
    let arc_reader: Box<dyn FnMut() -> Result<ArcStats>> =
        Box::new(|| arcstats::freebsd::from_sysctl());
    let mem: Option<Box<dyn MemSource>> = meminfo::freebsd::FreeBsdMemSource::new()
        .ok()
        .map(|s| Box::new(s) as Box<dyn MemSource>);
    (arc_reader, mem)
}

#[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
fn build_sources(
    _source: PathBuf,
    _meminfo_source: Option<PathBuf>,
) -> (
    Box<dyn FnMut() -> Result<ArcStats>>,
    Option<Box<dyn MemSource>>,
) {
    let arc_reader: Box<dyn FnMut() -> Result<ArcStats>> =
        Box::new(|| Err(anyhow::anyhow!("zftop only supports Linux and FreeBSD")));
    (arc_reader, None)
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

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App, interval: Duration) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(interval)? {
            if let Event::Key(key) = event::read()? {
                app.on_key(key);
            }
        } else {
            // Timeout — refresh data
            app.refresh().ok();
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

fn print_help() {
    let version = env!("CARGO_PKG_VERSION");
    println!("zftop {version} — a terminal dashboard for ZFS");
    println!();
    println!("USAGE:");
    println!("    zftop [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("    -n, --interval <ms>     Polling interval in milliseconds [default: 1000]");
    println!("        --source <path>     Path to arcstats file [Linux only; default: /proc/spl/kstat/zfs/arcstats]");
    println!("        --meminfo <path>    Path to meminfo file [Linux only; default: /proc/meminfo]");
    println!("    -h, --help              Print this help message");
    println!("    -V, --version           Print version");
    println!();
    println!("CONTROLS:");
    println!("    q, Ctrl+C               Quit");
    println!("    r                        Force refresh");
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
