//! Photo Explorer — библиотечный корень.
//!
//! Бизнес-логика и модули доступны как библиотека, чтобы их можно было
//! использовать из бинарника (`main.rs`) и из интеграционных тестов (`tests/`).

pub mod core;
pub mod logging;
pub mod scanner;
pub mod security;
pub mod thumbnails;
pub mod ui;
