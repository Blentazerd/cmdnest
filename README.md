# my-term-mux

Минималистичный терминальный мультиплексор на Rust (в разработке).

## Что умеет
- Запускает shell внутри собственного TUI
- Поддержка цветов и стилей (256 цветов, truecolor)
- Видимый курсор
- Динамический размер PTY

## Требования
- Arch Linux (или любой Linux)
- Rust (`sudo pacman -S rust cargo`)

## Сборка и запуск
git clone https://github.com/ТВОЙ_НИК/my-term-mux.git
cd my-term-mux
cargo run --release

## Управление
- `Ctrl+Q` — выход

## Лицензия
MIT (см. LICENSE)
