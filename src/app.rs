use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, KeyCode, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::execute;

use ratatui::layout::{Rect, Size};

use crate::config::{self, Config};
use crate::layout::{find_neighbor, Layout, NavDir, ResizeDir, SplitDir};
use crate::pane::{key_event_to_bytes, Pane};

pub struct Tab {
    pub layout: Layout,
    pub panes: Vec<Pane>,
    pub focused: usize,
}

impl Tab {
    pub fn new(area: Rect, config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        let pane = Pane::new(
            area.height.saturating_sub(2),
            area.width.saturating_sub(2),
            config,
        )?;
        Ok(Tab {
            layout: Layout::Leaf(0),
            panes: vec![pane],
            focused: 0,
        })
    }

    pub fn pane_rects(&self, area: Rect) -> Vec<(usize, Rect)> {
        let mut out = Vec::new();
        self.layout.compute(area, &mut out);
        out
    }

    pub fn resize_all(&mut self, area: Rect) {
        for (idx, rect) in self.pane_rects(area) {
            if idx < self.panes.len() {
                self.panes[idx].resize(rect);
            }
        }
    }

    pub fn split(
        &mut self,
        dir: SplitDir,
        area: Rect,
        config: &Config,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let new_idx = self.panes.len();
        if self.layout.split_at(self.focused, new_idx, dir) {
            let new_pane = Pane::new(1, 1, config)?;
            self.panes.push(new_pane);
            self.focused = new_idx;
            self.resize_all(area);
        }
        Ok(())
    }

    pub fn close_focused(&mut self, area: Rect) {
        if self.panes.len() > 1 && self.layout.close_at(self.focused) {
            self.panes.remove(self.focused);
            self.layout.renumber(self.focused);
            if self.focused >= self.panes.len() {
                self.focused = self.panes.len() - 1;
            }
            self.resize_all(area);
        }
    }

    pub fn resize_focused(&mut self, dir: ResizeDir, area: Rect) -> bool {
        let delta = 0.03;
        if self.layout.adjust_ratio(self.focused, dir, delta) {
            self.resize_all(area);
            true
        } else {
            false
        }
    }
}

pub struct App {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub prefix_active: bool,
    pub main_area: Rect,
    pub config: Config,
    pub mouse_enabled: bool,
}

impl App {
    pub fn new(config: Config) -> Result<Self, Box<dyn std::error::Error>> {
        let (cols, rows) = crossterm::terminal::size()?;
        let main_area = Rect::new(0, 0, cols, rows.saturating_sub(1));
        let tab = Tab::new(main_area, &config)?;
        Ok(App {
            tabs: vec![tab],
            active: 0,
            prefix_active: false,
            main_area,
            config,
            mouse_enabled: true,
        })
    }

    /// Проверить все панели всех вкладок: есть ли где-то новые данные.
    pub fn take_dirty(&self) -> bool {
        let mut any = false;
        for tab in &self.tabs {
            for pane in &tab.panes {
                if pane.take_dirty() {
                    any = true;
                }
            }
        }
        any
    }

    pub fn current_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub fn current_tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    pub fn handle_key(
        &mut self,
        key: event::KeyEvent,
        size: Size,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.main_area = Rect::new(0, 0, size.width, size.height.saturating_sub(1));

        if self.prefix_active {
            self.prefix_active = false;

            let k = self.config.keys.clone();
            let area = self.main_area;

            // ---- вкладки ----
            if config::char_matches(&key, &k.new_tab) {
                let new_tab = Tab::new(area, &self.config)?;
                self.tabs.push(new_tab);
                self.active = self.tabs.len() - 1;
                return Ok(());
            }
            if config::char_matches(&key, &k.next_tab) {
                if self.active < self.tabs.len() - 1 {
                    self.active += 1;
                }
                return Ok(());
            }
            if config::char_matches(&key, &k.prev_tab) {
                if self.active > 0 {
                    self.active -= 1;
                }
                return Ok(());
            }
            if config::char_matches(&key, &k.close_tab) {
                if self.tabs.len() > 1 {
                    self.tabs.remove(self.active);
                    if self.active >= self.tabs.len() {
                        self.active = self.tabs.len() - 1;
                    }
                }
                return Ok(());
            }

            // ---- сплиты ----
            if config::char_matches(&key, &k.split_vertical) {
                let cfg = self.config.clone();
                self.current_tab_mut()
                    .split(SplitDir::Vertical, area, &cfg)?;
                return Ok(());
            }
            if config::char_matches(&key, &k.split_horizontal) {
                let cfg = self.config.clone();
                self.current_tab_mut()
                    .split(SplitDir::Horizontal, area, &cfg)?;
                return Ok(());
            }
            if config::char_matches(&key, &k.close_pane) {
                self.current_tab_mut().close_focused(area);
                return Ok(());
            }

            // ---- переключение мыши ----
            if config::char_matches(&key, &k.toggle_mouse) {
                self.mouse_enabled = !self.mouse_enabled;
                if self.mouse_enabled {
                    let _ = execute!(std::io::stdout(), EnableMouseCapture);
                } else {
                    let _ = execute!(std::io::stdout(), DisableMouseCapture);
                }
                return Ok(());
            }

            // ---- resize / фокус ----
            if key.modifiers.contains(KeyModifiers::ALT) {
                match key.code {
                    KeyCode::Left => {
                        self.resize_active(ResizeDir::Left);
                    }
                    KeyCode::Right => {
                        self.resize_active(ResizeDir::Right);
                    }
                    KeyCode::Up => {
                        self.resize_active(ResizeDir::Up);
                    }
                    KeyCode::Down => {
                        self.resize_active(ResizeDir::Down);
                    }
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Left => self.focus_dir(NavDir::Left),
                    KeyCode::Right => self.focus_dir(NavDir::Right),
                    KeyCode::Up => self.focus_dir(NavDir::Up),
                    KeyCode::Down => self.focus_dir(NavDir::Down),
                    KeyCode::Esc => {}
                    _ => {}
                }
            }
            return Ok(());
        }

        // Префикс?
        if config::matches_key(&key, &self.config.general.prefix) {
            self.prefix_active = true;
            return Ok(());
        }

        // Alt+стрелки — прямое переключение фокуса (без префикса)
        if key.modifiers.contains(KeyModifiers::ALT) {
            let dir = match key.code {
                KeyCode::Left => Some(NavDir::Left),
                KeyCode::Right => Some(NavDir::Right),
                KeyCode::Up => Some(NavDir::Up),
                KeyCode::Down => Some(NavDir::Down),
                _ => None,
            };
            if let Some(d) = dir {
                self.focus_dir(d);
            }
            return Ok(());
        }

        // Всё остальное — в PTY
        let bytes = key_event_to_bytes(key);
        if !bytes.is_empty() {
            let focused = self.current_tab().focused;
            if focused < self.current_tab().panes.len() {
                self.current_tab_mut().panes[focused].write_bytes(&bytes);
            }
        }
        Ok(())
    }

    fn focus_dir(&mut self, dir: NavDir) {
        let rects = self.current_tab().pane_rects(self.main_area);
        let focused = self.current_tab().focused;
        if let Some(n) = find_neighbor(&rects, focused, dir) {
            self.current_tab_mut().focused = n;
        }
    }

    pub fn resize_active(&mut self, dir: ResizeDir) -> bool {
        let area = self.main_area;
        self.current_tab_mut().resize_focused(dir, area)
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        // Если мышь отключена — игнорируем все события
        if !self.mouse_enabled {
            return;
        }

        // 1. Клик левой кнопкой — найти панель под курсором и переключить фокус
        if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
            let area = self.main_area;
            let rects = self.current_tab().pane_rects(area);

            for (idx, rect) in &rects {
                let inside_x = mouse.column >= rect.x && mouse.column < rect.x + rect.width;
                let inside_y = mouse.row >= rect.y && mouse.row < rect.y + rect.height;
                if inside_x && inside_y {
                    self.current_tab_mut().focused = *idx;
                    return;
                }
            }
            return;
        }

        // 2. Скролл — к активной панели
        let focused = self.current_tab().focused;
        let tab = self.current_tab_mut();
        if focused >= tab.panes.len() {
            return;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => tab.panes[focused].scroll_up(3),
            MouseEventKind::ScrollDown => tab.panes[focused].scroll_down(3),
            _ => {}
        }
    }

    pub fn handle_resize(&mut self, cols: u16, rows: u16) {
        self.main_area = Rect::new(0, 0, cols, rows.saturating_sub(1));
        for tab in &mut self.tabs {
            tab.resize_all(self.main_area);
        }
    }
}
