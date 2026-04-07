mod app;
mod arcstats;
mod meminfo;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;

const DEFAULT_SOURCE: &str = "/proc/spl/kstat/zfs/arcstats";

fn main() -> Result<()> {
    let (source, meminfo_source, interval) = parse_args();
    let mut app = match App::new(source.clone(), meminfo_source) {
        Ok(app) => app,
        Err(_) if source == PathBuf::from(DEFAULT_SOURCE) => {
            eprintln!("ztop: ZFS is not found on this system ({DEFAULT_SOURCE} does not exist)");
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
    println!("ztop {version} — a terminal dashboard for ZFS");
    println!();
    println!("USAGE:");
    println!("    ztop [OPTIONS]");
    println!();
    println!("OPTIONS:");
    println!("    -n, --interval <ms>     Polling interval in milliseconds [default: 1000]");
    println!("        --source <path>     Path to arcstats file [default: /proc/spl/kstat/zfs/arcstats]");
    println!("        --meminfo <path>    Path to meminfo file [default: /proc/meminfo]");
    println!("    -h, --help              Print this help message");
    println!("    -V, --version           Print version");
    println!();
    println!("CONTROLS:");
    println!("    q, Ctrl+C               Quit");
    println!("    r                        Force refresh");
    println!();
    println!("Copyright (c) 2026 Raphael Bitton. Licensed under MIT.");
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
                println!("ztop {}", env!("CARGO_PKG_VERSION"));
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
