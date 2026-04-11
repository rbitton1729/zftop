//! Overview tab — 3-section alarm summary. No new data; reuses the RAM bar
//! and ARC gauge widgets from `super::widgets`, plus a compact pools table.
//!
//! Layout:
//!
//!     ┌─ System RAM ─────────────────────┐  height 5
//!     │  ... bar + sidecars ...          │
//!     └──────────────────────────────────┘
//!     ┌─ ARC ────────────────────────────┐  height 4
//!     │  ... gauge + 1-line summary ...  │
//!     └──────────────────────────────────┘
//!     ┌─ Pools ──────────────────────────┐  Min 4
//!     │  name  health  capacity  scrub   │
//!     │  ...                             │
//!     └──────────────────────────────────┘

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use ratatui::Frame;

use super::widgets;
use crate::app::{format_bytes, App};
use crate::pools::ScrubState;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let [ram_area, arc_area, pools_area] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Min(4),
    ])
    .areas(area);

    // RAM section — uses the full area (RAM bar draws its own bordered block).
    widgets::draw_ram_bar(frame, ram_area, app);

    // ARC section — gauge + 1-line summary.
    draw_arc_section(frame, arc_area, app);

    // Pools section — bordered block wrapping a compact list.
    draw_pools_section(frame, pools_area, app);
}

fn draw_arc_section(frame: &mut Frame, area: Rect, app: &App) {
    // Gauge takes 3 rows (title + filled bar + bottom border); summary is 1.
    let [gauge_area, summary_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(area);
    widgets::draw_arc_gauge(frame, gauge_area, app);

    let overall = app.hit_ratio_overall();
    let demand = app.hit_ratio_demand();
    let hits = app
        .throughput_hits()
        .map(|h| format!("{h}/s"))
        .unwrap_or_else(|| "—".to_string());
    let summary = Paragraph::new(Line::from(vec![
        Span::raw(format!(
            "Overall {overall:.1}%    Demand {demand:.1}%    Hits {hits}"
        )),
    ]))
    .alignment(Alignment::Center);
    frame.render_widget(summary, summary_area);
}

fn draw_pools_section(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title("Pools");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // State dispatch: libzfs init failed, libzfs ok but zero pools, or populated.
    if let Some(err) = &app.pools_init_error {
        draw_centered(
            frame,
            inner,
            &format!("libzfs unavailable: {err}"),
            Style::default().fg(Color::Red),
        );
        return;
    }
    if app.pools_snapshot.is_empty() {
        draw_centered(
            frame,
            inner,
            "(no pools imported)",
            Style::default().fg(Color::DarkGray),
        );
        return;
    }

    // Four-column compact list: name / health / capacity / scrub.
    let rows: Vec<Row> = app
        .pools_snapshot
        .iter()
        .map(|p| {
            let health_label = pool_health_label(p.health);
            let capacity = format!(
                "{}/{} ({:.0}%)",
                format_bytes(p.allocated_bytes),
                format_bytes(p.size_bytes),
                p.capacity_fraction() * 100.0
            );
            let scrub = format_scrub_compact(&p.scrub);
            Row::new(vec![
                Span::raw(p.name.clone()),
                Span::styled(health_label, widgets::pool_health_style(p.health)),
                Span::raw(capacity),
                Span::raw(scrub),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(16),
        Constraint::Length(10),
        Constraint::Length(24),
        Constraint::Min(8),
    ];

    let header = Row::new(vec![
        Span::styled("NAME", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("HEALTH", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("CAPACITY", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled("SCRUB", Style::default().add_modifier(Modifier::BOLD)),
    ]);

    let table = Table::new(rows, widths).header(header);
    frame.render_widget(table, inner);
}

fn draw_centered(frame: &mut Frame, area: Rect, text: &str, style: Style) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let mid_y = area.y + area.height / 2;
    let row = Rect {
        x: area.x,
        y: mid_y,
        width: area.width,
        height: 1,
    };
    let p = Paragraph::new(Line::from(Span::styled(text.to_string(), style)))
        .alignment(Alignment::Center);
    frame.render_widget(p, row);
}

fn pool_health_label(health: crate::pools::PoolHealth) -> String {
    use crate::pools::PoolHealth;
    match health {
        PoolHealth::Online => "ONLINE",
        PoolHealth::Degraded => "DEGRADED",
        PoolHealth::Faulted => "FAULTED",
        PoolHealth::Offline => "OFFLINE",
        PoolHealth::Removed => "REMOVED",
        PoolHealth::Unavail => "UNAVAIL",
    }
    .to_string()
}

fn format_scrub_compact(scrub: &ScrubState) -> String {
    match scrub {
        ScrubState::Never => "—".into(),
        ScrubState::Error => "error".into(),
        ScrubState::InProgress {
            progress_pct,
            is_resilver,
            ..
        } => {
            if *is_resilver {
                format!("resilver {progress_pct}%")
            } else {
                format!("scrub {progress_pct}%")
            }
        }
        ScrubState::Finished { completed_at, .. } => format_relative_time(*completed_at),
    }
}

fn format_relative_time(when: std::time::SystemTime) -> String {
    match when.elapsed() {
        Ok(dur) => {
            let days = dur.as_secs() / 86_400;
            if days == 0 {
                "today".into()
            } else if days < 30 {
                format!("{days}d ago")
            } else if days < 365 {
                format!("{} mo ago", days / 30)
            } else {
                format!("{}y ago", days / 365)
            }
        }
        Err(_) => "—".into(),
    }
}

#[cfg(test)]
mod tests {
    use crate::app::{App, Tab};
    use crate::arcstats;
    use crate::meminfo::{self, MemSource};
    use crate::pools::fake::FakePoolsSource;
    use crate::pools::{
        ErrorCounts, PoolHealth, PoolInfo, ScrubState, VdevKind, VdevNode, VdevState,
    };
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    fn test_pool(name: &str, health: PoolHealth, size: u64, alloc: u64) -> PoolInfo {
        PoolInfo {
            name: name.into(),
            health,
            allocated_bytes: alloc,
            size_bytes: size,
            free_bytes: size - alloc,
            fragmentation_pct: Some(10),
            scrub: ScrubState::Never,
            errors: ErrorCounts::default(),
            root_vdev: VdevNode {
                name: name.into(),
                kind: VdevKind::Root,
                state: VdevState::Online,
                size_bytes: Some(size),
                errors: ErrorCounts::default(),
                children: vec![],
            },
        }
    }

    fn app_for_overview(
        pools: Vec<PoolInfo>,
        pools_source: Option<Box<dyn crate::pools::PoolsSource>>,
        pools_init_error: Option<String>,
    ) -> App {
        let arcstats_path = PathBuf::from("fixtures/arcstats");
        let meminfo_path = PathBuf::from("fixtures/meminfo");
        let arc_reader: Box<dyn FnMut() -> anyhow::Result<arcstats::ArcStats>> = {
            let p = arcstats_path.clone();
            Box::new(move || arcstats::linux::from_procfs_path(&p))
        };
        let mem: Option<Box<dyn MemSource>> = Some(Box::new(
            meminfo::linux::LinuxMemSource::new(meminfo_path),
        ));
        let mut app =
            App::new(arc_reader, mem, pools_source, pools_init_error).expect("fixture App::new");
        app.current_tab = Tab::Overview;
        app.pools_snapshot = pools;
        app
    }

    fn whole_text(backend: &TestBackend) -> String {
        let buf = backend.buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_overview(app: &App) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| super::super::draw(frame, app))
            .expect("draw");
        whole_text(terminal.backend())
    }

    #[test]
    fn overview_shows_system_ram_and_arc_and_pools_blocks() {
        let pools = vec![
            test_pool("tank", PoolHealth::Online, 1_000_000_000, 500_000_000),
        ];
        let app = app_for_overview(
            pools.clone(),
            Some(Box::new(FakePoolsSource::new(pools))),
            None,
        );
        let out = render_overview(&app);
        assert!(out.contains("System RAM"), "missing System RAM block");
        assert!(out.contains("ARC"), "missing ARC label");
        assert!(out.contains("Pools"), "missing Pools block");
        assert!(out.contains("tank"), "missing pool name");
        assert!(out.contains("ONLINE"), "missing health label");
    }

    #[test]
    fn overview_shows_libzfs_unavailable_on_init_error() {
        let app = app_for_overview(vec![], None, Some("init failed".to_string()));
        let out = render_overview(&app);
        assert!(
            out.contains("libzfs unavailable"),
            "missing libzfs-unavailable notice"
        );
    }

    #[test]
    fn overview_shows_no_pools_when_snapshot_empty() {
        let app = app_for_overview(
            vec![],
            Some(Box::new(FakePoolsSource::new(vec![]))),
            None,
        );
        let out = render_overview(&app);
        assert!(
            out.contains("no pools imported"),
            "missing no-pools notice: {out}"
        );
    }

    #[test]
    fn overview_renders_degraded_pool_health_label() {
        let pools = vec![
            test_pool("tank", PoolHealth::Online, 1_000, 500),
            test_pool("scratch", PoolHealth::Degraded, 1_000, 500),
        ];
        let app = app_for_overview(
            pools.clone(),
            Some(Box::new(FakePoolsSource::new(pools))),
            None,
        );
        let out = render_overview(&app);
        assert!(out.contains("DEGRADED"), "missing DEGRADED label");
    }
}
