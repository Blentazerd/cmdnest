use ratatui::{
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use vt100::Color as VtColor;

use crate::app::App;

pub fn draw(f: &mut Frame, app: &App) {
    let area = f.area();
    let main_area = Rect::new(0, 0, area.width, area.height.saturating_sub(1));

    let tab = &app.tabs[app.active];
    let rects = tab.pane_rects(main_area);

    for (idx, rect) in &rects {
        if *idx >= tab.panes.len() {
            continue;
        }
        let pane = &tab.panes[*idx];
        let is_focused = *idx == tab.focused;
        let border_color = if is_focused {
            Color::Cyan
        } else {
            Color::DarkGray
        };

        if pane.scroll_offset > 0 {
            if let Ok(mut p) = pane.parser.lock() {
                p.set_scrollback(pane.scroll_offset);
            }
        }

        let (lines, hide_cursor, cursor_pos) = {
            let parser = match pane.parser.lock() {
                Ok(g) => g,
                Err(_) => continue,
            };
            let screen = parser.screen();
            let inner = Rect::new(
                rect.x + 1,
                rect.y + 1,
                rect.width.saturating_sub(2),
                rect.height.saturating_sub(2),
            );
            let lines: Vec<Line> = (0..inner.height)
                .map(|row| build_line(screen, row, inner.width))
                .collect();
            (lines, screen.hide_cursor(), screen.cursor_position())
        };

        let inner = Rect::new(
            rect.x + 1,
            rect.y + 1,
            rect.width.saturating_sub(2),
            rect.height.saturating_sub(2),
        );
        f.render_widget(Paragraph::new(lines), inner);

        if pane.scroll_offset > 0 {
            if let Ok(mut p) = pane.parser.lock() {
                p.set_scrollback(0);
            }
        }

        let title = format!(" {} ", idx + 1);
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(title),
            *rect,
        );

        if is_focused
            && !hide_cursor
            && pane.scroll_offset == 0
            && cursor_pos.0 < inner.height
            && cursor_pos.1 < inner.width
        {
            f.set_cursor_position(Position::new(
                inner.x + cursor_pos.1,
                inner.y + cursor_pos.0,
            ));
        }
    }

    let status_area = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
    let (status_text, status_bg) = if app.prefix_active {
        (
            " PREFIX: c=new tab  n/p=next/prev  &=close tab  %=V  \"=H  x=pane  Esc=cancel "
                .to_string(),
            Color::Yellow,
        )
    } else {
        (
            format!(
                " Tab {}/{} | Pane {}/{} | Wheel: scroll | Alt+Arrows: focus | Ctrl+B: prefix ",
                app.active + 1,
                app.tabs.len(),
                tab.focused + 1,
                tab.panes.len(),
            ),
            Color::Cyan,
        )
    };
    let status = Paragraph::new(Line::from(vec![Span::styled(
        status_text,
        Style::default()
            .fg(Color::Black)
            .bg(status_bg)
            .add_modifier(Modifier::BOLD),
    )]));
    f.render_widget(status, status_area);
}

fn build_line(screen: &vt100::Screen, row: u16, width: u16) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur_style: Option<Style> = None;
    let mut cur_text = String::new();

    for col in 0..width {
        let Some(cell) = screen.cell(row, col) else {
            continue;
        };
        let text = cell.contents();
        if text.is_empty() {
            continue;
        }
        let style = cell_style(cell);
        if Some(style) == cur_style {
            cur_text.push_str(&text);
        } else {
            if !cur_text.is_empty() {
                spans.push(Span::styled(cur_text, cur_style.unwrap()));
            }
            cur_style = Some(style);
            cur_text = text;
        }
    }
    if !cur_text.is_empty() {
        spans.push(Span::styled(cur_text, cur_style.unwrap()));
    }
    Line::from(spans)
}

fn cell_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default()
        .fg(to_ratatui_color(cell.fgcolor()))
        .bg(to_ratatui_color(cell.bgcolor()));
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

fn to_ratatui_color(c: VtColor) -> Color {
    match c {
        VtColor::Default => Color::Reset,
        VtColor::Idx(i) => Color::Indexed(i),
        VtColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
