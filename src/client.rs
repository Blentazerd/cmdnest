use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::Duration;

use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame, Terminal,
};
use vt100::Color as VtColor;

use crate::protocol::*;

struct PaneParser {
    parser: vt100::Parser,
    history: Vec<u8>,
}

struct ClientState {
    snap: Snapshot,
    parsers: HashMap<u64, PaneParser>,
}

impl ClientState {
    fn new(snap: Snapshot) -> Self {
        let mut parsers = HashMap::new();
        for tab in &snap.tabs {
            for p in &tab.panes {
                let rows = p.rect.h.saturating_sub(2).max(1);
                let cols = p.rect.w.saturating_sub(2).max(1);
                let mut parser = vt100::Parser::new(rows, cols, 5000);
                parser.process(&p.history);
                parsers.insert(
                    p.id,
                    PaneParser {
                        parser,
                        history: p.history.clone(),
                    },
                );
            }
        }
        ClientState { snap, parsers }
    }

    fn apply(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Snapshot { snap } => {
                let mut new_parsers = HashMap::new();
                for tab in &snap.tabs {
                    for p in &tab.panes {
                        let rows = p.rect.h.saturating_sub(2).max(1);
                        let cols = p.rect.w.saturating_sub(2).max(1);
                        if let Some(old) = self.parsers.remove(&p.id) {
                            if old.parser.screen().size() != (rows, cols) {
                                let mut np = vt100::Parser::new(rows, cols, 5000);
                                np.process(&old.history);
                                new_parsers.insert(
                                    p.id,
                                    PaneParser {
                                        parser: np,
                                        history: old.history,
                                    },
                                );
                            } else {
                                new_parsers.insert(p.id, old);
                            }
                        } else {
                            let mut np = vt100::Parser::new(rows, cols, 5000);
                            np.process(&p.history);
                            new_parsers.insert(
                                p.id,
                                PaneParser {
                                    parser: np,
                                    history: p.history.clone(),
                                },
                            );
                        }
                    }
                }
                self.parsers = new_parsers;
                self.snap = snap;
            }
            ServerMsg::PaneOutput { tab, pane, data } => {
                if let Some(pp) = self.parsers.get_mut(&pane) {
                    pp.parser.process(&data);
                    pp.history.extend_from_slice(&data);
                    if pp.history.len() > 5_000_000 {
                        let ex = pp.history.len() - 5_000_000;
                        pp.history.drain(0..ex);
                    }
                }
                if let Some(t) = self.snap.tabs.iter_mut().find(|t| t.id == tab) {
                    if let Some(p) = t.panes.iter_mut().find(|p| p.id == pane) {
                        p.history.extend_from_slice(&data);
                        if p.history.len() > 5_000_000 {
                            let ex = p.history.len() - 5_000_000;
                            p.history.drain(0..ex);
                        }
                    }
                }
            }
        }
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let stream = UnixStream::connect(socket_path())?;
    let read_stream = stream.try_clone()?;
    let mut writer = stream;
    let reader = BufReader::new(read_stream);
    let mut lines = reader.lines();

    let snap: Snapshot = match lines.next() {
        Some(Ok(l)) => match serde_json::from_str::<ServerMsg>(&l)? {
            ServerMsg::Snapshot { snap } => snap,
            _ => return Err("expected snapshot".into()),
        },
        _ => return Err("no initial message".into()),
    };

    let state = Arc::new(Mutex::new(ClientState::new(snap)));
    let dirty = Arc::new(AtomicBool::new(true));

    let state_cl = Arc::clone(&state);
    let dirty_cl = Arc::clone(&dirty);
    thread::spawn(move || {
        for line in lines {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };
            let msg: ServerMsg = match serde_json::from_str(&line) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if let Ok(mut st) = state_cl.lock() {
                st.apply(msg);
            }
            dirty_cl.store(true, Ordering::Relaxed);
        }
    });

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let (term_cols, term_rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let _ = writeln!(
        writer,
        "{}",
        serde_json::to_string(&ClientMsg::Resize {
            cols: term_cols,
            rows: term_rows
        })?
    );

    let mut prefix_active = false;
    let mut needs_draw = true;

    let result = loop {
        if needs_draw || dirty.swap(false, Ordering::Relaxed) {
            let st = state.lock().unwrap();
            let dr = terminal.draw(|f| render(f, &st, prefix_active));
            drop(st);
            if let Err(e) = dr {
                eprintln!("draw failed: {}", e);
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
                    let _ = writeln!(
                        writer,
                        "{}",
                        serde_json::to_string(&ClientMsg::Detach).unwrap()
                    );
                    break Ok(());
                }



                if prefix_active {
                    prefix_active = false;
                    let msg = match key.code {
                        KeyCode::Char('d') => Some(ClientMsg::Detach),
                        KeyCode::Char('c') => Some(ClientMsg::NewTab),
                        KeyCode::Char('n') => Some(ClientMsg::NextTab),
                        KeyCode::Char('p') => Some(ClientMsg::PrevTab),
                        KeyCode::Char('&') => Some(ClientMsg::CloseTab),
                        KeyCode::Char('%') | KeyCode::Char('|') => Some(ClientMsg::SplitPane {
                            dir: SplitDirP::Vertical,
                        }),
                        KeyCode::Char('"') | KeyCode::Char('-') => Some(ClientMsg::SplitPane {
                            dir: SplitDirP::Horizontal,
                        }),
                        KeyCode::Char('x') => Some(ClientMsg::ClosePane),
                        KeyCode::Left => Some(ClientMsg::FocusPane {
                            dir: NavDirP::Left,
                        }),
                        KeyCode::Right => Some(ClientMsg::FocusPane {
                            dir: NavDirP::Right,
                        }),
                        KeyCode::Up => Some(ClientMsg::FocusPane { dir: NavDirP::Up }),
                        KeyCode::Down => Some(ClientMsg::FocusPane {
                            dir: NavDirP::Down,
                        }),
                        _ => None,
                    };
                    if let Some(m) = msg {
                        if matches!(m, ClientMsg::Detach) {
                            let _ = writeln!(writer, "{}", serde_json::to_string(&m)?);
                            break Ok(());
                        }
                        let _ = writeln!(writer, "{}", serde_json::to_string(&m)?);
                    }
                    needs_draw = true;
                    continue;
                }
                if key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    prefix_active = true;
                    needs_draw = true;
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::ALT) {
                    let dir = match key.code {
                        KeyCode::Left => Some(ResizeDirP::Left),
                        KeyCode::Right => Some(ResizeDirP::Right),
                        KeyCode::Up => Some(ResizeDirP::Up),
                        KeyCode::Down => Some(ResizeDirP::Down),
                        _ => None,
                    };
                    if let Some(d) = dir {
                        let _ = writeln!(
                            writer,
                            "{}",
                            serde_json::to_string(&ClientMsg::ResizePane { dir: d })?
                        );
                        continue;
                    }
                }
                let bytes = crate::pane::key_event_to_bytes(key);
                if !bytes.is_empty() {
                    let _ = writeln!(
                        writer,
                        "{}",
                        serde_json::to_string(&ClientMsg::Input { data: bytes })?
                    );
                }
            }
            Ok(Event::Mouse(m)) => {
                if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                    let st = state.lock().unwrap();
                    let mut hit = None;
                    if let Some(tab) = st.snap.tabs.iter().find(|t| t.id == st.snap.active) {
                        for p in &tab.panes {
                            let r = p.rect;
                            if m.column >= r.x
                                && m.column < r.x + r.w
                                && m.row >= r.y
                                && m.row < r.y + r.h
                            {
                                hit = Some(p.id);
                                break;
                            }
                        }
                    }
                    drop(st);
                    if let Some(pid) = hit {
                        let _ = writeln!(
                            writer,
                            "{}",
                            serde_json::to_string(&ClientMsg::SetFocus { pane_id: pid })?
                        );
                    }
                }
            }
            Ok(Event::Resize(c, r)) => {
                let _ = writeln!(
                    writer,
                    "{}",
                    serde_json::to_string(&ClientMsg::Resize { cols: c, rows: r })?
                );
                needs_draw = true;
            }
            _ => {}
        }
    };

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

fn render(f: &mut Frame, st: &ClientState, prefix_active: bool) {
    let area = f.area();
    let tab = match st.snap.tabs.iter().find(|t| t.id == st.snap.active) {
        Some(t) => t,
        None => return,
    };

    for pane in &tab.panes {
        let r = pane.rect;
        let rect = Rect::new(r.x, r.y, r.w, r.h);
        let inner = Rect::new(
            rect.x + 1,
            rect.y + 1,
            rect.width.saturating_sub(2),
            rect.height.saturating_sub(2),
        );

        if let Some(pp) = st.parsers.get(&pane.id) {
            let screen = pp.parser.screen();
            let lines: Vec<Line> = (0..inner.height)
                .map(|row| build_line(screen, row, inner.width))
                .collect();
            f.render_widget(Paragraph::new(lines), inner);

            if pane.id == tab.focused && !screen.hide_cursor() {
                let (cr, cc) = screen.cursor_position();
                if cr < inner.height && cc < inner.width {
                    f.set_cursor_position(Position::new(inner.x + cc, inner.y + cr));
                }
            }
        }

        let is_focused = pane.id == tab.focused;
        let border = if is_focused {
            Color::Cyan
        } else {
            Color::DarkGray
        };
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border)),
            rect,
        );
    }

    let status_area = Rect::new(0, area.height.saturating_sub(1), area.width, 1);
    let tab_idx = st
        .snap
        .tabs
        .iter()
        .position(|t| t.id == st.snap.active)
        .unwrap_or(0);
    let pane_idx = tab
        .panes
        .iter()
        .position(|p| p.id == tab.focused)
        .unwrap_or(0);
    let (text, bg) = if prefix_active {
        (
            " PREFIX: c=new-tab n/p=next/prev %/\"=split x=close d=detach Esc ".to_string(),
            Color::Yellow,
        )
    } else {
        (
            format!(
                " Tab {}/{} | Pane {}/{} | C-b: prefix | Ctrl+Q: exit ",
                tab_idx + 1,
                st.snap.tabs.len(),
                pane_idx + 1,
                tab.panes.len()
            ),
            Color::Cyan,
        )
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default()
                .fg(Color::Black)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ))),
        status_area,
    );
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
