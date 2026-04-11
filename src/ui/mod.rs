//! Top-level UI entry. Owns the tab strip, per-tab dispatch, and the footer.
//! Tab content rendering is delegated to per-tab modules (v0.2b: only
//! `arc_view` has real content; Overview and Pools are placeholders).

use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::DOT;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::Frame;

use crate::app::{App, Tab};

mod arc_view;

pub fn draw(frame: &mut Frame, app: &App) {
    let [tab_strip_area, content_area, footer_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(10),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_tab_strip(frame, tab_strip_area, app);

    match app.current_tab {
        Tab::Arc => arc_view::draw(frame, content_area, app),
        Tab::Overview => draw_placeholder(frame, content_area, "Overview"),
        Tab::Pools => draw_placeholder(frame, content_area, "Pools"),
    }

    draw_footer(frame, footer_area, app);
}

fn draw_tab_strip(frame: &mut Frame, area: Rect, app: &App) {
    let titles: Vec<Line> = Tab::ALL.iter().map(|t| Line::from(t.title())).collect();
    let selected = Tab::ALL
        .iter()
        .position(|t| *t == app.current_tab)
        .unwrap_or(0);
    let tabs = Tabs::new(titles)
        .select(selected)
        .divider(DOT)
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(tabs, area);
}

fn draw_placeholder(frame: &mut Frame, area: Rect, label: &'static str) {
    let block = Block::default().borders(Borders::ALL).title(label);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let text = Paragraph::new(Line::from(vec![
        Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" — coming in v0.2c"),
    ]))
    .alignment(Alignment::Center);
    if inner.height >= 1 {
        // Centre vertically: pick the middle row.
        let mid_row = Rect {
            x: inner.x,
            y: inner.y + inner.height / 2,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(text, mid_row);
    }
}

fn draw_footer(frame: &mut Frame, area: Rect, _app: &App) {
    let footer = Paragraph::new(Line::from(vec![
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(": quit  "),
        Span::styled("1/2/3", Style::default().fg(Color::Yellow)),
        Span::raw(": tabs  "),
        Span::styled("r", Style::default().fg(Color::Yellow)),
        Span::raw(": refresh"),
    ]));
    frame.render_widget(footer, area);
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
        let mut app = App::new(arc_reader, mem).expect("fixture App::new");
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
    fn tab_strip_shows_all_three_tab_titles() {
        let app = app_from_fixtures_on_tab(Tab::Arc);
        let terminal = draw_and_collect(&app, 80, 24);
        let row0 = row_text(terminal.backend(), 0);
        assert!(row0.contains("Overview"), "row0 = {row0:?}");
        assert!(row0.contains("ARC"), "row0 = {row0:?}");
        assert!(row0.contains("Pools"), "row0 = {row0:?}");
    }

    #[test]
    fn footer_shows_global_hints() {
        let app = app_from_fixtures_on_tab(Tab::Arc);
        let terminal = draw_and_collect(&app, 80, 24);
        let last = row_text(terminal.backend(), 23);
        assert!(last.contains("q"), "footer = {last:?}");
        assert!(last.contains("1/2/3"), "footer = {last:?}");
        assert!(last.contains("r"), "footer = {last:?}");
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
    fn overview_tab_shows_placeholder() {
        let app = app_from_fixtures_on_tab(Tab::Overview);
        let terminal = draw_and_collect(&app, 80, 24);
        let whole = whole_text(terminal.backend());
        assert!(whole.contains("Overview"), "missing Overview label");
        assert!(whole.contains("coming in v0.2c"), "missing placeholder text");
    }

    #[test]
    fn pools_tab_shows_placeholder() {
        let app = app_from_fixtures_on_tab(Tab::Pools);
        let terminal = draw_and_collect(&app, 80, 24);
        let whole = whole_text(terminal.backend());
        assert!(whole.contains("Pools"), "missing Pools label");
        assert!(whole.contains("coming in v0.2c"), "missing placeholder text");
    }
}
