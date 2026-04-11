//! Top-level UI entry. Owns the tab strip, per-tab dispatch, and the footer.
//! Tab content rendering is delegated to per-tab modules (v0.2b: only
//! `arc_view` has real content; Overview and Pools are placeholders).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, PoolsView, Tab};

mod arc_view;
mod overview;
mod pools_detail;
mod pools_list;
mod widgets;

pub fn draw(frame: &mut Frame, app: &App) {
    let [title_area, tab_strip_area, content_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(10),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_title(frame, title_area, app);
    draw_tab_strip(frame, tab_strip_area, app);

    match app.current_tab {
        Tab::Arc => arc_view::draw(frame, content_area, app),
        Tab::Overview => overview::draw(frame, content_area, app),
        Tab::Pools => match app.pools_view {
            PoolsView::List { .. } => pools_list::draw(frame, content_area, app),
            PoolsView::Detail { .. } => pools_detail::draw(frame, content_area, app),
        },
    }

    draw_footer(frame, footer_area, app);
}

fn draw_title(frame: &mut Frame, area: Rect, _app: &App) {
    let title = Paragraph::new(Line::from(vec![
        Span::styled("zftop", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(" — v{}", env!("CARGO_PKG_VERSION"))),
    ]));
    frame.render_widget(title, area);
}

fn draw_tab_strip(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans: Vec<Span> = Vec::new();
    spans.push(Span::raw(" "));
    for (i, tab) in Tab::ALL.iter().enumerate() {
        let is_selected = *tab == app.current_tab;

        // Selected tab: bold black text on cyan background, making the whole
        // label pop off the row like a lit button. Unselected tabs are plain
        // white with the hotkey highlighted in yellow so the key binding
        // stays visible without drawing the eye away from the selection.
        let base_style = if is_selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let hotkey_style = if is_selected {
            base_style
        } else {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        };

        spans.push(Span::styled("[", base_style));
        spans.push(Span::styled(tab.hotkey().to_string(), hotkey_style));
        spans.push(Span::styled(" ", base_style));
        spans.push(Span::styled(tab.title(), base_style));
        spans.push(Span::styled("]", base_style));

        if i < Tab::ALL.len() - 1 {
            spans.push(Span::raw("  "));
        }
    }

    let paragraph = Paragraph::new(Line::from(spans));
    frame.render_widget(paragraph, area);
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let hint: Line = match (app.current_tab, app.pools_view) {
        (Tab::Pools, PoolsView::List { .. }) => Line::from(vec![
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::raw(": quit  "),
            Span::styled("1/2/3", Style::default().fg(Color::Yellow)),
            Span::raw(": tabs  "),
            Span::styled("↑↓", Style::default().fg(Color::Yellow)),
            Span::raw(": select  "),
            Span::styled("enter", Style::default().fg(Color::Yellow)),
            Span::raw(": details  "),
            Span::styled("r", Style::default().fg(Color::Yellow)),
            Span::raw(": refresh"),
        ]),
        (Tab::Pools, PoolsView::Detail { .. }) => Line::from(vec![
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::raw(": quit  "),
            Span::styled("1/2/3", Style::default().fg(Color::Yellow)),
            Span::raw(": tabs  "),
            Span::styled("esc", Style::default().fg(Color::Yellow)),
            Span::raw(": back  "),
            Span::styled("r", Style::default().fg(Color::Yellow)),
            Span::raw(": refresh"),
        ]),
        _ => Line::from(vec![
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::raw(": quit  "),
            Span::styled("1/2/3", Style::default().fg(Color::Yellow)),
            Span::raw(": tabs  "),
            Span::styled("r", Style::default().fg(Color::Yellow)),
            Span::raw(": refresh"),
        ]),
    };
    frame.render_widget(Paragraph::new(hint), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::arcstats;
    use crate::meminfo::{self, MemSource};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::path::PathBuf;

    fn app_from_fixtures_on_tab(tab: Tab) -> App {
        let arcstats_path = PathBuf::from("fixtures/arcstats");
        let meminfo_path = PathBuf::from("fixtures/meminfo");
        let arc_reader: Box<dyn FnMut() -> anyhow::Result<arcstats::ArcStats>> = {
            let p = arcstats_path.clone();
            Box::new(move || arcstats::linux::from_procfs_path(&p))
        };
        let mem: Option<Box<dyn MemSource>> = Some(Box::new(
            meminfo::linux::LinuxMemSource::new(meminfo_path),
        ));
        let mut app = App::new(arc_reader, mem, None, None).expect("fixture App::new");
        app.current_tab = tab;
        app
    }

    fn row_text(backend: &TestBackend, y: u16) -> String {
        let buf = backend.buffer();
        let width = buf.area.width;
        let mut s = String::with_capacity(width as usize);
        for x in 0..width {
            s.push_str(buf[(x, y)].symbol());
        }
        s
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

    fn draw_and_collect(app: &App, w: u16, h: u16) -> Terminal<TestBackend> {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, app)).expect("draw");
        terminal
    }

    #[test]
    fn title_row_shows_zftop_and_version() {
        let app = app_from_fixtures_on_tab(Tab::Arc);
        let terminal = draw_and_collect(&app, 80, 24);
        let row0 = row_text(terminal.backend(), 0);
        assert!(row0.contains("zftop"), "row0 = {row0:?}");
        // Version format is `v<CARGO_PKG_VERSION>`; in-tree that's `v0.0.0-dev`,
        // on a release CI build it's `v<tag>`. We just check for the `v` prefix
        // followed by a digit — stable across both build contexts.
        let has_version = row0
            .split_whitespace()
            .any(|w| w.starts_with('v') && w.chars().nth(1).is_some_and(|c| c.is_ascii_digit()));
        assert!(has_version, "row0 = {row0:?}");
    }

    #[test]
    fn tab_strip_shows_all_three_tab_titles() {
        let app = app_from_fixtures_on_tab(Tab::Arc);
        let terminal = draw_and_collect(&app, 80, 24);
        let row1 = row_text(terminal.backend(), 1);
        assert!(row1.contains("Overview"), "row1 = {row1:?}");
        assert!(row1.contains("ARC"), "row1 = {row1:?}");
        assert!(row1.contains("Pools"), "row1 = {row1:?}");
    }

    #[test]
    fn footer_on_overview_shows_global_hints() {
        let app = app_from_fixtures_on_tab(Tab::Overview);
        let terminal = draw_and_collect(&app, 80, 24);
        let last = row_text(terminal.backend(), 23);
        assert!(last.contains("q"));
        assert!(last.contains("1/2/3"));
        assert!(last.contains("r"));
        // Overview shouldn't show pool-nav keys.
        assert!(!last.contains("enter"));
        assert!(!last.contains("esc"));
    }

    #[test]
    fn footer_on_arc_shows_global_hints() {
        let app = app_from_fixtures_on_tab(Tab::Arc);
        let terminal = draw_and_collect(&app, 80, 24);
        let last = row_text(terminal.backend(), 23);
        assert!(last.contains("q"));
        assert!(last.contains("1/2/3"));
        assert!(last.contains("r"));
        assert!(!last.contains("enter"));
    }

    #[test]
    fn footer_on_pools_list_shows_selection_keys() {
        let app = app_from_fixtures_on_tab(Tab::Pools);
        let terminal = draw_and_collect(&app, 80, 24);
        let last = row_text(terminal.backend(), 23);
        assert!(last.contains("select"), "footer = {last:?}");
        assert!(last.contains("enter"), "footer = {last:?}");
        assert!(last.contains("details"), "footer = {last:?}");
    }

    #[test]
    fn footer_on_pools_detail_shows_esc_back() {
        use crate::app::PoolsView;
        let mut app = app_from_fixtures_on_tab(Tab::Pools);
        app.pools_view = PoolsView::Detail { pool_index: 0 };
        let terminal = draw_and_collect(&app, 80, 24);
        let last = row_text(terminal.backend(), 23);
        assert!(last.contains("esc"), "footer = {last:?}");
        assert!(last.contains("back"), "footer = {last:?}");
    }

    #[test]
    fn arc_tab_renders_v0_1_content_somewhere() {
        let app = app_from_fixtures_on_tab(Tab::Arc);
        let terminal = draw_and_collect(&app, 80, 24);
        let whole = whole_text(terminal.backend());
        assert!(whole.contains("Hit Ratios"), "missing Hit Ratios panel");
        assert!(whole.contains("Breakdown"), "missing Breakdown panel");
        assert!(whole.contains("ARC"), "missing ARC label");
    }

    #[test]
    fn overview_tab_renders_real_content() {
        let app = app_from_fixtures_on_tab(Tab::Overview);
        let terminal = draw_and_collect(&app, 80, 24);
        let whole = whole_text(terminal.backend());
        // Overview now shows 3 sections: System RAM, ARC gauge, Pools.
        // Without libzfs+fixture pools wired in here, the Pools section
        // shows either "(no pools imported)" or "libzfs unavailable".
        assert!(whole.contains("System RAM"), "missing System RAM block");
        assert!(whole.contains("Pools"), "missing Pools block");
        // The ARC gauge itself has "ARC" in its title.
        assert!(whole.contains("ARC"), "missing ARC label");
    }

    #[test]
    fn pools_tab_renders_real_content() {
        let app = app_from_fixtures_on_tab(Tab::Pools);
        let terminal = draw_and_collect(&app, 80, 24);
        let whole = whole_text(terminal.backend());
        // Without a real PoolsSource, this fixture app lands on the
        // empty-snapshot path which shows "(no pools imported)".
        assert!(whole.contains("Pools"), "missing Pools block title");
        assert!(
            whole.contains("no pools imported"),
            "expected empty-state notice, got: {whole:?}"
        );
    }
}
