// Ratatui rendering: bars, tables, layout.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::{format_bytes, App};

pub fn draw(frame: &mut Frame, app: &App) {
    let has_meminfo = app.mem_snapshot.is_some();

    // Top section: title + bars (full width)
    // Middle section: panels side by side
    // Bottom: footer
    let top_height = if has_meminfo { 7 } else { 4 }; // title + ram + gauge vs title + gauge

    let [top_area, middle_area, footer_area] = Layout::vertical([
        Constraint::Length(top_height),
        Constraint::Min(10),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    // -- Top: title + bars --
    if has_meminfo {
        let [title_area, ram_area, gauge_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .areas(top_area);
        draw_title(frame, title_area);
        draw_ram_bar(frame, ram_area, app);
        draw_gauge(frame, gauge_area, app);
    } else {
        let [title_area, gauge_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .areas(top_area);
        draw_title(frame, title_area);
        draw_gauge(frame, gauge_area, app);
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

    // -- Footer --
    let footer = Paragraph::new(Line::from(vec![
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(": quit  "),
        Span::styled("r", Style::default().fg(Color::Yellow)),
        Span::raw(": refresh"),
    ]));
    frame.render_widget(footer, footer_area);
}

fn draw_title(frame: &mut Frame, area: Rect) {
    let title = Paragraph::new(Line::from(vec![
        Span::styled("zfstop", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" — ARC"),
    ]));
    frame.render_widget(title, area);
}

fn draw_ram_bar(frame: &mut Frame, area: Rect, app: &App) {
    let Some((total_bytes, segments)) = app.ram_segments() else {
        return;
    };
    if total_bytes == 0 {
        return;
    }

    // Bottom title: total used + each segment with its label and percentage.
    let used_total: u64 = segments.iter().map(|s| s.bytes).sum();
    let used_pct = used_total as f64 / total_bytes as f64 * 100.0;

    let mut title_spans: Vec<Span> = Vec::with_capacity(1 + segments.len());
    title_spans.push(Span::raw(format!(
        " {}/{} ({:.1}%) ",
        format_bytes(used_total),
        format_bytes(total_bytes),
        used_pct,
    )));
    for seg in segments {
        title_spans.push(Span::styled(
            format!(
                "{} {} ({:.1}%) ",
                seg.label,
                format_bytes(seg.bytes),
                seg.bytes as f64 / total_bytes as f64 * 100.0,
            ),
            Style::default().fg(seg.color),
        ));
    }
    let bottom_title = Line::from(title_spans);

    let block = Block::default()
        .borders(Borders::ALL)
        .title("System RAM")
        .title_bottom(bottom_title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let bar_width = inner.width as usize;
    let mut bar_spans: Vec<Span> = Vec::new();
    let mut cols_used = 0;

    for seg in segments {
        let frac = seg.bytes as f64 / total_bytes as f64;
        let cols = (frac * bar_width as f64).round() as usize;
        let cols = cols.min(bar_width.saturating_sub(cols_used));
        if cols > 0 {
            bar_spans.push(Span::styled(
                "|".repeat(cols),
                Style::default().fg(seg.color),
            ));
            cols_used += cols;
        }
    }

    // Fill remaining with empty space (free).
    if cols_used < bar_width {
        bar_spans.push(Span::raw(" ".repeat(bar_width - cols_used)));
    }

    let bar_line = Line::from(bar_spans);
    frame.render_widget(Paragraph::new(bar_line), inner);
}

fn draw_gauge(frame: &mut Frame, area: Rect, app: &App) {
    let pct = app.arc_usage_pct();
    let label = format!(
        "ARC: {} / {} ({:.1}%)",
        format_bytes(app.current.size),
        format_bytes(app.current.c_max),
        pct * 100.0
    );
    let color = if pct > 0.9 {
        Color::Red
    } else if pct > 0.75 {
        Color::Yellow
    } else {
        Color::Green
    };
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title("ARC Size"))
        .gauge_style(Style::default().fg(color))
        .ratio(pct.min(1.0))
        .label(label);
    frame.render_widget(gauge, area);
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

    let header = Row::new(vec!["Category", "Size", "% of ARC"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);

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
            let color = if *pct >= 95.0 {
                Color::Green
            } else if *pct >= 80.0 {
                Color::Yellow
            } else {
                Color::Red
            };
            Row::new(vec![label.to_string(), format!("{:.2}%", pct)])
                .style(Style::default().fg(color))
        })
        .collect();

    let widths = [Constraint::Length(12), Constraint::Length(10)];

    let header = Row::new(vec!["Type", "Hit Ratio"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);

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
