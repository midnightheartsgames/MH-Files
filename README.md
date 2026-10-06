# MH Files

Файловый менеджер для Windows 10/11 на Rust: скорость и управление в духе File Pilot,
технические идеи OneCommander и визуальный язык
[MH Monitoring](https://github.com/midnightheartsgames/MH-Monitoring) и
[MH Sidebar](https://github.com/midnightheartsgames/MH-Sidebar). Один `.exe`, без
Electron/WebView и фоновых служб.

![MH Files](docs/screenshot.png)

## Возможности 0.1

* Панели делятся в любую сторону, у каждой свои вкладки; сеанс восстанавливается.
* Таблица и плитки с эскизами, «Этот компьютер» с заполнением дисков.
* Боковая панель: диски, известные папки, свои группы избранного.
* Строка пути со списком подпапок, ввод пути с `%переменными%`.
* Инспектор: картинки, текст, сведения; быстрый просмотр по пробелу.
* Палитра команд (Ctrl+Shift+P), GoTo (Ctrl+G), фильтр (Ctrl+F), поиск во вложенных папках
  (Ctrl+Shift+F).
* Пакетное переименование с правилами, переменными и проверкой имён Windows.
* Копирование, перемещение, корзина, конфликты и UAC — через Windows Shell (`IFileOperation`);
  буфер обмена совместим с Проводником; перетаскивание между панелями и из Проводника.
* Горячие клавиши настраиваются; окно настроек в стиле MH Monitoring.
* Папки на 100 000 файлов прокручиваются без задержек: список виртуальный, чтение потоковое.

Подробности, архитектура, правила проекта и планы — в [PLAN.md](PLAN.md).

## Сборка

```
cargo build --release
```

На Windows (MSVC) получается `target\release\MH-Files.exe` со значком, версией и манифестом
(длинные пути, PerMonitorV2). Под Linux программа собирается и запускается для разработки и
тестов — с упрощёнными заглушками вместо Windows Shell.

Проверки, как в CI:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo check --workspace --target x86_64-pc-windows-gnu
```

## Устройство

| Крейт | Что внутри |
|---|---|
| `crates/core` | модель без ввода-вывода: список, выделение, история, раскладка, сортировка, переименование, настройки |
| `crates/fs` | фоновые воркеры: чтение папок, поиск, предпросмотр, эскизы, наблюдение |
| `crates/platform` | Windows Shell, COM и Win32 за безопасным API |
| `crates/ui` | интерфейс на eframe/egui, `MH-Files.exe` |

Настройки и сеанс: `%APPDATA%\MH Files\`.

## Лицензия

MIT. Шрифт Cuprum — SIL Open Font License 1.1 (`assets/fonts/OFL.txt`).
