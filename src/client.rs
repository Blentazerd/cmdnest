use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers, MouseEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::Position,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Terminal,
};
use vt100::Color as VtColor;

use crate::protocol::*;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("[DEBUG] client::run starting");

    let stream = UnixStream::connect(socket_path())?;
    eprintln!("[DEBUG] connected");
    let read_stream = stream.try_clone()?;
    let mut writer = stream;
    let reader = BufReader::new(read_stream);

    let mut lines = reader.lines();
    let init: ServerMsg = match lines.next() {
        Some(Ok(l)) => {
            eprintln!("[DEBUG] got Init ({} bytes)", l.len());
            serde_json::from_str(&l)?
        }
        Some(Err(e)) => return Err(format!("read error: {}", e).into()),
        None => return Err("no init from server".into()),
    };

    let (initial_history, cols, rows) = match init {
        ServerMsg::Init { history, cols, rows } => {
            eprintln!("[DEBUG] Init {}x{}, hist {} bytes", cols, rows, history.len());
            (history, cols, rows)
        }
        _ => return Err("expected Init".into()),
    };

    let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((cols, rows));
    eprintln!("[DEBUG] real terminal = {}x{}", term_cols, term_rows);

    // Держим всю историю для re-processing при resize
    let history = Arc::new(Mutex::new(initial_history.clone()));

    // Парсер создаём сразу с реальным размером (иначе set_size очистит буфер)
    let parser = vt100::Parser::new(term_rows.max(1), term_cols.max(1), 5000);
    let parser = Arc::new(Mutex::new(parser));
    if let Ok(mut p) = parser.lock() {
        p.process(&initial_history);
    }
    eprintln!("[DEBUG] history processed");

    let dirty = Arc::new(AtomicBool::new(true));

    // Reader thread
    let parser_cl = Arc::clone(&parser);
    let dirty_cl = Arc::clone(&dirty);
    let history_cl = Arc::clone(&history);
    thread::spawn(move || {
        eprintln!("[DEBUG] reader thread started");
        for line in lines {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let msg: ServerMsg = match serde_json::from_str(&line) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if let ServerMsg::PtyOutput { data } = msg {
                if let Ok(mut h) = history_cl.lock() {
                    h.extend_from_slice(&data);
                    if h.len() > 5_000_000 {
                        let excess = h.len() - 5_000_000;
                        h.drain(0..excess);
                    }
                }
                if let Ok(mut p) = parser_cl.lock() {
                    p.process(&data);
                }
                dirty_cl.store(true, Ordering::Relaxed);
            }
        }
        eprintln!("[DEBUG] reader thread exit");
    });

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    eprintln!("[DEBUG] terminal ready");

    if term_cols != cols || term_rows != rows {
        let msg = serde_json::to_string(&ClientMsg::Resize {
            cols: term_cols,
            rows: term_rows,
        })?;
        let _ = writeln!(writer, "{}", msg);
    }

    let mut needs_draw = true;
    let mut scroll_offset: usize = 0;
    let mut prefix_active = false;

    let result = loop {
        if needs_draw || dirty.swap(false, Ordering::Relaxed) {
            let dr = terminal.draw(|f| {
                let area = f.area();
                if scroll_offset > 0 {
                    if let Ok(mut p) = parser.lock() {
                        p.set_scrollback(scroll_offset);
                    }
                }
                let (vlines, cursor_pos, hide_cursor) = {
                    let p = match parser.lock() {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                    let screen = p.screen();
                    let vlines: Vec<Line> = (0..area.height)
                        .map(|row| build_line(screen, row, area.width))
                        .collect();
                    (vlines, screen.cursor_position(), screen.hide_cursor())
                };
                if scroll_offset > 0 {
                    if let Ok(mut p) = parser.lock() {
                        p.set_scrollback(0);
                    }
                }
                f.render_widget(Paragraph::new(vlines), area);
                if !hide_cursor
                    && scroll_offset == 0
                    && cursor_pos.0 < area.height
                    && cursor_pos.1 < area.width
                {
                    f.set_cursor_position(Position::new(cursor_pos.1, cursor_pos.0));
                }
            });
            if let Err(e) = dr {
                eprintln!("[DEBUG] EXIT: draw failed: {}", e);
                break Ok(());
            }
            needs_draw = false;
        }

        if !event::poll(Duration::from_millis(16)).unwrap_or(false) {
            continue;
        }
        match event::read() {
            Ok(Event::Key(key)) => {
                if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    eprintln!("[DEBUG] EXIT: Ctrl+Q");
                    break Ok(());
                }
                if prefix_active {
                    prefix_active = false;
                    if key.code == KeyCode::Char('d') {
                        eprintln!("[DEBUG] EXIT: detach");
                        let _ = writeln!(
                            writer,
                            "{}",
                            serde_json::to_string(&ClientMsg::Detach).unwrap()
                        );
                        break Ok(());
                    }
                    continue;
                }
                if key.code == KeyCode::Char('b')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    prefix_active = true;
                    needs_draw = true;
                    continue;
                }
                let bytes = crate::pane::key_event_to_bytes(key);
                if !bytes.is_empty() {
                    let msg = serde_json::to_string(&ClientMsg::Input { data: bytes }).unwrap();
                    let _ = writeln!(writer, "{}", msg);
                }
            }
            Ok(Event::Mouse(m)) => match m.kind {
                MouseEventKind::ScrollUp => {
                    scroll_offset += 3;
                    needs_draw = true;
                }
                MouseEventKind::ScrollDown => {
                    scroll_offset = scroll_offset.saturating_sub(3);
                    needs_draw = true;
                }
                _ => {}
            },
            Ok(Event::Resize(c, r)) => {
                // Пересоздаём парсер с новым размером и переобрабатываем историю,
                // иначе set_size очистит буфер и экран станет пустым.
                let mut new_parser = vt100::Parser::new(r.max(1), c.max(1), 5000);
                if let Ok(hist) = history.lock() {
                    new_parser.process(&hist);
                }
                if let Ok(mut p) = parser.lock() {
                    *p = new_parser;
                }
                let msg = serde_json::to_string(&ClientMsg::Resize { cols: c, rows: r }).unwrap();
                let _ = writeln!(writer, "{}", msg);
                needs_draw = true;
                eprintln!("[DEBUG] resized to {}x{}", c, r);
            }
            _ => {}
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    eprintln!("[DEBUG] client exiting");
    result
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
