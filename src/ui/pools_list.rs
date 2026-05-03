//! Pools tab — list view. Full-width table of all imported pools with a
//! moving selection marker. Wide layout (≥100 cols) shows 6 columns; narrow
//! layout drops FRAG and ERR.

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Row, Table};
use ratatui::Frame;

use super::widgets;
use crate::app::{format_bytes, App, PoolsView};
use crate::pools::{PoolInfo, ScrubState};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title("Pools");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Empty / error states.
    if let Some(err) = &app.pools_init_error {
        widgets::draw_centered(
            frame,
            inner,
            &format!("libzfs unavailable: {err}"),
            Style::default().fg(Color::Red),
        );
        return;
    }
    if app.pools_snapshot.is_empty() {
        widgets::draw_centered(
            frame,
            inner,
            "(no pools imported)",
            Style::default().fg(Color::DarkGray),
        );
        return;
    }

    let wide = inner.width >= 100;
    let selected_idx = match app.pools_view {
        PoolsView::List { selected } => selected,
        PoolsView::Detail { pool_index } => pool_index, // keep the row highlighted while in detail
    };

    let rows: Vec<Row> = app
        .pools_snapshot
        .iter()
        .enumerate()
        .map(|(i, p)| build_row(p, i == selected_idx, wide))
        .collect();

    let header = build_header(wide);

    if wide {
        let widths = [
            Constraint::Length(2),   // marker
            Constraint::Length(14),  // name
            Constraint::Length(10),  // health
            Constraint::Length(8),   // type
            Constraint::Length(28),  // capacity + minibar
            Constraint::Length(6),   // frag
            Constraint::Length(16),  // scrub
            Constraint::Length(6),   // err
        ];
        let table = Table::new(rows, widths).header(header);
        frame.render_widget(table, inner);
    } else {
        let widths = [
            Constraint::Length(2),   // marker
            Constraint::Length(14),  // name
            Constraint::Length(10),  // health
            Constraint::Length(8),   // type
            Constraint::Length(28),  // capacity + minibar
            Constraint::Length(16),  // scrub
        ];
        let table = Table::new(rows, widths).header(header);
        frame.render_widget(table, inner);
    }
}

fn build_header(wide: bool) -> Row<'static> {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    if wide {
        Row::new(vec![
            Span::raw(""),
            Span::styled("NAME", bold),
            Span::styled("HEALTH", bold),
            Span::styled("TYPE", bold),
            Span::styled("CAPACITY", bold),
            Span::styled("FRAG", bold),
            Span::styled("SCRUB", bold),
            Span::styled("ERR", bold),
        ])
    } else {
        Row::new(vec![
            Span::raw(""),
            Span::styled("NAME", bold),
            Span::styled("HEALTH", bold),
            Span::styled("TYPE", bold),
            Span::styled("CAPACITY", bold),
            Span::styled("SCRUB", bold),
        ])
    }
}

fn build_row(p: &PoolInfo, is_selected: bool, wide: bool) -> Row<'static> {
    let marker = if is_selected { ">" } else { " " }.to_string();
    let name = p.name.clone();
    let health_label = pool_health_label(p.health);
    let raid_type = p.raid_label();
    let capacity = render_capacity_cell(p);
    let scrub = render_scrub_cell(&p.scrub);

    let health_cell = Span::styled(health_label, widgets::pool_health_style(p.health));

    let row = if wide {
        let frag = match p.fragmentation_pct {
            Some(v) => format!("{v}%"),
            None => "—".into(),
        };
        let err_sum = p.root_vdev.total_errors();
        let err_style = if err_sum == 0 {
            Style::default()
        } else {
            Style::default().fg(Color::Red)
        };
        Row::new(vec![
            Span::raw(marker),
            Span::raw(name),
            health_cell,
            Span::raw(raid_type),
            Span::raw(capacity),
            Span::raw(frag),
            Span::raw(scrub),
            Span::styled(format!("{err_sum}"), err_style),
        ])
    } else {
        Row::new(vec![
            Span::raw(marker),
            Span::raw(name),
            health_cell,
            Span::raw(raid_type),
            Span::raw(capacity),
            Span::raw(scrub),
        ])
    };

    if is_selected {
        // Dark-gray background + bold instead of `Modifier::REVERSED`.
        // REVERSED flips fg and bg on every glyph, which mangles the
        // capacity minibar's █/░ shading into something unreadable.
        // An explicit bg lets cells with their own fg style (the HEALTH
        // column) keep their health color, and plain-fg cells (the
        // capacity minibar) render normally on the grey bg.
        row.style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        row
    }
}

fn render_capacity_cell(p: &PoolInfo) -> String {
    // "8.2 GiB/16.0 GiB [████░░]"
    let used = format_bytes(p.allocated_bytes);
    let total = format_bytes(p.size_bytes);
    let frac = p.capacity_fraction().clamp(0.0, 1.0);
    let filled = (frac * 6.0).round() as usize;
    let filled = filled.min(6);
    let mini: String = "█".repeat(filled) + &"░".repeat(6 - filled);
    format!("{used}/{total} [{mini}]")
}

fn render_scrub_cell(scrub: &ScrubState) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
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
            fragmentation_pct: Some(12),
            scrub: ScrubState::Never,
            errors: ErrorCounts::default(),
            root_vdev: VdevNode {
                name: name.into(),
                kind: VdevKind::Root,
                state: VdevState::Online,
                size_bytes: Some(size),
                errors: ErrorCounts::default(),
                children: vec![],
                device_path: None,
            },
        }
    }

    fn app_for_pools_list(
        pools: Vec<PoolInfo>,
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
        let pools_source: Option<Box<dyn crate::pools::PoolsSource>> =
            Some(Box::new(FakePoolsSource::new(pools.clone())));
        let mut app =
            App::new(arc_reader, mem, pools_source, pools_init_error, None, None)
                .expect("fixture App::new");
        app.current_tab = Tab::Pools;
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

    fn render_pools(app: &App, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| super::super::draw(frame, app))
            .expect("draw");
        whole_text(terminal.backend())
    }

    #[test]
    fn wide_layout_shows_frag_and_err_columns() {
        let app = app_for_pools_list(
            vec![test_pool("tank", PoolHealth::Online, 1_000_000_000, 500_000_000)],
            None,
        );
        let out = render_pools(&app, 120, 24); // wide
        assert!(out.contains("FRAG"), "wide layout missing FRAG column");
        assert!(out.contains("ERR"), "wide layout missing ERR column");
        assert!(out.contains("12%"), "wide layout missing frag value");
    }

    #[test]
    fn narrow_layout_drops_frag_and_err() {
        let app = app_for_pools_list(
            vec![test_pool("tank", PoolHealth::Online, 1_000, 500)],
            None,
        );
        let out = render_pools(&app, 80, 24); // narrow
        assert!(!out.contains("FRAG"), "narrow layout should not show FRAG");
        // ERR is a substring of "error" etc., guard more carefully:
        // header word is exactly "ERR" padded by spaces.
        assert!(
            !out.contains(" ERR "),
            "narrow layout should not show ERR column header"
        );
    }

    #[test]
    fn selection_marker_on_selected_row() {
        let pools = vec![
            test_pool("a", PoolHealth::Online, 100, 50),
            test_pool("b", PoolHealth::Online, 100, 50),
            test_pool("c", PoolHealth::Online, 100, 50),
        ];
        let mut app = app_for_pools_list(pools, None);
        app.pools_view = PoolsView::List { selected: 1 };
        let out = render_pools(&app, 120, 24);
        // The selected row should have `>` as the marker prefix. Find the
        // line containing "b" and assert it has the marker.
        let line = out
            .lines()
            .find(|l| l.contains(" b "))
            .expect("missing b row");
        assert!(line.contains('>'), "selected row missing > marker: {line:?}");
    }

    #[test]
    fn empty_snapshot_shows_notice() {
        let app = app_for_pools_list(vec![], None);
        let out = render_pools(&app, 80, 24);
        assert!(out.contains("no pools imported"), "missing no-pools notice");
    }

    #[test]
    fn libzfs_unavailable_shows_init_error() {
        let app = app_for_pools_list(vec![], Some("init failed at /dev/zfs".to_string()));
        let out = render_pools(&app, 80, 24);
        assert!(
            out.contains("libzfs unavailable"),
            "missing libzfs-unavailable notice"
        );
    }
}
