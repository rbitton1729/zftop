//! Pools tab — per-pool detail view. Entered via `Enter` on a selected row
//! in the list view. Shows a header with pool-level health / capacity /
//! fragmentation / scrub summary, then an indented vdev tree below.
//!
//! The vdev tree is rendered by flattening the recursive `VdevNode`
//! structure into `Row`s with depth-based indentation and rendering as a
//! ratatui `Table`. Top-level group nodes (logs/cache/spares) render as
//! unindented section headers with their members indented underneath.

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use ratatui::Frame;

use super::widgets;
use crate::app::{format_bytes, App, PoolsView};
use crate::pools::{PoolInfo, ScrubState, VdevKind, VdevNode, VdevState};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    // Figure out which pool to show. Defensive clamp in case pools_view
    // references an out-of-range index (shouldn't happen — clamp_pools_selection
    // runs after every refresh — but don't panic if it does).
    let pool_index = match app.pools_view {
        PoolsView::Detail { pool_index } => pool_index,
        PoolsView::List { selected } => selected,
    };

    // If there's no pool at this index (empty snapshot or libzfs error),
    // fall back to a centred notice.
    let Some(pool) = app.pools_snapshot.get(pool_index) else {
        let block = Block::default().borders(Borders::ALL).title("Pool Detail");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        draw_centered(
            frame,
            inner,
            "(no pool selected)",
            Style::default().fg(Color::DarkGray),
        );
        return;
    };

    // Outer block with the pool name as title.
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", pool.name));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Vertical split: header (2 rows) / tree (fill).
    let [header_area, tree_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(4),
    ])
    .areas(inner);

    draw_header(frame, header_area, pool);
    draw_vdev_tree(frame, tree_area, pool);
}

fn draw_header(frame: &mut Frame, area: Rect, pool: &PoolInfo) {
    let [line1, line2] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);

    // Line 1: <HEALTH>   <alloc>/<size> (<pct>%)   [<minibar>]   frag <frag>%
    let frac = pool.capacity_fraction().clamp(0.0, 1.0);
    let filled = (frac * 8.0).round() as usize;
    let filled = filled.min(8);
    let mini: String = "█".repeat(filled) + &"░".repeat(8 - filled);
    let cap = format!(
        "{}/{} ({:.0}%)",
        format_bytes(pool.allocated_bytes),
        format_bytes(pool.size_bytes),
        frac * 100.0
    );
    let frag = match pool.fragmentation_pct {
        Some(v) => format!("frag {v}%"),
        None => "frag —".into(),
    };
    let health = Span::styled(
        pool_health_label(pool.health),
        widgets::pool_health_style(pool.health),
    );

    let header1 = Paragraph::new(Line::from(vec![
        Span::raw(" "),
        health,
        Span::raw("  "),
        Span::raw(cap),
        Span::raw("  ["),
        Span::raw(mini),
        Span::raw("]  "),
        Span::raw(frag),
    ]));
    frame.render_widget(header1, line1);

    // Line 2: scrub summary (one of: "Last scrub: Xd ago, Y errors repaired",
    // "Scrub: N% complete", "No scrub recorded", "Scrub error").
    let scrub_line = format_scrub_header(&pool.scrub);
    let header2 = Paragraph::new(Line::from(vec![Span::raw(" "), Span::raw(scrub_line)]));
    frame.render_widget(header2, line2);
}

fn format_scrub_header(scrub: &ScrubState) -> String {
    match scrub {
        ScrubState::Never => "No scrub recorded".into(),
        ScrubState::Error => "Scrub: last run errored or was canceled".into(),
        ScrubState::InProgress {
            progress_pct,
            eta_seconds,
            speed_bytes_per_sec,
            is_resilver,
        } => {
            let verb = if *is_resilver { "Resilver" } else { "Scrub" };
            let mut s = format!("{verb}: {progress_pct}% complete");
            if let Some(eta) = eta_seconds {
                let hours = eta / 3600;
                let minutes = (eta % 3600) / 60;
                s.push_str(&format!(", ETA {hours:02}:{minutes:02}"));
            }
            if let Some(bps) = speed_bytes_per_sec {
                s.push_str(&format!(", {}/s", format_bytes(*bps)));
            }
            s
        }
        ScrubState::Finished {
            completed_at,
            errors_repaired,
        } => {
            let rel = format_relative_time(*completed_at);
            format!("Last scrub: {rel}, {errors_repaired} errors repaired")
        }
    }
}

fn draw_vdev_tree(frame: &mut Frame, area: Rect, pool: &PoolInfo) {
    // Flatten the VdevNode tree into (depth, &VdevNode) pairs.
    let mut flat: Vec<(usize, &VdevNode)> = Vec::new();
    flatten(&pool.root_vdev, 0, &mut flat);

    let rows: Vec<Row> = flat
        .iter()
        .map(|(depth, node)| build_vdev_row(*depth, node))
        .collect();

    let widths = [
        Constraint::Length(28), // NAME (indented)
        Constraint::Length(10), // STATE
        Constraint::Length(8),  // READ
        Constraint::Length(8),  // WRITE
        Constraint::Length(8),  // CKSUM
        Constraint::Length(10), // SIZE
    ];

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let header = Row::new(vec![
        Span::styled("NAME", bold),
        Span::styled("STATE", bold),
        Span::styled("READ", bold),
        Span::styled("WRITE", bold),
        Span::styled("CKSUM", bold),
        Span::styled("SIZE", bold),
    ]);

    let table = Table::new(rows, widths).header(header);
    frame.render_widget(table, area);
}

fn flatten<'a>(node: &'a VdevNode, depth: usize, out: &mut Vec<(usize, &'a VdevNode)>) {
    out.push((depth, node));
    for child in &node.children {
        flatten(child, depth + 1, out);
    }
}

fn build_vdev_row(depth: usize, node: &VdevNode) -> Row<'static> {
    let indent = "  ".repeat(depth);
    let name = format!("{indent}{}", node.name);

    // State label + style. Group-header vdevs (LogGroup/CacheGroup/
    // SpareGroup) don't have meaningful state — render blank.
    let state_label = if is_group(node.kind) {
        String::new()
    } else {
        vdev_state_label(node.state).to_string()
    };
    let state_span = Span::styled(state_label, vdev_state_style(node.state));

    // READ/WRITE/CKSUM counts. Groups render blank. Interior nodes
    // (raidz/mirror) render blank for error counts — the rollup shows on
    // the root row and leaf rows.
    let (read, write, cksum) = match node.kind {
        VdevKind::LogGroup | VdevKind::CacheGroup | VdevKind::SpareGroup => {
            (String::new(), String::new(), String::new())
        }
        _ => (
            format!("{}", node.errors.read),
            format!("{}", node.errors.write),
            format!("{}", node.errors.checksum),
        ),
    };

    // Color the error columns red if non-zero.
    let err_style = |n: u64| {
        if n == 0 {
            Style::default()
        } else {
            Style::default().fg(Color::Red)
        }
    };

    let size = match node.size_bytes {
        Some(b) => format_bytes(b),
        None => String::new(),
    };

    Row::new(vec![
        Span::raw(name),
        state_span,
        Span::styled(read, err_style(node.errors.read)),
        Span::styled(write, err_style(node.errors.write)),
        Span::styled(cksum, err_style(node.errors.checksum)),
        Span::raw(size),
    ])
}

fn is_group(kind: VdevKind) -> bool {
    matches!(
        kind,
        VdevKind::LogGroup | VdevKind::CacheGroup | VdevKind::SpareGroup
    )
}

fn vdev_state_label(state: VdevState) -> &'static str {
    match state {
        VdevState::Online => "ONLINE",
        VdevState::Degraded => "DEGRADED",
        VdevState::Faulted => "FAULTED",
        VdevState::Offline => "OFFLINE",
        VdevState::Removed => "REMOVED",
        VdevState::Unavail => "UNAVAIL",
    }
}

fn vdev_state_style(state: VdevState) -> Style {
    match state {
        VdevState::Online => Style::default().fg(Color::Green),
        VdevState::Degraded => Style::default().fg(Color::Yellow),
        VdevState::Faulted | VdevState::Removed | VdevState::Unavail => {
            Style::default().fg(Color::Red)
        }
        VdevState::Offline => Style::default().fg(Color::DarkGray),
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

#[cfg(test)]
mod tests {
    use crate::app::{App, PoolsView, Tab};
    use crate::arcstats;
    use crate::meminfo::{self, MemSource};
    use crate::pools::fake::FakePoolsSource;
    use crate::pools::{
        ErrorCounts, PoolHealth, PoolInfo, ScrubState, VdevKind, VdevNode, VdevState,
    };
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    fn leaf(name: &str, size: u64, errors: ErrorCounts) -> VdevNode {
        VdevNode {
            name: name.into(),
            kind: VdevKind::Disk,
            state: VdevState::Online,
            size_bytes: Some(size),
            errors,
            children: vec![],
        }
    }

    fn raidz_pool() -> PoolInfo {
        // tank: raidz1-0 containing 4 disks, 0 errors, scrubbed 3 days ago.
        PoolInfo {
            name: "tank".into(),
            health: PoolHealth::Online,
            allocated_bytes: 500 * 1024 * 1024 * 1024,
            size_bytes: 1024 * 1024 * 1024 * 1024,
            free_bytes: 524 * 1024 * 1024 * 1024,
            fragmentation_pct: Some(8),
            scrub: ScrubState::Finished {
                completed_at: SystemTime::now() - Duration::from_secs(3 * 86_400),
                errors_repaired: 0,
            },
            errors: ErrorCounts::default(),
            root_vdev: VdevNode {
                name: "tank".into(),
                kind: VdevKind::Root,
                state: VdevState::Online,
                size_bytes: Some(1024 * 1024 * 1024 * 1024),
                errors: ErrorCounts::default(),
                children: vec![VdevNode {
                    name: "raidz1".into(),
                    kind: VdevKind::Raidz,
                    state: VdevState::Online,
                    size_bytes: Some(1024 * 1024 * 1024 * 1024),
                    errors: ErrorCounts::default(),
                    children: vec![
                        leaf("sda", 256 * 1024 * 1024 * 1024, ErrorCounts::default()),
                        leaf("sdb", 256 * 1024 * 1024 * 1024, ErrorCounts::default()),
                        leaf("sdc", 256 * 1024 * 1024 * 1024, ErrorCounts::default()),
                        leaf("sdd", 256 * 1024 * 1024 * 1024, ErrorCounts::default()),
                    ],
                }],
            },
        }
    }

    fn degraded_pool() -> PoolInfo {
        let mut p = raidz_pool();
        p.health = PoolHealth::Degraded;
        p.root_vdev.state = VdevState::Degraded;
        if let Some(rz) = p.root_vdev.children.first_mut() {
            rz.state = VdevState::Degraded;
            if let Some(disk) = rz.children.first_mut() {
                disk.state = VdevState::Faulted;
                disk.errors.read = 5;
            }
        }
        p
    }

    fn scrubbing_pool() -> PoolInfo {
        let mut p = raidz_pool();
        p.scrub = ScrubState::InProgress {
            progress_pct: 42,
            eta_seconds: Some(1800),
            speed_bytes_per_sec: Some(100 * 1024 * 1024),
            is_resilver: false,
        };
        p
    }

    fn app_for_detail(pool: PoolInfo) -> App {
        let arcstats_path = PathBuf::from("fixtures/arcstats");
        let meminfo_path = PathBuf::from("fixtures/meminfo");
        let arc_reader: Box<dyn FnMut() -> anyhow::Result<arcstats::ArcStats>> = {
            let p = arcstats_path.clone();
            Box::new(move || arcstats::linux::from_procfs_path(&p))
        };
        let mem: Option<Box<dyn MemSource>> = Some(Box::new(
            meminfo::linux::LinuxMemSource::new(meminfo_path),
        ));
        let pools = vec![pool];
        let pools_source: Option<Box<dyn crate::pools::PoolsSource>> =
            Some(Box::new(FakePoolsSource::new(pools.clone())));
        let mut app = App::new(arc_reader, mem, pools_source, None).expect("fixture App::new");
        app.current_tab = Tab::Pools;
        app.pools_snapshot = pools;
        app.pools_view = PoolsView::Detail { pool_index: 0 };
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

    fn render_detail(app: &App) -> String {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| super::super::draw(frame, app))
            .expect("draw");
        whole_text(terminal.backend())
    }

    #[test]
    fn detail_renders_indented_vdev_tree() {
        let app = app_for_detail(raidz_pool());
        let out = render_detail(&app);
        assert!(out.contains("tank"), "missing pool name");
        assert!(out.contains("raidz1"), "missing raidz group");
        assert!(out.contains("sda"), "missing leaf sda");
        assert!(out.contains("sdd"), "missing leaf sdd");
        assert!(out.contains("NAME"), "missing table header NAME");
        assert!(out.contains("STATE"), "missing table header STATE");
        // Leaf should be indented deeper than the raidz group. Compare
        // column positions (skipping the block border at col 0).
        let sda_line = out.lines().find(|l| l.contains("sda")).expect("no sda");
        let rz_line = out.lines().find(|l| l.contains("raidz1")).expect("no raidz");
        let sda_col = sda_line.find("sda").expect("sda col");
        let rz_col = rz_line.find("raidz1").expect("raidz col");
        assert!(
            sda_col > rz_col,
            "expected sda column > raidz column: sda={sda_col}, raidz={rz_col}"
        );
    }

    #[test]
    fn detail_shows_degraded_label() {
        let app = app_for_detail(degraded_pool());
        let out = render_detail(&app);
        assert!(
            out.contains("DEGRADED"),
            "missing DEGRADED label: {out:?}"
        );
        assert!(
            out.contains("FAULTED"),
            "missing FAULTED label for the bad leaf: {out:?}"
        );
    }

    #[test]
    fn detail_shows_active_scrub_header() {
        let app = app_for_detail(scrubbing_pool());
        let out = render_detail(&app);
        assert!(
            out.contains("Scrub: 42% complete"),
            "missing scrub progress header: {out:?}"
        );
    }

    #[test]
    fn detail_shows_finished_scrub_header() {
        let app = app_for_detail(raidz_pool());
        let out = render_detail(&app);
        assert!(
            out.contains("Last scrub"),
            "missing finished-scrub header: {out:?}"
        );
    }

    #[test]
    fn detail_shows_empty_notice_when_index_out_of_range() {
        let mut app = app_for_detail(raidz_pool());
        // Force a bad index.
        app.pools_view = PoolsView::Detail { pool_index: 42 };
        let out = render_detail(&app);
        assert!(
            out.contains("no pool selected"),
            "missing fallback notice: {out:?}"
        );
    }

}
