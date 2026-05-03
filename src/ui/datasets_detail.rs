//! Datasets tab — detail drilldown. Header (3 lines) + property grid
//! (2-column Table). `format_quota_value` formats quota usage cells
//! with a colour cue when usage crosses the warning/critical thresholds.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::{format_bytes, App, DatasetsView};
use crate::datasets::{DatasetKind, DatasetNode};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let name = match &app.datasets_view {
        DatasetsView::Detail { name, .. } => name.clone(),
        DatasetsView::Tree { .. } => return,
    };
    let block = Block::default().borders(Borders::ALL).title(name.clone());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(node) = find_dataset(&app.datasets_snapshot, &name) else {
        let p = Paragraph::new("(dataset no longer present)")
            .style(Style::default().fg(Color::Red));
        frame.render_widget(p, inner);
        return;
    };

    let [header_area, _gap, body_area] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .areas(inner);

    draw_header(frame, header_area, node);
    draw_property_grid(frame, body_area, node);
}

fn find_dataset<'a>(roots: &'a [DatasetNode], target: &str) -> Option<&'a DatasetNode> {
    fn walk<'a>(node: &'a DatasetNode, target: &str) -> Option<&'a DatasetNode> {
        if node.name == target {
            return Some(node);
        }
        for c in &node.children {
            if let Some(found) = walk(c, target) {
                return Some(found);
            }
        }
        None
    }
    for r in roots {
        if let Some(found) = walk(r, target) {
            return Some(found);
        }
    }
    None
}

fn draw_header(frame: &mut Frame, area: Rect, node: &DatasetNode) {
    let kind_label = match node.kind {
        DatasetKind::Filesystem => "Filesystem",
        DatasetKind::Volume => "Volume",
    };
    let line1 = format!(
        "{kind_label}    {} used    {} referenced    {} available",
        format_bytes(node.used_bytes),
        format_bytes(node.refer_bytes),
        format_bytes(node.available_bytes),
    );
    let line2 = match node.kind {
        DatasetKind::Filesystem => match &node.properties.mountpoint {
            Some(m) => format!("Mountpoint: {m}"),
            None => "Mountpoint: (none)".into(),
        },
        DatasetKind::Volume => format!("Device: /dev/zvol/{}", node.name),
    };
    let line3 = match &node.properties.creation_time {
        Some(t) => format!("Created:    {}", format_time_with_relative(*t)),
        None => "Created:    (unknown)".into(),
    };
    let lines = vec![Line::from(line1), Line::from(line2), Line::from(line3)];
    let p = Paragraph::new(lines);
    frame.render_widget(p, area);
}

fn format_time_with_relative(t: std::time::SystemTime) -> String {
    use std::time::UNIX_EPOCH;
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let date = format_unix_date(secs);
    let rel = match t.elapsed() {
        Ok(dur) => {
            let days = dur.as_secs() / 86_400;
            if days < 30 {
                format!("({days}d ago)")
            } else if days < 365 {
                format!("({} mo ago)", days / 30)
            } else {
                let y = days / 365;
                let m = (days % 365) / 30;
                if m > 0 {
                    format!("({y}y {m}mo ago)")
                } else {
                    format!("({y}y ago)")
                }
            }
        }
        Err(_) => String::new(),
    };
    format!("{date}  {rel}")
}

/// Minimal YYYY-MM-DD formatter so we don't pull in `chrono`. Uses
/// Howard Hinnant's "civil_from_days" algorithm; correct for any UTC
/// timestamp from 1970 forward.
fn format_unix_date(secs: u64) -> String {
    let days_since_epoch = (secs / 86_400) as i64;
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", y, m, d)
}

fn draw_property_grid(frame: &mut Frame, area: Rect, node: &DatasetNode) {
    let p = &node.properties;
    let mut rows: Vec<Row> = Vec::new();

    let bold = Style::default().add_modifier(Modifier::BOLD);
    let header = Row::new(vec![
        Span::styled("PROPERTY", bold),
        Span::styled("VALUE", bold),
    ]);

    if let Some(algo) = &p.compression_algorithm {
        rows.push(prop_row(
            "Compression",
            format!("{algo}  ({:.2}x ratio)", node.compression_ratio),
            Style::default(),
        ));
    } else {
        rows.push(prop_row("Compression", "—".into(), Style::default()));
    }

    match node.kind {
        DatasetKind::Filesystem => {
            if let Some(rs) = p.recordsize_bytes {
                rows.push(prop_row("Recordsize", format_bytes(rs), Style::default()));
            }
            if let Some(a) = p.atime_on {
                rows.push(prop_row(
                    "atime",
                    if a { "on".into() } else { "off".into() },
                    Style::default(),
                ));
            }
            if let Some(s) = &p.snapdir_visible {
                rows.push(prop_row(
                    "snapdir",
                    if *s { "visible".into() } else { "hidden".into() },
                    Style::default(),
                ));
            }
        }
        DatasetKind::Volume => {
            if let Some(vbs) = p.volblocksize_bytes {
                rows.push(prop_row("Volblocksize", format_bytes(vbs), Style::default()));
            }
        }
    }

    if let Some(s) = &p.sync_mode {
        rows.push(prop_row("Sync", s.clone(), Style::default()));
    }

    let (quota_text, quota_style) = format_quota_value(p.quota_bytes, node.used_bytes);
    rows.push(prop_row("Quota", quota_text, quota_style));

    let (refquota_text, refquota_style) = format_quota_value(p.refquota_bytes, node.refer_bytes);
    rows.push(prop_row("Refquota", refquota_text, refquota_style));

    rows.push(prop_row("Reservation", opt_bytes(p.reservation_bytes), Style::default()));
    rows.push(prop_row("Refreservation", opt_bytes(p.refreservation_bytes), Style::default()));
    if let Some(d) = p.dedup_on {
        rows.push(prop_row(
            "Dedup",
            if d { "on".into() } else { "off".into() },
            Style::default(),
        ));
    }
    if let Some(c) = p.copies {
        rows.push(prop_row("Copies", format!("{c}"), Style::default()));
    }
    if let Some(e) = &p.encryption_algorithm {
        rows.push(prop_row("Encryption", e.clone(), Style::default()));
    }

    let widths = [Constraint::Length(20), Constraint::Min(0)];
    let table = Table::new(rows, widths).header(header);
    frame.render_widget(table, area);
}

fn prop_row(name: &'static str, value: String, value_style: Style) -> Row<'static> {
    Row::new(vec![
        Span::raw(format!("  {name}")),
        Span::styled(value, value_style),
    ])
}

fn opt_bytes(b: Option<u64>) -> String {
    b.map(format_bytes).unwrap_or_else(|| "—".into())
}

/// Format a quota value cell. Returns (text, style). When no quota is
/// set returns ("—", default). When set returns "<limit>  (<used>
/// used, <pct>%)" with the style coloured red ≥90%, yellow ≥75%,
/// default otherwise.
pub(super) fn format_quota_value(
    limit: Option<u64>,
    used: u64,
) -> (String, Style) {
    let Some(limit) = limit else {
        return ("—".into(), Style::default());
    };
    let pct = if limit == 0 {
        0
    } else {
        ((used.saturating_mul(100)) / limit).min(999) as u32
    };
    let style = if pct >= 90 {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else if pct >= 75 {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    (
        format!(
            "{}  ({} used, {pct}%)",
            format_bytes(limit),
            format_bytes(used)
        ),
        style,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Tab};
    use crate::arcstats;
    use crate::datasets::fake::FakeDatasetsSource;
    use crate::datasets::types::DatasetProperties;
    use crate::datasets::DatasetsSource;
    use crate::meminfo::{self, MemSource};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    #[test]
    fn format_quota_value_none_returns_dash() {
        let (text, _style) = format_quota_value(None, 0);
        assert_eq!(text, "—");
    }

    #[test]
    fn format_quota_value_under_75_default_style() {
        let (text, style) = format_quota_value(Some(1000), 500);
        assert!(text.contains("50%"));
        assert_eq!(style, Style::default());
    }

    #[test]
    fn format_quota_value_75_to_89_yellow() {
        let (_text, style) = format_quota_value(Some(1000), 800);
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn format_quota_value_90_or_above_red_bold() {
        let (_text, style) = format_quota_value(Some(1000), 950);
        assert_eq!(style.fg, Some(Color::Red));
        assert!(style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn format_unix_date_known_dates() {
        // 2024-03-15 00:00 UTC = 1_710_460_800
        assert_eq!(format_unix_date(1_710_460_800), "2024-03-15");
        // 2026-05-02 00:00 UTC = 1_777_680_000
        assert_eq!(format_unix_date(1_777_680_000), "2026-05-02");
    }

    fn fs_with_props(name: &str, props: DatasetProperties) -> DatasetNode {
        DatasetNode {
            name: name.into(),
            kind: DatasetKind::Filesystem,
            used_bytes: 440 * 1024 * 1024 * 1024,
            refer_bytes: 440 * 1024 * 1024 * 1024,
            available_bytes: 7 * 1024_u64.pow(4),
            compression_ratio: 1.42,
            properties: props,
            children: vec![],
        }
    }

    fn vol_with_props(name: &str, props: DatasetProperties) -> DatasetNode {
        DatasetNode {
            name: name.into(),
            kind: DatasetKind::Volume,
            used_bytes: 8 * 1024 * 1024 * 1024,
            refer_bytes: 8 * 1024 * 1024 * 1024,
            available_bytes: 7 * 1024_u64.pow(4),
            compression_ratio: 1.0,
            properties: props,
            children: vec![],
        }
    }

    fn render_detail_for(node: DatasetNode) -> String {
        let arc_reader: Box<dyn FnMut() -> anyhow::Result<arcstats::ArcStats>> = {
            let p = PathBuf::from("fixtures/arcstats");
            Box::new(move || arcstats::linux::from_procfs_path(&p))
        };
        let mem: Option<Box<dyn MemSource>> = Some(Box::new(
            meminfo::linux::LinuxMemSource::new(PathBuf::from("fixtures/meminfo")),
        ));
        let ds: Option<Box<dyn DatasetsSource>> =
            Some(Box::new(FakeDatasetsSource::new(vec![node.clone()])));
        let mut app = App::new(arc_reader, mem, None, None, ds, None)
            .expect("fixture App::new");
        app.current_tab = Tab::Datasets;
        let mut expanded = BTreeSet::new();
        expanded.insert(node.name.clone());
        app.datasets_view = DatasetsView::Detail {
            name: node.name.clone(),
            expanded,
        };
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| crate::ui::draw(frame, &app))
            .expect("draw");
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn filesystem_detail_shows_mountpoint_line() {
        let mut props = DatasetProperties::default();
        props.mountpoint = Some("/tank/home/alice".into());
        let out = render_detail_for(fs_with_props("tank/home/alice", props));
        assert!(out.contains("Mountpoint: /tank/home/alice"));
    }

    #[test]
    fn volume_detail_shows_device_line() {
        let props = DatasetProperties::default();
        let out = render_detail_for(vol_with_props("tank/swap", props));
        assert!(out.contains("Device: /dev/zvol/tank/swap"));
    }

    #[test]
    fn volume_detail_omits_atime_and_snapdir_rows() {
        let mut props = DatasetProperties::default();
        props.volblocksize_bytes = Some(8192);
        let out = render_detail_for(vol_with_props("tank/swap", props));
        assert!(out.contains("Volblocksize"));
        assert!(!out.contains("atime"));
        assert!(!out.contains("snapdir"));
    }

    #[test]
    fn quota_row_shows_dash_when_unset() {
        let props = DatasetProperties::default();
        let out = render_detail_for(fs_with_props("tank/home/alice", props));
        let line = out
            .lines()
            .find(|l| l.contains("Quota"))
            .expect("missing Quota row");
        assert!(line.contains("—"), "expected dash on unset quota: {line:?}");
    }
}
