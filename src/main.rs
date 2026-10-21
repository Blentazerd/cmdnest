// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Blentazerd
//
// CmdNest — минималистичный терминальный мультиплексор.
// Распространяется под GNU Affero General Public License v3.0 или новее.
// Полный текст — в файле LICENSE в корне проекта.


use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use vt100::Color as VtColor;

struct Pane {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let size = terminal.size()?;
    let init_rows = size.height.saturating_sub(3);
    let init_cols = size.width.saturating_sub(2);
    let mut pane = spawn_pty_pane(init_rows, init_cols)?;

    loop {
        terminal.draw(|f| {
            let area = f.area();
            let parser = pane.parser.lock().unwrap();
            let screen = parser.screen();

            let content_area = Rect::new(
                area.x + 1,
                area.y + 1,
                area.width.saturating_sub(2),
                area.height.saturating_sub(3),
            );

            // Собираем строки с цветами и стилями
            let lines: Vec<Line> = (0..content_area.height)
                .map(|row| build_line(screen, row, content_area.width))
                .collect();
            f.render_widget(Paragraph::new(lines), content_area);

            // Рамка
            f.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::DarkGray)),
                area,
            );

            // Статус-бар
            let status_area = Rect::new(area.x, area.y + area.height - 1, area.width, 1);
            let status = Paragraph::new(Line::from(vec![Span::styled(
                " Ctrl+Q: Выход ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]));
            f.render_widget(status, status_area);

            // НОВОЕ: рисуем курсор, если приложение его не скрыло
            if !screen.hide_cursor() {
                let (crow, ccol) = screen.cursor_position();
                if crow < content_area.height && ccol < content_area.width {
                    f.set_cursor_position(Position::new(
                        content_area.x + ccol,
                        content_area.y + crow,
                    ));
                }
            }
        })?;

        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('q')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }
                    let bytes = key_event_to_bytes(key);
                    if !bytes.is_empty() {
                        let _ = pane.writer.write_all(&bytes);
                        let _ = pane.writer.flush();
                    }
                }
                Event::Resize(cols, rows) => {
                    let pty_rows = rows.saturating_sub(3);
                    let pty_cols = cols.saturating_sub(2);
                    if pty_rows > 0 && pty_cols > 0 {
                        let _ = pane.master.resize(PtySize {
                            rows: pty_rows,
                            cols: pty_cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                        if let Ok(mut p) = pane.parser.lock() {
                            p.set_size(pty_rows, pty_cols);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Строит одну строку текста, группируя подряд идущие ячейки с одинаковым
/// стилем в один `Span` (это в разы эффективнее, чем один Span на символ).
fn build_line(screen: &vt100::Screen, row: u16, width: u16) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur_style: Option<Style> = None;
    let mut cur_text = String::new();

    for col in 0..width {
        let Some(cell) = screen.cell(row, col) else {
            continue;
        };
        let text = cell.contents();
        // Ячейка-продолжение wide-символа (например, второй байт для CJK).
        // Она пустая по содержимому, и визуально уже занята широким символом.
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

/// Преобразует атрибуты vt100-ячейки в ratatui Style.
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
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string());
    let mut cmd = CommandBuilder::new(&shell);
    // НОВОЕ: сообщаем программам, что мы поддерживаем цвета
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let _child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let parser = vt100::Parser::new(rows, cols, 0);
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
