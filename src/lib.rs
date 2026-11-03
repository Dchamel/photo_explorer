//! Photo Explorer — библиотечный корень.
//!
//! Бизнес-логика и модули доступны как библиотека, чтобы их можно было
//! использовать из бинарника (`main.rs`) и из интеграционных тестов (`tests/`).

pub mod core;
pub mod logging;
// 4.7b — декод видео (Media Foundation): инфраструктура, нужна и
// миниатюрам (превью-кадр), и просмотрщику (первый кадр)
pub mod media;
pub mod scanner;
pub mod security;
// 4.7 — сессионное хранилище (проекты/недавние)
pub mod session;
// 4.6c — глобальные настройки (Ультра-скорость: ядра/батарея)
pub mod settings;
pub mod thumbnails;
pub mod ui;
