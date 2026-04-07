mod app;
mod arcstats;
mod meminfo;
mod ui;

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use app::App;

const DEFAULT_SOURCE: &str = "/proc/spl/kstat/zfs/arcstats";

fn main() -> Result<()> {
    let (source, meminfo_source) = parse_args();
    let mut app = App::new(source.clone(), meminfo_source)
        .with_context(|| format!("failed to read {}", source.display()))?;

    // Set up terminal
    terminal::enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let result = run(&mut terminal, &mut app);

    // Restore terminal no matter what
    terminal::disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;

    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(Duration::from_secs(1))? {
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

fn parse_args() -> (PathBuf, Option<PathBuf>) {
    let args: Vec<String> = std::env::args().collect();
    let mut source = PathBuf::from(DEFAULT_SOURCE);
    let mut meminfo_source = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
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
            _ => {}
        }
        i += 1;
    }
    (source, meminfo_source)
}
