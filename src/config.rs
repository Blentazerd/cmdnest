use std::fs;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use serde::Deserialize;

/// Все настройки CmdNest. Загружается из ~/.config/cmdnest/config.toml.
/// Если файла нет — создаётся дефолтный. Если файл битый — программа
/// работает с дефолтными значениями (никогда не падает из-за конфига).
#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub keys: Keys,
    pub colors: Colors,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct General {
    /// Префикс для хоткеев. Формат: "C-b" (Ctrl+B), "M-x" (Alt+X), "b" (просто B).
    pub prefix: String,
    /// Сколько строк истории хранить в scrollback.
    pub scrollback_lines: usize,
    /// Какой shell запускать. None = взять из $SHELL.
    pub shell: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Keys {
    /// Клавиша для вертикального сплита (после префикса).
    pub split_vertical: String,
    /// Клавиша для горизонтального сплита.
    pub split_horizontal: String,
    /// Новая вкладка.
    pub new_tab: String,
    /// Следующая вкладка.
    pub next_tab: String,
    /// Предыдущая вкладка.
    pub prev_tab: String,
    /// Закрыть вкладку.
    pub close_tab: String,
    /// Закрыть панель.
    pub close_pane: String,
    /// Переключить захват мыши (вкл/выкл).
    pub toggle_mouse: String,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Colors {
    pub border_focused: String,
    pub border_unfocused: String,
    pub status_bg: String,
    pub status_fg: String,
}

// ---------- Defaults ----------

impl Default for Config {
    fn default() -> Self {
        Config {
            general: General::default(),
            keys: Keys::default(),
            colors: Colors::default(),
        }
    }
}

impl Default for General {
    fn default() -> Self {
        General {
            prefix: "C-b".to_string(),
            scrollback_lines: 1000,
            shell: None,
        }
    }
}

impl Default for Keys {
    fn default() -> Self {
        Keys {
            split_vertical: "%".to_string(),
            split_horizontal: "\"".to_string(),
            new_tab: "c".to_string(),
            next_tab: "n".to_string(),
            prev_tab: "p".to_string(),
            close_tab: "&".to_string(),
            close_pane: "x".to_string(),
            toggle_mouse: "m".to_string(),
        }
    }
}

impl Default for Colors {
    fn default() -> Self {
        Colors {
            border_focused: "cyan".to_string(),
            border_unfocused: "darkgray".to_string(),
            status_bg: "cyan".to_string(),
            status_fg: "black".to_string(),
        }
    }
}

// ---------- Loading ----------

impl Config {
    /// Путь к конфигу: ~/.config/cmdnest/config.toml
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("cmdnest").join("config.toml"))
    }

    /// Загрузить конфиг. Если файла нет — создать дефолтный.
    /// Если файл битый — вывести ошибку в stderr и вернуть дефолт.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Config::default();
        };

        if !path.exists() {
            // Создаём директорию и дефолтный файл
            if let Some(dir) = path.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let default_text = DEFAULT_CONFIG_TEXT;
            if fs::write(&path, default_text).is_ok() {
                eprintln!("[cmdnest] Создан дефолтный конфиг: {}", path.display());
            }
            return Config::default();
        }

        match fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    eprintln!(
                        "[cmdnest] Ошибка в конфиге {}: {}. Использую значения по умолчанию.",
                        path.display(),
                        e
                    );
                    Config::default()
                }
            },
            Err(e) => {
                eprintln!(
                    "[cmdnest] Не удалось прочитать {}: {}. Использую значения по умолчанию.",
                    path.display(),
                    e
                );
                Config::default()
            }
        }
    }
}

// ---------- Helpers ----------

/// Проверяет, что нажатая клавиша соответствует строке вида "C-b", "M-x" или "b".
pub fn matches_key(key: &KeyEvent, spec: &str) -> bool {
    parse_key_spec(spec)
        .map(|(code, mods)| key.code == code && key.modifiers.contains(mods))
        .unwrap_or(false)
}

fn parse_key_spec(spec: &str) -> Option<(KeyCode, KeyModifiers)> {
    let mut mods = KeyModifiers::empty();
    let mut rest = spec;

    // Парсим префиксы модификаторов: C-, M-, S-
    loop {
        if let Some(r) = rest.strip_prefix("C-") {
            mods |= KeyModifiers::CONTROL;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("M-") {
            mods |= KeyModifiers::ALT;
            rest = r;
        } else if let Some(r) = rest.strip_prefix("S-") {
            mods |= KeyModifiers::SHIFT;
            rest = r;
        } else {
            break;
        }
    }

    // Один символ — Char, остальное игнорируем (для MVP)
    let mut chars = rest.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None; // больше одного символа — не поддерживаем
    }

    Some((KeyCode::Char(c), mods))
}

/// Проверяет, что в нажатой клавише есть указанный символ (без модификаторов).
/// Используется для хоткеев вроде "%", "\"", "c" после префикса.
pub fn char_matches(key: &KeyEvent, spec: &str) -> bool {
    let mut chars = spec.chars();
    let Some(c) = chars.next() else {
        return false;
    };
    if chars.next().is_some() {
        return false;
    }
    key.code == KeyCode::Char(c)
}

/// Парсит строку цвета в ratatui::style::Color.
pub fn parse_color(s: &str) -> Color {
    match s.to_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" | "dark_gray" | "dark_grey" => Color::DarkGray,
        "white" => Color::White,
        "reset" => Color::Reset,
        _ => Color::Reset,
    }
}

/// Шаблон конфига, который создаётся при первом запуске.
const DEFAULT_CONFIG_TEXT: &str = r#"# Конфигурация CmdNest
# Файл создаётся автоматически при первом запуске.
# После изменения перезапустите CmdNest, чтобы применить настройки.

[general]
# Префикс для хоткеев. Формат: "C-b" (Ctrl+B), "M-x" (Alt+X), "b" (просто B).
prefix = "C-b"

# Сколько строк истории хранить (scrollback).
scrollback_lines = 1000

# Какой shell запускать. Если закомментировать — возьмётся из $SHELL.
# shell = "/bin/zsh"

[keys]
# Все клавиши — одиночные символы, срабатывают после префикса.
split_vertical = "%"
split_horizontal = "\""
new_tab = "c"
next_tab = "n"
prev_tab = "p"
close_tab = "&"
close_pane = "x"
toggle_mouse = "m"


[colors]
# Цвета можно задавать именами: black, red, green, yellow, blue,
# magenta, cyan, gray, darkgray, white, reset.
border_focused = "cyan"
border_unfocused = "darkgray"
status_bg = "cyan"
status_fg = "black"
"#;
