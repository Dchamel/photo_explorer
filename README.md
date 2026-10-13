<div align="center">

# 📷 Photo Explorer

**Быстрый и безопасный эксплорер фотографий для Windows 11**

Сканирует ваши папки с фото и показывает их красивой сеткой миниатюр.
Доступ защищён биометрией Windows Hello и PIN-кодом.

[![CI](https://img.shields.io/badge/CI-GitHub%20Actions-blue)]()
[![Language](https://img.shields.io/badge/Rust-🦀-orange)]()
[![UI](https://img.shields.io/badge/UI-Slint-brightgreen)]()
[![Platform](https://img.shields.io/badge/Platform-Windows%2011-blue)]()

</div>

---

## ✨ Возможности (MVP)

- 🗂️ **Три режима сканирования:**
  - все папки (по доступным дискам);
  - только выбранная папка;
  - выбранная папка со всеми подпапками.
- 🖼️ **Сетка миниатюр** с кэшем на диске и параллельной генерацией — быстро даже на больших коллекциях.
- 🔐 **Защита доступа:** вход по отпечатку пальца (Windows Hello) или PIN-коду.
- 🔒 **Автоблокировка:** при блокировке Windows (Win+L) приложение тоже блокируется.

> Планы на будущее: просмотр фото в полном размере, EXIF-метаданные, поиск и фильтры, шифрование хранилища.

---

## 🏗️ Архитектура

Проект построен по принципам **SOLID / DRY / KISS** и архитектуре **Hexagonal (Ports & Adapters)**:
ядро приложения не зависит от интерфейса, поэтому UI можно менять, не трогая бизнес-логику.

Подробности: [`doc/dev_plans/00_architecture_plan.md`](doc/dev_plans/00_architecture_plan.md).

```
photo_explorer/
├── src/           # исходный код (ядро + модули + UI)
├── doc/           # документация и планы разработки
├── tests/         # автотесты модулей
└── .github/       # CI/CD пайплайны
```

---

## 🛠️ Технологии

| Назначение | Технология |
|------------|-----------|
| Язык | [Rust](https://www.rust-lang.org/) 🦀 |
| Интерфейс | [Slint](https://slint.dev/) |
| Изображения | crate `image` |
| Обход файлов | crate `walkdir` |
| Параллелизм | crate `rayon` |
| Безопасность | crate `windows` (Windows Hello), `argon2` (PIN) |

---

## 🚀 Запуск

### Требования
- **ОС:** Windows 11
- **Rust** (устанавливается через [rustup](https://rustup.rs))
- **Visual Studio Build Tools** (компонент «Desktop development with C++»)
- **Ноутбук/ПК** со сканером отпечатка (для биометрии) — опционально, иначе используется PIN

> Пошаговая установка окружения: [`doc/dev_plans/01_rust_setup_windows.md`](doc/dev_plans/01_rust_setup_windows.md)

### Команды

```powershell
# Собрать и запустить
cargo run

# Собрать релизную (оптимизированную) версию
cargo build --release

# Запустить тесты
cargo test
```

---

## 🧪 Разработка

- Форматирование: `cargo fmt`
- Линтер: `cargo clippy`
- Все изменения проходят автоматическую проверку в CI (GitHub Actions).

---

## 📬 Контакты

Автор проекта — связь по вопросам и предложениям:
**dchamel@yandex.ru**

---

<div align="center">
Сделано с 🦀 Rust и ❤️
</div>
