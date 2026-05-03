//! v0.1 ARC dashboard: RAM bar, ARC gauge, breakdown table, hit ratios,
//! compression, throughput. Owned layout-and-widgets; the parent `ui::draw`
//! hands this function a `Rect` and gets a rendered ARC screen back.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::{format_bytes, App};

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let has_meminfo = app.mem_snapshot.is_some();

    // Top section: RAM bar (if meminfo) + ARC gauge.
    // Middle section: panels side by side.
    // Title is owned by the parent `ui::draw` as a global header, not drawn
    // here. Footer is likewise owned by the parent.
    let top_height = if has_meminfo { 6 } else { 3 }; // ram + gauge vs gauge only

    let [top_area, middle_area] = Layout::vertical([
        Constraint::Length(top_height),
        Constraint::Min(10),
    ])
    .areas(area);

    // -- Top: bars --
    if has_meminfo {
        let [ram_area, gauge_area] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .areas(top_area);
        super::widgets::draw_ram_bar(frame, ram_area, app);
        super::widgets::draw_arc_gauge(frame, gauge_area, app);
    } else {
        super::widgets::draw_arc_gauge(frame, top_area, app);
    }

    // -- Middle: panels side by side --
    let [left_col, right_col] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .areas(middle_area);

    // Left column: Breakdown table (takes full height)
    draw_breakdown(frame, left_col, app);

    // Right column: Hit Ratios, Compression, Throughput stacked
    if has_meminfo {
        let [ratios_area, compression_area, throughput_area] = Layout::vertical([
            Constraint::Length(7),
            Constraint::Length(5),
            Constraint::Min(3),
        ])
        .areas(right_col);
        draw_hit_ratios(frame, ratios_area, app);
        draw_compression(frame, compression_area, app);
        draw_throughput(frame, throughput_area, app);
    } else {
        let [ratios_area, throughput_area] = Layout::vertical([
            Constraint::Length(7),
            Constraint::Min(3),
        ])
        .areas(right_col);
        draw_hit_ratios(frame, ratios_area, app);
        draw_throughput(frame, throughput_area, app);
    }
}

fn draw_breakdown(frame: &mut Frame, area: Rect, app: &App) {
    let rows: Vec<Row> = app
        .arc_breakdown()
        .iter()
        .map(|r| {
            Row::new(vec![
                r.label.to_string(),
                format_bytes(r.bytes),
                format!("{:.1}%", r.pct),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(12),
        Constraint::Length(14),
        Constraint::Length(8),
    ];

    let header = Row::new(vec!["Category", "Size", "% of ARC"]).style(
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Breakdown"));

    frame.render_widget(table, area);
}

fn draw_compression(frame: &mut Frame, area: Rect, app: &App) {
    let s = &app.current;
    let ratio_str = app
        .arc_compression_ratio()
        .map(|r| format!("{:.2}x", r))
        .unwrap_or_else(|| "N/A".to_string());

    let lines = vec![
        Line::from(vec![
            Span::styled("Compression:  ", Style::default().fg(Color::Cyan)),
            Span::raw(&ratio_str),
            Span::raw(format!(
                "  ({} -> {})",
                format_bytes(s.uncompressed_size),
                format_bytes(s.compressed_size)
            )),
        ]),
        Line::from(vec![
            Span::styled("Data:         ", Style::default().fg(Color::Cyan)),
            Span::raw(format_bytes(s.data_size)),
            Span::raw("    "),
            Span::styled("Metadata:  ", Style::default().fg(Color::Cyan)),
            Span::raw(format_bytes(s.metadata_size)),
        ]),
    ];

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title("ARC Compression"),
    );
    frame.render_widget(paragraph, area);
}

fn draw_hit_ratios(frame: &mut Frame, area: Rect, app: &App) {
    let ratios = [
        ("Overall", app.hit_ratio_overall()),
        ("Demand", app.hit_ratio_demand()),
        ("Prefetch", app.hit_ratio_prefetch()),
    ];

    let rows: Vec<Row> = ratios
        .iter()
        .map(|(label, pct)| {
            // Prefetch is speculative — its "good" range is workload-dependent
            // (sequential reads can hit 90%+, random-read workloads near 0 is
            // expected). Use looser thresholds so it isn't permanently red on
            // random-heavy workloads where there's nothing to fix.
            let (green_at, yellow_at) = if *label == "Prefetch" {
                (60.0, 30.0)
            } else {
                (95.0, 80.0)
            };
            let color = if *pct >= green_at {
                Color::Green
            } else if *pct >= yellow_at {
                Color::Yellow
            } else {
                Color::Red
            };
            Row::new(vec![label.to_string(), format!("{:.2}%", pct)])
                .style(Style::default().fg(color))
        })
        .collect();

    let widths = [Constraint::Length(12), Constraint::Length(10)];

    let header = Row::new(vec!["Type", "Hit Ratio"]).style(
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title("Hit Ratios"));

    frame.render_widget(table, area);
}

fn draw_throughput(frame: &mut Frame, area: Rect, app: &App) {
    let dash = "\u{2014}".to_string();
    let hits = app.throughput_hits().map(format_count).unwrap_or_else(|| dash.clone());
    let iohits = app.throughput_iohits().map(format_count).unwrap_or_else(|| dash.clone());
    let misses = app.throughput_misses().map(format_count).unwrap_or_else(|| dash.clone());

    let text = Line::from(vec![
        Span::styled("Hits/s: ", Style::default().fg(Color::Green)),
        Span::raw(&hits),
        Span::raw("    "),
        Span::styled("IO hits/s: ", Style::default().fg(Color::Yellow)),
        Span::raw(&iohits),
        Span::raw("    "),
        Span::styled("Misses/s: ", Style::default().fg(Color::Red)),
        Span::raw(&misses),
    ]);

    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title("Throughput"));
    frame.render_widget(paragraph, area);
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::arcstats;
    use crate::meminfo::{self, MemSource};
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use std::path::PathBuf;

    /// Serialize a rendered buffer to a newline-joined string of glyphs.
    /// One line per row, padded to the buffer width with spaces. No ANSI
    /// codes — styles are asserted separately when needed.
    fn buffer_to_string(buffer: &Buffer) -> String {
        let width = buffer.area.width as usize;
        let height = buffer.area.height as usize;
        let mut out = String::with_capacity((width + 1) * height);
        for y in 0..height {
            for x in 0..width {
                out.push_str(buffer[(x as u16, y as u16)].symbol());
            }
            out.push('\n');
        }
        out
    }

    /// Build an `App` populated from the checked-in Linux fixtures. Used by
    /// the golden test so the rendered output is deterministic and portable
    /// across dev hosts.
    fn app_from_fixtures() -> App {
        let arcstats_path = PathBuf::from("fixtures/arcstats");
        let meminfo_path = PathBuf::from("fixtures/meminfo");

        let arc_reader: Box<dyn FnMut() -> anyhow::Result<arcstats::ArcStats>> = {
            let p = arcstats_path.clone();
            Box::new(move || arcstats::linux::from_procfs_path(&p))
        };
        let mem: Option<Box<dyn MemSource>> = Some(Box::new(
            meminfo::linux::LinuxMemSource::new(meminfo_path),
        ));
        App::new(arc_reader, mem, None, None, None, None).expect("fixture App::new")
    }

    #[test]
    fn arc_view_content_matches_v0_1_golden() {
        let app = app_from_fixtures();
        let backend = TestBackend::new(80, 21);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw(frame, area, &app);
            })
            .expect("draw");
        let rendered = buffer_to_string(terminal.backend().buffer());
        let golden = include_str!("../../fixtures/golden/arc_view_content_v0_1.txt");
        if rendered != golden {
            eprintln!("--- rendered ---\n{rendered}");
            eprintln!("--- golden ---\n{golden}");
            panic!("ARC content render does not match golden; diff above");
        }
    }
}
