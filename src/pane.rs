use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    thread,
};

use crossterm::event::{self, KeyCode, KeyModifiers};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use ratatui::layout::Rect;

pub struct Pane {
    pub parser: Arc<Mutex<vt100::Parser>>,
    pub writer: Box<dyn Write + Send>,
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub scroll_offset: usize,
}

impl Pane {
    pub fn new(rows: u16, cols: u16) -> Result<Self, Box<dyn std::error::Error>> {
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

    pub fn resize(&mut self, rect: Rect) {
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

    pub fn scroll_up(&mut self, lines: usize) {
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

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

pub fn key_event_to_bytes(key: event::KeyEvent) -> Vec<u8> {
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
