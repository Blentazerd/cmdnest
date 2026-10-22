use crossterm::event::{
    self, KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Rect, Size};

use crate::layout::{find_neighbor, Layout, NavDir, SplitDir};
use crate::pane::{key_event_to_bytes, Pane};

pub struct Tab {
    pub layout: Layout,
    pub panes: Vec<Pane>,
    pub focused: usize,
}

impl Tab {
    pub fn new(area: Rect) -> Result<Self, Box<dyn std::error::Error>> {
        let pane = Pane::new(
            area.height.saturating_sub(2),
            area.width.saturating_sub(2),
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

    pub fn split(&mut self, dir: SplitDir, area: Rect) -> Result<(), Box<dyn std::error::Error>> {
        let new_idx = self.panes.len();
        if self.layout.split_at(self.focused, new_idx, dir) {
            let new_pane = Pane::new(1, 1)?;
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
}

pub struct App {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub prefix_active: bool,
    pub main_area: Rect,
}

impl App {
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let (cols, rows) = crossterm::terminal::size()?;
        let main_area = Rect::new(0, 0, cols, rows.saturating_sub(1));
        let tab = Tab::new(main_area)?;
        Ok(App {
            tabs: vec![tab],
            active: 0,
            prefix_active: false,
            main_area,
        })
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
            match key.code {
                // ---- вкладки ----
                KeyCode::Char('c') => {
                    let new_tab = Tab::new(self.main_area)?;
                    self.tabs.push(new_tab);
                    self.active = self.tabs.len() - 1;
                }
                KeyCode::Char('n') => {
                    if self.active < self.tabs.len() - 1 {
                        self.active += 1;
                    }
                }
                KeyCode::Char('p') => {
                    if self.active > 0 {
                        self.active -= 1;
                    }
                }
                KeyCode::Char('&') => {
                    if self.tabs.len() > 1 {
                        self.tabs.remove(self.active);
                        if self.active >= self.tabs.len() {
                            self.active = self.tabs.len() - 1;
                        }
                    }
                }
                // ---- сплиты ----

KeyCode::Char('%') | KeyCode::Char('|') => {
    let area = self.main_area;
    self.current_tab_mut().split(SplitDir::Vertical, area)?;
}
KeyCode::Char('"') | KeyCode::Char('-') => {
    let area = self.main_area;
    self.current_tab_mut().split(SplitDir::Horizontal, area)?;
}
KeyCode::Char('x') => {
    let area = self.main_area;
    self.current_tab_mut().close_focused(area);
}

                // ---- фокус ----
                KeyCode::Left => self.focus_dir(NavDir::Left),
                KeyCode::Right => self.focus_dir(NavDir::Right),
                KeyCode::Up => self.focus_dir(NavDir::Up),
                KeyCode::Down => self.focus_dir(NavDir::Down),
                KeyCode::Esc => {}
                _ => {}
            }
        } else if key.code == KeyCode::Char('b')
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.prefix_active = true;
        } else if key.modifiers.contains(KeyModifiers::ALT) {
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
        } else {
            let bytes = key_event_to_bytes(key);
            if !bytes.is_empty() {
                let focused = self.current_tab().focused;
                if focused < self.current_tab().panes.len() {
                    self.current_tab_mut().panes[focused].write_bytes(&bytes);
                }
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


pub fn handle_mouse(&mut self, mouse: MouseEvent) {
    // 1. Клик левой кнопкой — найти панель под курсором и переключить на неё фокус
    if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
        let area = self.main_area;
        let rects = self.current_tab().pane_rects(area);

        // Перебираем все панели: если точка клика попадает в прямоугольник — фокус туда
        for (idx, rect) in &rects {
            let inside_x = mouse.column >= rect.x && mouse.column < rect.x + rect.width;
            let inside_y = mouse.row >= rect.y && mouse.row < rect.y + rect.height;
            if inside_x && inside_y {
                self.current_tab_mut().focused = *idx;
                return;
            }
        }
        // Клик мимо панелей (статус-бар, за пределами) — ничего не делаем
        return;
    }

    // 2. Скролл — применяется к активной (focused) панели
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
