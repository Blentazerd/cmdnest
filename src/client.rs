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
    paused_pane: Option<u64>,
    paused_buffer: Vec<u8>,
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
        ClientState {
            snap,
            parsers,
            paused_pane: None,
            paused_buffer: Vec::new(),
        }
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
                if self.paused_pane == Some(pane) {
                    self.paused_buffer.extend_from_slice(&data);
                    return;
                }
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

    fn resume_pane(&mut self) {
        if let Some(pid) = self.paused_pane.take() {
            let buf = std::mem::take(&mut self.paused_buffer);
            if let Some(pp) = self.parsers.get_mut(&pid) {
                pp.parser.process(&buf);
                pp.history.extend_from_slice(&buf);
                if pp.history.len() > 5_000_000 {
                    let ex = pp.history.len() - 5_000_000;
                    pp.history.drain(0..ex);
                }
            }
        }
    }
}

struct CopyMode {
    active: bool,
    scroll: usize,
    cursor_row: u16,
    cursor_col: u16,
    start: Option<(u16, u16)>,
    pane_id: u64,
}

impl CopyMode {
    fn new() -> Self {
        CopyMode {
            active: false,
            scroll: 0,
            cursor_row: 0,
            cursor_col: 0,
            start: None,
            pane_id: 0,
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
    let mut copy = CopyMode::new();

    let result = loop {
        if needs_draw || dirty.swap(false, Ordering::Relaxed) {
            let mut st = state.lock().unwrap();
            if copy.active {
                if let Some(pp) = st.parsers.get_mut(&copy.pane_id) {
                    pp.parser.set_scrollback(copy.scroll);
                }
            }
            let dr = terminal.draw(|f| render(f, &st, prefix_active, &copy));
            if copy.active {
                if let Some(pp) = st.parsers.get_mut(&copy.pane_id) {
                    pp.parser.set_scrollback(0);
                }
            }
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
                // ---- Copy mode ----
                if copy.active {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            copy.active = false;
                            if let Ok(mut st) = state.lock() {
                                st.resume_pane();
                            }
                        }
                        KeyCode::Char(' ') => {
                            copy.start = Some((copy.cursor_row, copy.cursor_col));
                        }

                        KeyCode::Enter => {
                            if let Some((sr, sc)) = copy.start {
                                let text = {
                                    let mut st = state.lock().unwrap();
                                    if let Some(pp) = st.parsers.get_mut(&copy.pane_id) {
                                        pp.parser.set_scrollback(copy.scroll);
                                        let s = extract_selection(
                                            pp.parser.screen(),
                                            (sr, sc),
                                            (copy.cursor_row, copy.cursor_col),
                                        );
                                        pp.parser.set_scrollback(0);
                                        s
                                    } else {
                                        String::new()
                                    }
                                };
                                if !text.is_empty() {
                                    let _ = copy_to_clipboard(&text);
                                }
                            }
                            copy.active = false;
                            if let Ok(mut st) = state.lock() {
                                st.resume_pane();
                            }
                        }

                        KeyCode::Up | KeyCode::Char('k') => {
                            if copy.cursor_row > 0 {
                                copy.cursor_row -= 1;
                            } else if copy.scroll > 0 {
                                // прокрутить viewport вверх
                                copy.scroll += 1;
                            }
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            let max_row = {
                                let st = state.lock().unwrap();
                                let pp = st.parsers.get(&copy.pane_id);
                                if let Some(p) = pp {
                                    p.parser.screen().size().0.saturating_sub(1)
                                } else {
                                    23
                                }
                            };
                            if copy.cursor_row < max_row {
                                copy.cursor_row += 1;
                            } else if copy.scroll > 0 {
                                copy.scroll -= 1;
                            }
                        }
                        KeyCode::Left | KeyCode::Char('h') => {
                            if copy.cursor_col > 0 {
                                copy.cursor_col -= 1;
                            }
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            let max_col = {
                                let st = state.lock().unwrap();
                                let pp = st.parsers.get(&copy.pane_id);
                                if let Some(p) = pp {
                                    p.parser.screen().size().1.saturating_sub(1)
                                } else {
                                    79
                                }
                            };
                            if copy.cursor_col < max_col {
                                copy.cursor_col += 1;
                            }
                        }
                        KeyCode::PageUp => {
                            copy.scroll += 20;
                        }
                        KeyCode::PageDown => {
                            copy.scroll = copy.scroll.saturating_sub(20);
                        }
                        KeyCode::Home => {
                            copy.cursor_col = 0;
                        }
                        KeyCode::End => {
                            let max_col = {
                                let st = state.lock().unwrap();
                                let pp = st.parsers.get(&copy.pane_id);
                                if let Some(p) = pp {
                                    p.parser.screen().size().1.saturating_sub(1)
                                } else {
                                    79
                                }
                            };
                            copy.cursor_col = max_col;
                        }
                        _ => {}
                    }
                    needs_draw = true;
                    continue;
                }

                // ---- Выход ----
                if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    let _ = writeln!(
                        writer,
                        "{}",
                        serde_json::to_string(&ClientMsg::Detach)?
                    );
                    break Ok(());
                }

                // ---- Prefix ----



                if prefix_active {
                    prefix_active = false;

                    // Zoom — только отправляем сообщение серверу
                    if key.code == KeyCode::Char('z') {
                        let st = state.lock().unwrap();
                        let zoomed = st
                            .snap
                            .tabs
                            .iter()
                            .find(|t| t.id == st.snap.active)
                            .map(|t| t.zoomed)
                            .unwrap_or(false);
                        drop(st);
                        let m = if zoomed {
                            ClientMsg::UnzoomPane
                        } else {
                            ClientMsg::ZoomPane
                        };
                        let _ = writeln!(writer, "{}", serde_json::to_string(&m)?);
                        needs_draw = true;
                        continue;
                    }

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
                        KeyCode::Char('[') => {
                            // вход в copy mode
                            let st = state.lock().unwrap();
                            if let Some(tab) = st.snap.tabs.iter().find(|t| t.id == st.snap.active) {
                                let pid = tab.focused;
                                let (scroll, h, w) = st
                                    .parsers
                                    .get(&pid)
                                    .map(|p| {
                                        let (hh, ww) = p.parser.screen().size();
                                        (p.parser.screen().scrollback(), hh, ww)
                                    })
                                    .unwrap_or((0, 24, 80));
                                drop(st);
                                copy.active = true;
                                copy.pane_id = pid;
                                copy.scroll = scroll;
                                copy.cursor_row = 0;
                                copy.cursor_col = 0;
                                copy.start = None;
                                let _ = (h, w);
                                let mut st2 = state.lock().unwrap();
                                st2.paused_pane = Some(pid);
                                st2.paused_buffer.clear();
                            }
                            None
                        }
                        KeyCode::Left => Some(ClientMsg::FocusPane { dir: NavDirP::Left }),
                        KeyCode::Right => Some(ClientMsg::FocusPane { dir: NavDirP::Right }),
                        KeyCode::Up => Some(ClientMsg::FocusPane { dir: NavDirP::Up }),
                        KeyCode::Down => Some(ClientMsg::FocusPane { dir: NavDirP::Down }),
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
                if copy.active {
                    match m.kind {
                        MouseEventKind::ScrollUp => copy.scroll += 3,
                        MouseEventKind::ScrollDown => {
                            copy.scroll = copy.scroll.saturating_sub(3);
                        }
                        _ => {}
                    }
                    needs_draw = true;
                    continue;
                }
                if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                    let st = state.lock().unwrap();
                    let mut hit = None;





                    if let Some(tab) = st.snap.tabs.iter().find(|t| t.id == st.snap.active) {
                        for p in &tab.panes {
                            if p.hidden {
                                continue;
                            }
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
                if copy.active {
                    copy.active = false;
                    if let Ok(mut st) = state.lock() {
                        st.resume_pane();
                    }
                }
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




fn render(f: &mut Frame, st: &ClientState, prefix_active: bool, copy: &CopyMode) {
    let area = f.area();
    let tab = match st.snap.tabs.iter().find(|t| t.id == st.snap.active) {
        Some(t) => t,
        None => return,
    };




    for pane in &tab.panes {
        if pane.hidden {
            continue;
        }
        let rect = Rect::new(pane.rect.x, pane.rect.y, pane.rect.w, pane.rect.h);
        let is_focused = pane.id == tab.focused;
        render_pane(f, st, pane, rect, is_focused, copy);
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

    let (text, bg) = if copy.active {
        let sel = if copy.start.is_some() { " [SELECTING] " } else { "" };
        (
            format!(
                " COPY MODE{} | Line {}:{} | Space=select Enter=copy q=exit ",
                sel, copy.cursor_row, copy.cursor_col
            ),
            Color::Yellow,
        )
    } else if prefix_active {
        (
            " PREFIX: c=new n/p=next %/\"=split x=close [=copy z=zoom d=detach ".to_string(),
            Color::Yellow,
        )
    } else {
        let zoom_tag = if tab.zoomed { "[ZOOM] " } else { "" };
        (
            format!(
                " {}Tab {}/{} | Pane {}/{} | C-b: prefix | Ctrl+Q: exit ",
                zoom_tag,
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

fn render_pane(
    f: &mut Frame,
    st: &ClientState,
    pane: &PaneSnap,
    rect: Rect,
    is_focused: bool,
    copy: &CopyMode,
) {
    let inner = Rect::new(
        rect.x + 1,
        rect.y + 1,
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    );

    let is_copy_pane = copy.active && copy.pane_id == pane.id;
    let sel = if is_copy_pane { copy.start } else { None };
    let cur = if is_copy_pane {
        Some((copy.cursor_row, copy.cursor_col))
    } else {
        None
    };

    if let Some(pp) = st.parsers.get(&pane.id) {
        let screen = pp.parser.screen();
        let lines: Vec<Line> = (0..inner.height)
            .map(|row| build_line(screen, row, inner.width, sel, cur))
            .collect();
        f.render_widget(Paragraph::new(lines), inner);

        if !is_copy_pane && is_focused && !screen.hide_cursor() {
            let (cr, cc) = screen.cursor_position();
            if cr < inner.height && cc < inner.width {
                f.set_cursor_position(Position::new(inner.x + cc, inner.y + cr));
            }
        } else if is_copy_pane && copy.cursor_row < inner.height && copy.cursor_col < inner.width
        {
            f.set_cursor_position(Position::new(
                inner.x + copy.cursor_col,
                inner.y + copy.cursor_row,
            ));
        }
    }

    let border = if is_copy_pane {
        Color::Yellow
    } else if is_focused {
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




fn build_line(
    screen: &vt100::Screen,
    row: u16,
    width: u16,
    sel: Option<(u16, u16)>,
    cur: Option<(u16, u16)>,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cur_style: Option<Style> = None;
    let mut cur_text = String::new();

    let (sel_sr, sel_sc, sel_er, sel_ec) = if let Some((sr, sc)) = sel {
        if let Some((cr, cc)) = cur {
            if (sr, sc) <= (cr, cc) {
                (sr, sc, cr, cc)
            } else {
                (cr, cc, sr, sc)
            }
        } else {
            (sr, sc, sr, sc)
        }
    } else {
        (0, 0, 0, 0)
    };

    for col in 0..width {
        let Some(cell) = screen.cell(row, col) else {
            continue;
        };
        let text = cell.contents();
        if text.is_empty() {
            continue;
        }
        let mut style = cell_style(cell);

        // Подсветка выделения
        if sel.is_some() {
            let in_sel = if row > sel_sr && row < sel_er {
                true
            } else if row == sel_sr && row == sel_er {
                col >= sel_sc && col <= sel_ec
            } else if row == sel_sr {
                col >= sel_sc
            } else if row == sel_er {
                col <= sel_ec
            } else {
                false
            };
            if in_sel {
                style = style.bg(Color::Blue).fg(Color::White);
            }
        }

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

fn extract_selection(screen: &vt100::Screen, start: (u16, u16), end: (u16, u16)) -> String {
    let (sr, sc, er, ec) = if start <= end {
        (start.0, start.1, end.0, end.1)
    } else {
        (end.0, end.1, start.0, start.1)
    };
    let mut result = String::new();
    for row in sr..=er {
        let row_start = if row == sr { sc } else { 0 };
        let row_end = if row == er { ec } else { 65535 };
        for col in row_start..=row_end {
            if let Some(cell) = screen.cell(row, col) {
                let c = cell.contents();
                if !c.is_empty() {
                    result.push_str(&c);
                }
            } else {
                break;
            }
        }
        if row < er {
            result.push('\n');
        }
    }
    result
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text).map_err(|e| e.to_string())
}
