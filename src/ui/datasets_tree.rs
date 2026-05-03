//! Datasets tab — tree view. Renders the flattened visible-row list as
//! a Table with depth-indented names and expand/collapse glyphs.
//! Selection highlight uses bg(DarkGray)+Bold (matches pools_list.rs)
//! to preserve cell-specific styling on selected rows. Wide layout
//! (≥90 cols) shows USED/REFER/AVAIL/COMPRESS; narrow drops AVAIL +
//! COMPRESS.

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Row, Table};
use ratatui::Frame;

use super::widgets;
use crate::app::{format_bytes, App, DatasetsView};
use crate::datasets::{DatasetKind, DatasetNode};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title("Datasets");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(err) = &app.datasets_init_error {
        widgets::draw_centered(
            frame,
            inner,
            &format!("datasets unavailable: {err}"),
            Style::default().fg(Color::Red),
        );
        return;
    }
    if app.datasets_snapshot.is_empty() {
        widgets::draw_centered(
            frame,
            inner,
            "(no pools imported)",
            Style::default().fg(Color::DarkGray),
        );
        return;
    }

    let wide = inner.width >= 90;
    let rows_data = app.flatten_visible_dataset_rows();
    let selected_idx = match &app.datasets_view {
        DatasetsView::Tree { selected, .. } => *selected,
        DatasetsView::Detail { .. } => 0,
    };
    let expanded = match &app.datasets_view {
        DatasetsView::Tree { expanded, .. } => expanded,
        DatasetsView::Detail { expanded, .. } => expanded,
    };

    let rows: Vec<Row> = rows_data
        .iter()
        .enumerate()
        .map(|(i, (depth, node))| {
            build_row(node, *depth, expanded.contains(&node.name),
                      i == selected_idx, wide)
        })
        .collect();

    let header = build_header(wide);

    if wide {
        let widths = [
            Constraint::Length(2),
            Constraint::Min(32),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(10),
            Constraint::Length(8),
        ];
        let table = Table::new(rows, widths).header(header);
        frame.render_widget(table, inner);
    } else {
        let widths = [
            Constraint::Length(2),
            Constraint::Min(20),
            Constraint::Length(10),
            Constraint::Length(10),
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
            Span::styled("USED", bold),
            Span::styled("REFER", bold),
            Span::styled("AVAIL", bold),
            Span::styled("COMPRESS", bold),
        ])
    } else {
        Row::new(vec![
            Span::raw(""),
            Span::styled("NAME", bold),
            Span::styled("USED", bold),
            Span::styled("REFER", bold),
        ])
    }
}

fn build_row(
    node: &DatasetNode,
    depth: usize,
    is_expanded: bool,
    is_selected: bool,
    wide: bool,
) -> Row<'static> {
    let marker = if is_selected { ">" } else { " " }.to_string();
    let glyph = match (node.kind, node.has_children(), is_expanded) {
        (DatasetKind::Volume, _, _) => 'V',
        (DatasetKind::Filesystem, true, true) => '▼',
        (DatasetKind::Filesystem, true, false) => '▶',
        (DatasetKind::Filesystem, false, _) => ' ',
    };
    let indent = " ".repeat(depth * 4);
    let name = format!("{indent}{glyph} {}", node.name);
    let used = format_bytes(node.used_bytes);
    let refer = format_bytes(node.refer_bytes);
    let avail = format_bytes(node.available_bytes);
    let compress = format!("{:.2}x", node.compression_ratio);

    let row = if wide {
        Row::new(vec![
            Span::raw(marker),
            Span::raw(name),
            Span::raw(used),
            Span::raw(refer),
            Span::raw(avail),
            Span::raw(compress),
        ])
    } else {
        Row::new(vec![
            Span::raw(marker),
            Span::raw(name),
            Span::raw(used),
            Span::raw(refer),
        ])
    };

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Tab};
    use crate::arcstats;
    use crate::datasets::fake::FakeDatasetsSource;
    use crate::datasets::types::{DatasetKind, DatasetProperties};
    use crate::datasets::DatasetsSource;
    use crate::meminfo::{self, MemSource};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    fn fs(name: &str, children: Vec<DatasetNode>) -> DatasetNode {
        DatasetNode {
            name: name.into(),
            kind: DatasetKind::Filesystem,
            used_bytes: 1024 * 1024 * 1024,
            refer_bytes: 1024 * 1024 * 1024,
            available_bytes: 10 * 1024 * 1024 * 1024,
            compression_ratio: 1.42,
            properties: DatasetProperties::default(),
            children,
        }
    }

    fn zvol(name: &str) -> DatasetNode {
        DatasetNode {
            name: name.into(),
            kind: DatasetKind::Volume,
            used_bytes: 1024 * 1024 * 1024,
            refer_bytes: 1024 * 1024 * 1024,
            available_bytes: 10 * 1024 * 1024 * 1024,
            compression_ratio: 1.0,
            properties: DatasetProperties::default(),
            children: vec![],
        }
    }

    fn app_for_datasets(roots: Vec<DatasetNode>, init_error: Option<String>) -> App {
        let arcstats_path = PathBuf::from("fixtures/arcstats");
        let meminfo_path = PathBuf::from("fixtures/meminfo");
        let arc_reader: Box<dyn FnMut() -> anyhow::Result<arcstats::ArcStats>> = {
            let p = arcstats_path.clone();
            Box::new(move || arcstats::linux::from_procfs_path(&p))
        };
        let mem: Option<Box<dyn MemSource>> =
            Some(Box::new(meminfo::linux::LinuxMemSource::new(meminfo_path)));
        let ds_source: Option<Box<dyn DatasetsSource>> =
            Some(Box::new(FakeDatasetsSource::new(roots.clone())));
        let mut app = App::new(arc_reader, mem, None, None, ds_source, init_error)
            .expect("fixture App::new");
        app.current_tab = Tab::Datasets;
        app.datasets_snapshot = roots.clone();
        if let DatasetsView::Tree { expanded, .. } = &mut app.datasets_view {
            for r in &roots {
                expanded.insert(r.name.clone());
            }
        }
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
            .draw(|frame| super::super::draw(frame, app))
            .expect("draw");
        whole_text(terminal.backend())
    }

    #[test]
    fn empty_snapshot_shows_no_pools_notice() {
        let app = app_for_datasets(vec![], None);
        let out = render(&app, 100, 24);
        assert!(out.contains("no pools imported"));
    }

    #[test]
    fn libzfs_init_error_shows_unavailable_notice() {
        let app = app_for_datasets(vec![], Some("init failed".to_string()));
        let out = render(&app, 100, 24);
        assert!(out.contains("datasets unavailable"));
    }

    #[test]
    fn wide_layout_shows_avail_and_compress() {
        let app = app_for_datasets(vec![fs("tank", vec![])], None);
        let out = render(&app, 100, 24);
        assert!(out.contains("AVAIL"));
        assert!(out.contains("COMPRESS"));
    }

    #[test]
    fn narrow_layout_drops_avail_and_compress() {
        let app = app_for_datasets(vec![fs("tank", vec![])], None);
        let out = render(&app, 80, 24);
        assert!(!out.contains(" AVAIL "), "narrow layout should not show AVAIL");
        assert!(!out.contains("COMPRESS"), "narrow layout should not show COMPRESS");
    }

    #[test]
    fn expanded_root_shows_expand_glyph() {
        let app = app_for_datasets(
            vec![fs("tank", vec![fs("tank/home", vec![])])],
            None,
        );
        let out = render(&app, 100, 24);
        assert!(out.contains("▼"), "expanded root should show ▼ glyph");
    }

    #[test]
    fn collapsed_node_shows_collapse_glyph() {
        let app = app_for_datasets(
            vec![fs(
                "tank",
                vec![fs("tank/home", vec![fs("tank/home/alice", vec![])])],
            )],
            None,
        );
        let out = render(&app, 100, 24);
        assert!(
            out.contains("▶"),
            "tank/home with children + not expanded should show ▶"
        );
    }

    #[test]
    fn zvol_row_shows_v_glyph() {
        let app = app_for_datasets(vec![fs("tank", vec![zvol("tank/swap")])], None);
        let out = render(&app, 100, 24);
        let line = out
            .lines()
            .find(|l| l.contains("tank/swap"))
            .expect("missing zvol row");
        assert!(line.contains(" V "), "zvol row should have V glyph: {line:?}");
    }

    #[test]
    fn selection_marker_on_selected_row() {
        let mut app = app_for_datasets(
            vec![fs("tank", vec![fs("tank/home", vec![])])],
            None,
        );
        if let DatasetsView::Tree { selected, .. } = &mut app.datasets_view {
            *selected = 1;
        }
        let out = render(&app, 100, 24);
        let line = out
            .lines()
            .find(|l| l.contains("tank/home"))
            .expect("missing tank/home row");
        assert!(line.contains('>'), "selected row missing > marker: {line:?}");
    }

    #[test]
    fn indentation_increases_with_depth() {
        let mut app = app_for_datasets(
            vec![fs(
                "tank",
                vec![fs("tank/home", vec![fs("tank/home/alice", vec![])])],
            )],
            None,
        );
        if let DatasetsView::Tree { expanded, .. } = &mut app.datasets_view {
            expanded.insert("tank/home".to_string());
        }
        let out = render(&app, 100, 24);
        let alice_line = out
            .lines()
            .find(|l| l.contains("tank/home/alice"))
            .expect("missing alice row");
        let home_line = out
            .lines()
            .find(|l| l.contains("tank/home") && !l.contains("tank/home/alice"))
            .expect("missing home row");
        // Indentation is expressed as spaces *before* the dataset name within
        // the name cell. Measure the byte position of the name in the line —
        // deeper nodes have more leading spaces so the name starts further right.
        let alice_pos = alice_line.find("tank/home/alice").expect("alice name missing");
        let home_pos = home_line.find("tank/home").expect("home name missing");
        assert!(
            alice_pos > home_pos,
            "alice col ({alice_pos}) should be > home col ({home_pos})"
        );
    }

    #[test]
    fn pool_root_with_zero_children_renders_as_bare_leaf() {
        let app = app_for_datasets(vec![fs("emptypool", vec![])], None);
        let out = render(&app, 100, 24);
        let line = out
            .lines()
            .find(|l| l.contains("emptypool"))
            .expect("missing emptypool row");
        assert!(
            !line.contains('▼') && !line.contains('▶'),
            "empty filesystem should not have expand glyph: {line:?}"
        );
    }
}
