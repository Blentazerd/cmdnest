use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use vt100::Color as VtColor;

#[derive(Clone, Copy, PartialEq, Debug)]
enum SplitDir {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy, Debug)]
enum NavDir {
    Left,
    Right,
    Up,
    Down,
}

struct Pane {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    scroll_offset: usize,
}

impl Pane {
    fn resize(&mut self, rect: Rect) {
        let rows = rect.height.saturating_sub(2);
        let cols = rect.width.saturating_sub(2);
        if rows == 0 || cols == 0 {
            return;
        }
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
    }

    fn scroll_up(&mut self, lines: usize) {
        let max = {
            let mut p = match self.parser.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            p.set_scrollback(usize::MAX / 2);
            let m = p.screen().scrollback();
            p.set_scrollback(0);
            m
        };
        self.scroll_offset = (self.scroll_offset + lines).min(max);
    }

    fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[derive(Clone)]
enum Layout {
    Leaf(usize),
    Split(SplitDir, Box<Layout>, Box<Layout>),
}

impl Layout {
    fn compute(&self, area: Rect, out: &mut Vec<(usize, Rect)>) {
        match self {
            Layout::Leaf(idx) => out.push((*idx, area)),
            Layout::Split(dir, a, b) => {
                let (ra, rb) = split_area(area, *dir);
                a.compute(ra, out);
                b.compute(rb, out);
            }
        }
    }

    fn split_at(&mut self, target: usize, new_idx: usize, dir: SplitDir) -> bool {
        match self {
            Layout::Leaf(i) if *i == target => {
                let old = *i;
                *self = Layout::Split(
                    dir,
                    Box::new(Layout::Leaf(old)),
                    Box::new(Layout::Leaf(new_idx)),
                );
                true
            }
            Layout::Split(_, a, b) => {
                a.split_at(target, new_idx, dir) || b.split_at(target, new_idx, dir)
            }
            _ => false,
        }
    }

    fn close_at(&mut self, target: usize) -> bool {
        match self {
            Layout::Leaf(_) => false,
            Layout::Split(_, a, b) => {
                if let Layout::Leaf(i) = **a {
                    if i == target {
                        let sibling = std::mem::replace(b.as_mut(), Layout::Leaf(0));
                        *self = sibling;
                        return true;
                    }
                }
                if let Layout::Leaf(i) = **b {
                    if i == target {
                        let sibling = std::mem::replace(a.as_mut(), Layout::Leaf(0));
                        *self = sibling;
                        return true;
                    }
                }
                a.close_at(target) || b.close_at(target)
            }
        }
    }

    fn renumber(&mut self, removed: usize) {
        match self {
            Layout::Leaf(i) => {
                if *i > removed {
                    *i -= 1;
                }
            }
            Layout::Split(_, a, b) => {
                a.renumber(removed);
                b.renumber(removed);
            }
        }
    }
}

fn split_area(area: Rect, dir: SplitDir) -> (Rect, Rect) {
    match dir {
        SplitDir::Vertical => {
            let half = area.width / 2;
            let a = Rect::new(area.x, area.y, half, area.height);
            let b = Rect::new(area.x + half, area.y, area.width - half, area.height);
            (a, b)
        }
        SplitDir::Horizontal => {
            let half = area.height / 2;
            let a = Rect::new(area.x, area.y, area.width, half);
            let b = Rect::new(area.x, area.y + half, area.width, area.height - half);
            (a, b)
        }
    }
}

fn find_neighbor(rects: &[(usize, Rect)], focused: usize, dir: NavDir) -> Option<usize> {
    let focused_rect = rects.iter().find(|(i, _)| *i == focused)?.1;
    let fx = focused_rect.x as i32 + focused_rect.width as i32 / 2;
    let fy = focused_rect.y as i32 + focused_rect.height as i32 / 2;

    let mut best: Option<(usize, i32)> = None;

    for (idx, rect) in rects {
        if *idx == focused {
            continue;
        }
        let cx = rect.x as i32 + rect.width as i32 / 2;
        let cy = rect.y as i32 + rect.height as i32 / 2;
        let dx = cx - fx;
        let dy = cy - fy;

        let dist = match dir {
            NavDir::Left if dx < 0 => dx.abs() * 4 + dy.abs(),
            NavDir::Right if dx > 0 => dx * 4 + dy.abs(),
            NavDir::Up if dy < 0 => dy.abs() * 4 + dx.abs(),
            NavDir::Down if dy > 0 => dy * 4 + dx.abs(),
            _ => continue,
        };

        if best.map_or(true, |(_, bd)| dist < bd) {
            best = Some((*idx, dist));
        }
    }

    best.map(|(i, _)| i)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let size = terminal.size()?;
    let main_area = Rect::new(0, 0, size.width, size.height.saturating_sub(1));

    let mut panes: Vec<Pane> = vec![spawn_pty_pane(
        main_area.height.saturating_sub(2),
        main_area.width.saturating_sub(2),
    )?];
    let mut layout = Layout::Leaf(0);
    let mut focused: usize = 0;
    let mut prefix_active = false;

    loop {
        terminal.draw(|f| {
            let area = f.area();
            let main_area = Rect::new(0, 0, area.width, area.height.saturating_sub(1));

            let mut pane_rects: Vec<(usize, Rect)> = Vec::new();
            layout.compute(main_area, &mut pane_rects);

            for (idx, rect) in &pane_rects {
                if *idx >= panes.len() {
                    continue;
                }
                let pane = &panes[*idx];
                let is_focused = *idx == focused;
                let border_color = if is_focused { Color::Cyan } else { Color::DarkGray };

                // 1. Установить scrollback
                if pane.scroll_offset > 0 {
                    if let Ok(mut p) = pane.parser.lock() {
                        p.set_scrollback(pane.scroll_offset);
                    }
                }

                // 2. Прочитать экран и нарисовать
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

                // 3. Сбросить scrollback
                if pane.scroll_offset > 0 {
                    if let Ok(mut p) = pane.parser.lock() {
                        p.set_scrollback(0);
                    }
                }

                // 4. Рамка
                let title = format!(" {} ", idx + 1);
                f.render_widget(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(border_color))
                        .title(title),
                    *rect,
                );

                // 5. Курсор (только если панель активна и не в режиме scrollback)
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
            let (status_text, status_bg) = if prefix_active {
                (
                    " PREFIX: %=V  \"=H  x=close  arrows=focus  Esc=cancel ".to_string(),
                    Color::Yellow,
                )
            } else {
                (
                    format!(
                        " Pane {}/{} | Wheel: scroll | Alt+Arrows: switch | Ctrl+B: prefix ",
                        focused + 1,
                        panes.len()
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
        })?;

        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('q')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }

                    let sz = terminal.size()?;
                    let main_area = Rect::new(0, 0, sz.width, sz.height.saturating_sub(1));

                    if prefix_active {
                        prefix_active = false;
                        match key.code {
                            KeyCode::Char('%') | KeyCode::Char('|') => {
                                let new_idx = panes.len();
                                if layout.split_at(focused, new_idx, SplitDir::Vertical) {
                                    let new_pane = spawn_pty_pane(1, 1)?;
                                    panes.push(new_pane);
                                    focused = new_idx;
                                    resize_all(&mut panes, &layout, main_area);
                                }
                            }
                            KeyCode::Char('"') | KeyCode::Char('-') => {
                                let new_idx = panes.len();
                                if layout.split_at(focused, new_idx, SplitDir::Horizontal) {
                                    let new_pane = spawn_pty_pane(1, 1)?;
                                    panes.push(new_pane);
                                    focused = new_idx;
                                    resize_all(&mut panes, &layout, main_area);
                                }
                            }
                            KeyCode::Char('x') => {
                                if panes.len() > 1 && layout.close_at(focused) {
                                    panes.remove(focused);
                                    layout.renumber(focused);
                                    if focused >= panes.len() {
                                        focused = panes.len() - 1;
                                    }
                                    resize_all(&mut panes, &layout, main_area);
                                }
                            }
                            KeyCode::Left => {
                                let rects = collect_rects(&layout, main_area);
                                if let Some(n) = find_neighbor(&rects, focused, NavDir::Left) {
                                    focused = n;
                                }
                            }
                            KeyCode::Right => {
                                let rects = collect_rects(&layout, main_area);
                                if let Some(n) = find_neighbor(&rects, focused, NavDir::Right) {
                                    focused = n;
                                }
                            }
                            KeyCode::Up => {
                                let rects = collect_rects(&layout, main_area);
                                if let Some(n) = find_neighbor(&rects, focused, NavDir::Up) {
                                    focused = n;
                                }
                            }
                            KeyCode::Down => {
                                let rects = collect_rects(&layout, main_area);
                                if let Some(n) = find_neighbor(&rects, focused, NavDir::Down) {
                                    focused = n;
                                }
                            }
                            KeyCode::Esc => {}
                            _ => {}
                        }
                    } else if key.code == KeyCode::Char('b')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        prefix_active = true;
                    } else if key.modifiers.contains(KeyModifiers::ALT) {
                        let dir = match key.code {
                            KeyCode::Left => Some(NavDir::Left),
                            KeyCode::Right => Some(NavDir::Right),
                            KeyCode::Up => Some(NavDir::Up),
                            KeyCode::Down => Some(NavDir::Down),
                            _ => None,
                        };
                        if let Some(d) = dir {
                            let rects = collect_rects(&layout, main_area);
                            if let Some(n) = find_neighbor(&rects, focused, d) {
                                focused = n;
                            }
                        }
                    } else {
                        if focused < panes.len() {
                            let bytes = key_event_to_bytes(key);
                            if !bytes.is_empty() {
                                let _ = panes[focused].writer.write_all(&bytes);
                                let _ = panes[focused].writer.flush();
                            }
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    if focused < panes.len() {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => panes[focused].scroll_up(3),
                            MouseEventKind::ScrollDown => panes[focused].scroll_down(3),
                            _ => {}
                        }
                    }
                }
                Event::Resize(cols, rows) => {
                    let main_area = Rect::new(0, 0, cols, rows.saturating_sub(1));
                    resize_all(&mut panes, &layout, main_area);
                }
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

fn collect_rects(layout: &Layout, area: Rect) -> Vec<(usize, Rect)> {
    let mut out = Vec::new();
    layout.compute(area, &mut out);
    out
}

fn resize_all(panes: &mut [Pane], layout: &Layout, area: Rect) {
    for (idx, rect) in collect_rects(layout, area) {
        if idx < panes.len() {
            panes[idx].resize(rect);
        }
    }
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

fn spawn_pty_pane(rows: u16, cols: u16) -> Result<Pane, Box<dyn std::error::Error>> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let mut cmd = CommandBuilder::new(&shell);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let parser = vt100::Parser::new(rows.max(1), cols.max(1), 1000);
    let parser = Arc::new(Mutex::new(parser));

    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;

    let parser_clone = Arc::clone(&parser);
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut p) = parser_clone.lock() {
                        p.process(&buf[..n]);
                    }
                }
                Err(_) => break,
            }
        }
    });

    Ok(Pane {
        parser,
        writer,
        master: pair.master,
        child,
        scroll_offset: 0,
    })
}

fn key_event_to_bytes(key: event::KeyEvent) -> Vec<u8> {
    match key.code {
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                let c = c.to_ascii_lowercase();
                if c.is_ascii_alphabetic() {
                    return vec![(c as u8) - b'a' + 1];
                }
            }
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf).as_bytes().to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Up => vec![0x1b, b'[', b'A'],
        KeyCode::Down => vec![0x1b, b'[', b'B'],
        KeyCode::Right => vec![0x1b, b'[', b'C'],
        KeyCode::Left => vec![0x1b, b'[', b'D'],
        KeyCode::Home => vec![0x1b, b'[', b'H'],
        KeyCode::End => vec![0x1b, b'[', b'F'],
        KeyCode::Delete => vec![0x1b, b'[', b'3', b'~'],
        KeyCode::PageUp => vec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown => vec![0x1b, b'[', b'6', b'~'],
        _ => vec![],
    }
}
