//! Shared ratatui helpers. Anything drawn in more than one place — the RAM
//! bar on Overview + ARC, the ARC gauge on Overview + ARC, pool-health
//! coloring on Overview + Pools list + Pools detail — lives here.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

use crate::app::{format_bytes, App};
use crate::pools::PoolHealth;

pub(super) fn draw_ram_bar(frame: &mut Frame, area: Rect, app: &App) {
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
        " Total: {}/{} ({:.1}%) ",
        format_bytes(used_total),
        format_bytes(total_bytes),
        used_pct,
    )));
    for seg in segments {
        title_spans.push(Span::styled(
            format!("{} {} ", seg.label, format_bytes(seg.bytes)),
            Style::default().fg(seg.color),
        ));
    }
    let bottom_title = Line::from(title_spans);

    // Two sidecar values on the bottom-right, side by side:
    //   - "ARC headroom": c_max - size. ARC's self-imposed cap headroom —
    //     how much room before ARC hits its own ceiling.
    //   - "Kernel free": memory_available_bytes. Kernel-pressure headroom —
    //     how much room before the kernel starts squeezing ARC.
    // Actual ARC growth stops at the min of the two, but they answer
    // different questions (tuning vs. external pressure), so we surface both.
    let arc_headroom_bytes = app.current.c_max.saturating_sub(app.current.size);
    let sidecar_title = Line::from(format!(
        " ARC headroom: {}   Kernel free: {} ",
        format_bytes(arc_headroom_bytes),
        format_bytes(app.current.memory_available_bytes),
    ))
    .right_aligned();

    let block = Block::default()
        .borders(Borders::ALL)
        .title("System RAM")
        .title_bottom(bottom_title)
        .title_bottom(sidecar_title);
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

pub(super) fn draw_arc_gauge(frame: &mut Frame, area: Rect, app: &App) {
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

/// Style for rendering a `PoolHealth` label. Used by the Overview pools
/// section, the Pools list HEALTH column, and the Pools detail header.
#[allow(dead_code)] // Tasks 9-11 bring the first callers.
pub(super) fn pool_health_style(health: PoolHealth) -> Style {
    match health {
        PoolHealth::Online => Style::default().fg(Color::Green),
        PoolHealth::Degraded => Style::default().fg(Color::Yellow),
        PoolHealth::Faulted | PoolHealth::Removed | PoolHealth::Unavail => {
            Style::default().fg(Color::Red)
        }
        PoolHealth::Offline => Style::default().fg(Color::DarkGray),
    }
}
