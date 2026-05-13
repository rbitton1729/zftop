//! Pools tab — unified tree view. Uses ratatui's `Table` widget so the
//! NAME and CAPACITY columns flex to fill the terminal width via
//! `Constraint::Min`; fixed columns (HEALTH, TYPE, FRAG, SCRUB, ERR/R/W/C)
//! stay at fixed widths via `Constraint::Length`.
//!
//! Pool rows and vdev rows share the same column schema; their content
//! differs. Tree depth is embedded as leading spaces in the column-0
//! string (along with the ▼/▶ expand glyph for pool rows). Selection
//! highlight uses `Row::style(bg DarkGray + bold)` which propagates to
//! all cells while preserving each cell's foreground color.

use ratatui::Frame;
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Cell, Row, Table};

use super::widgets;
use crate::app::{App, PoolsView, VisibleRow, format_bytes};
use crate::pools::{PoolHealth, PoolInfo, ScrubState, VdevKind, VdevNode, VdevState};

const WIDE_THRESHOLD: u16 = 100;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title("Pools");
    let inner = block.inner(area);
    frame.render_widget(block, area);

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

    let wide = inner.width >= WIDE_THRESHOLD;
    let rows_data = app.flatten_visible_pool_rows();
    let selected_idx = match &app.pools_view {
        PoolsView::Tree { selected, .. } => *selected,
        PoolsView::Detail { .. } => 0,
    };
    let expanded = match &app.pools_view {
        PoolsView::Tree { expanded, .. } => expanded,
        PoolsView::Detail { expanded, .. } => expanded,
    };

    let rows: Vec<Row> = rows_data
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let is_selected = i == selected_idx;
            match row {
                VisibleRow::Pool(p) => build_pool_row(p, expanded, is_selected, wide),
                VisibleRow::Vdev { node, depth } => build_vdev_row(node, *depth, is_selected, wide),
            }
        })
        .collect();

    let header = build_header_row(wide);
    let widths = build_widths(wide);
    let table = Table::new(rows, widths).header(header);
    frame.render_widget(table, inner);
}

fn build_widths(wide: bool) -> Vec<Constraint> {
    if wide {
        vec![
            Constraint::Min(16),    // NAME (flex)
            Constraint::Length(10), // HEALTH/STATE
            Constraint::Length(8),  // TYPE/KIND
            Constraint::Min(28),    // CAPACITY/SIZE+PATH (flex)
            Constraint::Length(6),  // FRAG
            Constraint::Length(16), // SCRUB
            Constraint::Length(6),  // ERR (pool sum) / R (vdev)
            Constraint::Length(6),  // (blank pool) / W (vdev)
            Constraint::Length(6),  // (blank pool) / C (vdev)
        ]
    } else {
        vec![
            Constraint::Min(16),    // NAME (flex)
            Constraint::Length(10), // HEALTH/STATE
            Constraint::Length(8),  // TYPE/KIND
            Constraint::Min(20),    // CAPACITY/SIZE (flex)
            Constraint::Length(16), // SCRUB
            Constraint::Length(6),  // ERR (vdev sum; pool blank)
        ]
    }
}

fn build_header_row(wide: bool) -> Row<'static> {
    let bold = Style::default().add_modifier(Modifier::BOLD);
    // Note: the "ERR" header in col 6 (wide) is approximate. On pool rows
    // it shows the per-pool error sum; on vdev rows that cell holds the
    // R count and cols 7/8 hold W/C. The visual context (three numbers
    // in a row on vdev lines) makes the meaning clear.
    let cells: Vec<Cell> = if wide {
        vec![
            Cell::from(Span::styled("NAME", bold)),
            Cell::from(Span::styled("HEALTH", bold)),
            Cell::from(Span::styled("TYPE", bold)),
            Cell::from(Span::styled("CAPACITY", bold)),
            Cell::from(Span::styled("FRAG", bold)),
            Cell::from(Span::styled("SCRUB", bold)),
            Cell::from(Span::styled("ERR", bold)),
            Cell::from(Span::styled("", bold)),
            Cell::from(Span::styled("", bold)),
        ]
    } else {
        vec![
            Cell::from(Span::styled("NAME", bold)),
            Cell::from(Span::styled("HEALTH", bold)),
            Cell::from(Span::styled("TYPE", bold)),
            Cell::from(Span::styled("CAPACITY", bold)),
            Cell::from(Span::styled("SCRUB", bold)),
            Cell::from(Span::styled("ERR", bold)),
        ]
    };
    Row::new(cells)
}

fn build_pool_row<'a>(
    p: &'a PoolInfo,
    expanded: &std::collections::BTreeSet<String>,
    is_selected: bool,
    wide: bool,
) -> Row<'a> {
    let glyph = if p.root_vdev.children.is_empty() {
        ' '
    } else if expanded.contains(&p.name) {
        '▼'
    } else {
        '▶'
    };
    let name_cell = format!("{glyph} {}", p.name);
    let health_cell = Cell::from(Span::styled(
        pool_health_label(p.health),
        widgets::pool_health_style(p.health),
    ));
    let type_cell = p.raid_label();
    let capacity_cell = render_capacity_cell(p);
    let scrub_cell = render_scrub_cell(&p.scrub);

    let cells: Vec<Cell> = if wide {
        let frag = match p.fragmentation_pct {
            Some(v) => format!("{v}%"),
            None => "—".into(),
        };
        let err_sum = p.root_vdev.total_errors();
        let err_cell = Cell::from(Span::styled(format!("{err_sum}"), err_style(err_sum)));
        vec![
            Cell::from(name_cell),
            health_cell,
            Cell::from(type_cell),
            Cell::from(capacity_cell),
            Cell::from(frag),
            Cell::from(scrub_cell),
            err_cell,
            Cell::from(""),
            Cell::from(""),
        ]
    } else {
        vec![
            Cell::from(name_cell),
            health_cell,
            Cell::from(type_cell),
            Cell::from(capacity_cell),
            Cell::from(scrub_cell),
            // Pool narrow rows have no ERR; only vdev narrow rows do.
            Cell::from(""),
        ]
    };

    let row = Row::new(cells);
    if is_selected {
        row.style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        row
    }
}

fn build_vdev_row<'a>(node: &'a VdevNode, depth: u8, is_selected: bool, wide: bool) -> Row<'a> {
    // Indent + leading 2-space "glyph slot" so the name aligns under the
    // pool's name (which is "{glyph} {name}" — glyph + space + name).
    let indent = "  ".repeat(depth as usize);
    let name_cell = format!("{indent}  {}", node.name);
    let state_label = if is_group(node.kind) {
        String::new()
    } else {
        vdev_state_label(node.state).to_string()
    };
    let state_cell = Cell::from(Span::styled(state_label, vdev_state_style(node.state)));
    let kind_cell = vdev_kind_label(node.kind);
    let size_str = match node.size_bytes {
        Some(b) => format_bytes(b),
        None => String::new(),
    };

    let cells: Vec<Cell> = if wide {
        // Pack SIZE + " " + DEVICE_PATH into the CAPACITY-equivalent cell
        // (col 3). Table will truncate from the right with `…` if the path
        // overflows the cell width allotted by the layout engine.
        let path_str = match (node.kind, node.device_path.as_ref()) {
            (
                VdevKind::Disk
                | VdevKind::File
                | VdevKind::LogVdev
                | VdevKind::CacheVdev
                | VdevKind::SpareVdev,
                Some(path),
            ) => path.clone(),
            _ => String::new(),
        };
        let size_path = if path_str.is_empty() {
            size_str
        } else {
            format!("{size_str:<8} {path_str}")
        };
        let (read, write, checksum) = if is_group(node.kind) {
            (String::new(), String::new(), String::new())
        } else {
            (
                format!("{}", node.errors.read),
                format!("{}", node.errors.write),
                format!("{}", node.errors.checksum),
            )
        };
        vec![
            Cell::from(name_cell),
            state_cell,
            Cell::from(kind_cell),
            Cell::from(size_path),
            Cell::from(""), // FRAG slot blank on vdev rows
            Cell::from(""), // SCRUB slot blank on vdev rows
            Cell::from(Span::styled(read, err_style(node.errors.read))),
            Cell::from(Span::styled(write, err_style(node.errors.write))),
            Cell::from(Span::styled(checksum, err_style(node.errors.checksum))),
        ]
    } else {
        // Narrow: drop DEVICE_PATH; combine R/W/C into a single ERR cell.
        let err_sum_val = if is_group(node.kind) {
            0
        } else {
            node.errors.sum()
        };
        let err_sum_str = if is_group(node.kind) {
            String::new()
        } else {
            format!("{err_sum_val}")
        };
        vec![
            Cell::from(name_cell),
            state_cell,
            Cell::from(kind_cell),
            Cell::from(size_str),
            Cell::from(""), // SCRUB slot blank on vdev rows
            Cell::from(Span::styled(err_sum_str, err_style(err_sum_val))),
        ]
    };

    let row = Row::new(cells);
    if is_selected {
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

fn pool_health_label(health: PoolHealth) -> String {
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

fn err_style(n: u64) -> Style {
    if n == 0 {
        Style::default()
    } else {
        Style::default().fg(Color::Red)
    }
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

fn vdev_kind_label(kind: VdevKind) -> String {
    match kind {
        VdevKind::Root => "root".into(),
        VdevKind::Raidz { parity } => format!("raidz{parity}"),
        VdevKind::Mirror => "mirror".into(),
        VdevKind::Disk => "disk".into(),
        VdevKind::File => "file".into(),
        VdevKind::LogGroup => "logs".into(),
        VdevKind::LogVdev => "log".into(),
        VdevKind::CacheGroup => "cache".into(),
        VdevKind::CacheVdev => "cache".into(),
        VdevKind::SpareGroup => "spares".into(),
        VdevKind::SpareVdev => "spare".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, PoolsView, Tab};
    use crate::arcstats;
    use crate::meminfo::{self, MemSource};
    use crate::pools::fake::FakePoolsSource;
    use crate::pools::{
        ErrorCounts, PoolHealth, PoolInfo, PoolsSource, ScrubState, VdevKind, VdevNode, VdevState,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::path::PathBuf;

    fn leaf(name: &str, errors: ErrorCounts, device_path: Option<&str>) -> VdevNode {
        VdevNode {
            name: name.into(),
            kind: VdevKind::Disk,
            state: VdevState::Online,
            size_bytes: Some(256 * 1024 * 1024 * 1024),
            errors,
            children: vec![],
            device_path: device_path.map(|s| s.to_string()),
        }
    }

    fn raidz1_pool(name: &str, leaves: Vec<VdevNode>) -> PoolInfo {
        PoolInfo {
            name: name.into(),
            health: PoolHealth::Online,
            allocated_bytes: 500 * 1024 * 1024 * 1024,
            size_bytes: 1024 * 1024 * 1024 * 1024,
            free_bytes: 524 * 1024 * 1024 * 1024,
            fragmentation_pct: Some(8),
            scrub: ScrubState::Never,
            errors: ErrorCounts::default(),
            root_vdev: VdevNode {
                name: name.into(),
                kind: VdevKind::Root,
                state: VdevState::Online,
                size_bytes: Some(1024 * 1024 * 1024 * 1024),
                errors: ErrorCounts::default(),
                children: vec![VdevNode {
                    name: "raidz1-0".into(),
                    kind: VdevKind::Raidz { parity: 1 },
                    state: VdevState::Online,
                    size_bytes: Some(1024 * 1024 * 1024 * 1024),
                    errors: ErrorCounts::default(),
                    children: leaves,
                    device_path: None,
                }],
                device_path: None,
            },
        }
    }

    fn app_for_tree(
        pools: Vec<PoolInfo>,
        pools_init_error: Option<String>,
        expanded_names: &[&str],
    ) -> App {
        let arcstats_path = PathBuf::from("fixtures/arcstats");
        let meminfo_path = PathBuf::from("fixtures/meminfo");
        let arc_reader: Box<dyn FnMut() -> anyhow::Result<arcstats::ArcStats>> = {
            let p = arcstats_path.clone();
            Box::new(move || arcstats::linux::from_procfs_path(&p))
        };
        let mem: Option<Box<dyn MemSource>> =
            Some(Box::new(meminfo::linux::LinuxMemSource::new(meminfo_path)));
        let pools_source: Option<Box<dyn PoolsSource>> =
            Some(Box::new(FakePoolsSource::new(pools.clone())));
        let mut app = App::new(arc_reader, mem, pools_source, pools_init_error, None, None)
            .expect("fixture App::new");
        app.current_tab = Tab::Pools;
        // Override default-expand: clear, then insert exactly the names asked for.
        if let PoolsView::Tree { expanded, selected } = &mut app.pools_view {
            expanded.clear();
            for n in expanded_names {
                expanded.insert((*n).to_string());
            }
            *selected = 0;
        }
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

    fn render(app: &App, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| draw(frame, frame.area(), app))
            .expect("draw");
        whole_text(terminal.backend())
    }

    #[test]
    fn empty_snapshot_shows_no_pools_notice() {
        let app = app_for_tree(vec![], None, &[]);
        let out = render(&app, 100, 24);
        assert!(out.contains("no pools imported"));
    }

    #[test]
    fn libzfs_unavailable_shows_init_error() {
        let app = app_for_tree(vec![], Some("init failed".to_string()), &[]);
        let out = render(&app, 100, 24);
        assert!(out.contains("libzfs unavailable"));
    }

    #[test]
    fn collapsed_pool_shows_only_pool_row() {
        let pool = raidz1_pool("tank", vec![leaf("sda", ErrorCounts::default(), None)]);
        let app = app_for_tree(vec![pool], None, &[]); // not expanded
        let out = render(&app, 120, 24);
        assert!(out.contains("tank"));
        assert!(
            !out.contains("raidz1-0"),
            "should not show raidz when collapsed"
        );
        assert!(!out.contains("sda"), "should not show leaf when collapsed");
        assert!(out.contains("▶"), "collapsed pool should show ▶ glyph");
    }

    #[test]
    fn expanded_pool_shows_vdev_rows() {
        let pool = raidz1_pool("tank", vec![leaf("sda", ErrorCounts::default(), None)]);
        let app = app_for_tree(vec![pool], None, &["tank"]);
        let out = render(&app, 120, 24);
        assert!(out.contains("tank"));
        assert!(out.contains("raidz1-0"));
        assert!(out.contains("sda"));
        assert!(out.contains("▼"), "expanded pool should show ▼ glyph");
    }

    #[test]
    fn vdev_row_shows_device_path() {
        let pool = raidz1_pool(
            "tank",
            vec![leaf(
                "sda",
                ErrorCounts::default(),
                Some("/dev/disk/by-id/wwn-0x500abc"),
            )],
        );
        let app = app_for_tree(vec![pool], None, &["tank"]);
        // Render wide enough for the CAPACITY+PATH cell to fit the full
        // 28-char path plus the "256.0 GiB " prefix without truncation.
        // Under the new flex layout, NAME and CAPACITY share the leftover
        // slack roughly evenly, so we need a wide terminal to give CAPACITY
        // enough room for the full path tail.
        let out = render(&app, 200, 24);
        assert!(
            out.contains("/dev/disk/by-id/wwn-0x500abc") || out.contains("by-id/wwn-0x500abc"),
            "device path missing or truncated unexpectedly: {out:?}"
        );
    }

    #[test]
    fn device_path_truncates_when_overflowing() {
        // With the Table widget, ratatui truncates cell content from the
        // right when it overflows the column's allotted width. We no
        // longer prepend `…` ourselves — Table handles that. The test
        // therefore asserts that the path is partially visible (the
        // leading prefix survives) but the trailing "fff" suffix does
        // not (because the cell is too narrow to fit the full path).
        let long_path = "/dev/disk/by-id/wwn-0x500abc123def4567890aaabbbccc1234567890dddeeefff";
        let pool = raidz1_pool(
            "tank",
            vec![leaf("sda", ErrorCounts::default(), Some(long_path))],
        );
        let app = app_for_tree(vec![pool], None, &["tank"]);
        // 102 cols outer → 100 cols inner = wide-mode threshold; CAPACITY
        // column flexes but is bounded by total width.
        let out = render(&app, 102, 24);
        // The path's leading prefix should be visible somewhere.
        assert!(
            out.contains("/dev/disk/by-id/")
                || out.contains("disk/by-id/")
                || out.contains("by-id/wwn"),
            "expected a prefix of the long path to be visible: {out:?}"
        );
        // And the path is too long for the CAPACITY cell at 102 cols, so
        // its full trailing suffix should NOT appear in full.
        assert!(
            !out.contains("dddeeefff"),
            "expected the long path's tail to be truncated at 102 cols: {out:?}"
        );
    }

    #[test]
    fn rwc_columns_color_red_when_nonzero() {
        let pool = raidz1_pool(
            "tank",
            vec![leaf(
                "sda",
                ErrorCounts {
                    read: 5,
                    write: 0,
                    checksum: 0,
                },
                None,
            )],
        );
        let app = app_for_tree(vec![pool], None, &["tank"]);
        let backend = TestBackend::new(140, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| draw(frame, frame.area(), &app))
            .expect("draw");
        // Find the cell containing "5" on the sda row and check its fg color.
        let buf = terminal.backend().buffer();
        let mut found_red_5 = false;
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            if row.contains("sda") {
                for x in 0..buf.area.width {
                    let cell = &buf[(x, y)];
                    if cell.symbol() == "5" && cell.fg == Color::Red {
                        found_red_5 = true;
                        break;
                    }
                }
                break;
            }
        }
        assert!(found_red_5, "expected red '5' in R column on sda row");
    }

    #[test]
    fn group_header_rows_blank_size_and_errors() {
        // Build a pool with a log group containing one log vdev.
        let log_vdev = VdevNode {
            name: "nvme0n1p1".into(),
            kind: VdevKind::LogVdev,
            state: VdevState::Online,
            size_bytes: Some(8 * 1024 * 1024 * 1024),
            errors: ErrorCounts::default(),
            children: vec![],
            device_path: Some("/dev/nvme0n1p1".into()),
        };
        let log_group = VdevNode {
            name: "logs".into(),
            kind: VdevKind::LogGroup,
            state: VdevState::Online,
            size_bytes: None,
            errors: ErrorCounts::default(),
            children: vec![log_vdev],
            device_path: None,
        };
        let mut pool = raidz1_pool("tank", vec![]);
        pool.root_vdev.children.push(log_group);
        let app = app_for_tree(vec![pool], None, &["tank"]);
        let out = render(&app, 140, 24);
        // The 'logs' group row should show its kind label but no size cell content.
        let logs_line = out
            .lines()
            .find(|l| l.contains("logs") && !l.contains("nvme"))
            .expect("missing logs group row");
        // The size column starts after STATE+KIND. Just assert the row has no
        // numeric byte unit in the SIZE cell.
        assert!(
            !logs_line.contains(" GiB ") && !logs_line.contains(" TiB "),
            "logs group row should have blank SIZE: {logs_line:?}"
        );
    }

    #[test]
    fn selection_highlight_persists_across_pool_and_vdev_rows() {
        let pool = raidz1_pool("tank", vec![leaf("sda", ErrorCounts::default(), None)]);
        let mut app = app_for_tree(vec![pool], None, &["tank"]);
        // Move selection to sda (row 2: pool, raidz, sda).
        if let PoolsView::Tree { selected, .. } = &mut app.pools_view {
            *selected = 2;
        }
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| draw(frame, frame.area(), &app))
            .expect("draw");
        let buf = terminal.backend().buffer();
        // Find the row containing "sda" and assert at least one cell in that
        // row has DarkGray bg.
        let mut found_dark_bg = false;
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            if row.contains("sda") {
                for x in 0..buf.area.width {
                    if buf[(x, y)].bg == Color::DarkGray {
                        found_dark_bg = true;
                        break;
                    }
                }
                break;
            }
        }
        assert!(
            found_dark_bg,
            "selected sda row should have DarkGray bg cells"
        );
    }

    #[test]
    fn narrow_layout_drops_columns() {
        let pool = raidz1_pool(
            "tank",
            vec![leaf("sda", ErrorCounts::default(), Some("/dev/sda"))],
        );
        let app = app_for_tree(vec![pool], None, &["tank"]);
        let out = render(&app, 80, 24);
        assert!(!out.contains("FRAG"), "narrow should drop FRAG header");
        assert!(
            !out.contains("/dev/sda"),
            "narrow should drop DEVICE_PATH on vdev rows: {out:?}"
        );
    }

    #[test]
    fn default_expanded_visible_after_first_paint() {
        // Use the real default-expand path (App::new + first refresh insert).
        let arcstats_path = PathBuf::from("fixtures/arcstats");
        let meminfo_path = PathBuf::from("fixtures/meminfo");
        let arc_reader: Box<dyn FnMut() -> anyhow::Result<arcstats::ArcStats>> = {
            let p = arcstats_path.clone();
            Box::new(move || arcstats::linux::from_procfs_path(&p))
        };
        let mem: Option<Box<dyn MemSource>> =
            Some(Box::new(meminfo::linux::LinuxMemSource::new(meminfo_path)));
        let pool = raidz1_pool("tank", vec![leaf("sda", ErrorCounts::default(), None)]);
        let pools_source: Option<Box<dyn PoolsSource>> =
            Some(Box::new(FakePoolsSource::new(vec![pool.clone()])));
        let mut app = App::new(arc_reader, mem, pools_source, None, None, None).expect("App::new");
        app.current_tab = Tab::Pools;
        app.pools_snapshot = vec![pool];
        let out = render(&app, 120, 24);
        assert!(
            out.contains("▼"),
            "default-expand should show ▼ on first paint: {out:?}"
        );
        assert!(
            out.contains("raidz1-0"),
            "vdev rows should be visible by default: {out:?}"
        );
    }
}
