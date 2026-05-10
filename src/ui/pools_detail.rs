//! Pools tab — per-pool detail view. Slimmed for v0.3.1: pool header
//! and indented vdev tree both moved to `pools_tree.rs`. Detail now
//! shows a Scrub block (active / finished / never) and a reserved
//! v0.5 SMART placeholder.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::app::{App, PoolsView, format_bytes};
use crate::pools::ScrubState;

pub(super) fn draw(frame: &mut Frame, area: Rect, app: &App) {
    let pool_index = match &app.pools_view {
        PoolsView::Detail { pool_index, .. } => *pool_index,
        PoolsView::Tree { selected, .. } => *selected, // defensive fallback
    };

    let Some(pool) = app.pools_snapshot.get(pool_index) else {
        let block = Block::default().borders(Borders::ALL).title("Pool Detail");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        super::widgets::draw_centered(
            frame,
            inner,
            "(no pool selected)",
            Style::default().fg(Color::DarkGray),
        );
        return;
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", pool.name));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Vertical layout: Scrub section header(1) + body(1) + spacer(1) +
    // SMART header(1) + body(1) + spacer(rest).
    let [
        scrub_header,
        scrub_body,
        _spacer1,
        smart_header,
        smart_body,
        _rest,
    ] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);

    let bold = Style::default().add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled("Scrub", bold),
        ])),
        scrub_header,
    );
    let scrub_text = format_scrub_header(&pool.scrub);
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::raw("   "), Span::raw(scrub_text)])),
        scrub_body,
    );

    let placeholder_style = Style::default().fg(Color::DarkGray);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled("SMART (v0.5)", bold),
        ])),
        smart_header,
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                "(per-disk SMART rollup will appear here in v0.5)",
                placeholder_style,
            ),
        ])),
        smart_body,
    );
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
                s.push_str(&format!(", ETA {}", format_eta(*eta)));
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

/// Human-friendly ETA renderer. Uses explicit unit labels (`s`, `m`, `h`,
/// `d`) instead of colon-separated digits so the user can never confuse
/// `HH:MM` with `MM:SS`. Always shows at least one non-zero unit when
/// `secs > 0`, so a sub-minute ETA surfaces as e.g. `45s` instead of
/// collapsing to `00:00`.
///
/// - `0..60`         → `"Ns"`              (e.g., `"45s"`)
/// - `60..3600`      → `"MmSSs"`           (e.g., `"2m33s"`)
/// - `3600..86400`   → `"HhMMm"`           (e.g., `"1h23m"`)
/// - `86400..`       → `"Dd HHh"`          (e.g., `"2d 5h"`)
fn format_eta(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{m}m{s:02}s")
    } else if secs < 86_400 {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{h}h{m:02}m")
    } else {
        let d = secs / 86_400;
        let h = (secs % 86_400) / 3600;
        format!("{d}d {h}h")
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
    use std::collections::BTreeSet;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    fn raidz_pool() -> PoolInfo {
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
                children: vec![],
                device_path: None,
            },
        }
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
        let mem: Option<Box<dyn MemSource>> =
            Some(Box::new(meminfo::linux::LinuxMemSource::new(meminfo_path)));
        let pools = vec![pool];
        let pools_source: Option<Box<dyn PoolsSource>> =
            Some(Box::new(FakePoolsSource::new(pools.clone())));
        let mut app =
            App::new(arc_reader, mem, pools_source, None, None, None).expect("fixture App::new");
        app.current_tab = Tab::Pools;
        app.pools_snapshot = pools;
        app.pools_view = PoolsView::Detail {
            pool_index: 0,
            expanded: BTreeSet::new(),
        };
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
            .draw(|frame| draw(frame, frame.area(), app))
            .expect("draw");
        whole_text(terminal.backend())
    }

    #[test]
    fn detail_renders_pool_name_in_block_title() {
        let app = app_for_detail(raidz_pool());
        let out = render_detail(&app);
        assert!(
            out.contains("tank"),
            "block title missing pool name: {out:?}"
        );
    }

    #[test]
    fn detail_shows_active_scrub_progress() {
        let app = app_for_detail(scrubbing_pool());
        let out = render_detail(&app);
        assert!(
            out.contains("Scrub: 42% complete"),
            "missing scrub progress: {out:?}"
        );
        assert!(
            out.contains("ETA 30m00s"),
            "missing ETA in scrub line: {out:?}"
        );
    }

    #[test]
    fn detail_shows_finished_scrub_summary() {
        let app = app_for_detail(raidz_pool());
        let out = render_detail(&app);
        assert!(
            out.contains("Last scrub:"),
            "missing finished-scrub summary: {out:?}"
        );
    }

    #[test]
    fn detail_shows_smart_placeholder() {
        let app = app_for_detail(raidz_pool());
        let out = render_detail(&app);
        assert!(
            out.contains("SMART (v0.5)"),
            "missing SMART section header: {out:?}"
        );
        assert!(
            out.contains("v0.5"),
            "placeholder missing v0.5 mention: {out:?}"
        );
    }

    #[test]
    fn detail_shows_no_pool_selected_when_index_out_of_range() {
        let mut app = app_for_detail(raidz_pool());
        app.pools_view = PoolsView::Detail {
            pool_index: 42,
            expanded: BTreeSet::new(),
        };
        let out = render_detail(&app);
        assert!(
            out.contains("no pool selected"),
            "missing fallback: {out:?}"
        );
    }

    // Existing format_eta tests — keep these unchanged.
    #[test]
    fn format_eta_shows_seconds_for_sub_minute_times() {
        assert_eq!(format_eta(0), "0s");
        assert_eq!(format_eta(1), "1s");
        assert_eq!(format_eta(45), "45s");
        assert_eq!(format_eta(59), "59s");
    }

    #[test]
    fn format_eta_uses_minute_second_below_an_hour() {
        assert_eq!(format_eta(60), "1m00s");
        assert_eq!(format_eta(153), "2m33s");
        assert_eq!(format_eta(1800), "30m00s");
        assert_eq!(format_eta(3599), "59m59s");
    }

    #[test]
    fn format_eta_uses_hour_minute_above_an_hour() {
        assert_eq!(format_eta(3600), "1h00m");
        assert_eq!(format_eta(3720), "1h02m");
        assert_eq!(format_eta(5 * 3600 + 30 * 60), "5h30m");
        assert_eq!(format_eta(86_399), "23h59m");
    }

    #[test]
    fn format_eta_uses_day_hour_for_multi_day_jobs() {
        assert_eq!(format_eta(86_400), "1d 0h");
        assert_eq!(format_eta(2 * 86_400 + 5 * 3600), "2d 5h");
    }
}
