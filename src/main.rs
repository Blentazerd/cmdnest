mod app;
mod config;
mod layout;
mod pane;
mod ui;

use std::time::Duration;

use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use app::App;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Загружаем конфиг ПЕРЕД всеми остальными действиями
    let config = config::Config::load();

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // 2. Передаём ЕГО в App::new


    let mut app = App::new(config)?;

    // Первый кадр рисуем всегда
    let mut needs_draw = true;

    loop {
        // Рисуем только если: (а) было событие, (б) пришли новые данные из PTY
        if needs_draw || app.take_dirty() {
            terminal.draw(|f| ui::draw(f, &app))?;
            needs_draw = false;
        }

        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(key) => {
                    if key.code == KeyCode::Char('q')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }
                    app.handle_key(key, terminal.size()?)?;
                    needs_draw = true;
                }
                Event::Mouse(mouse) => {
                    app.handle_mouse(mouse);
                    needs_draw = true;
                }
                Event::Resize(cols, rows) => {
                    app.handle_resize(cols, rows);
                    needs_draw = true;
                }
                _ => {}
            }
        }
    }



    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}
