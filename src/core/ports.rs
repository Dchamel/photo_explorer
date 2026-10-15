//! Порты (трейты-абстракции) ядра.
//!
//! Порт описывает ЧТО нужно уметь, но не КАК.
//! Реализация (КАК) находится в адаптерах: `scanner`, `thumbnails`, `security`.

use std::path::PathBuf;

use thiserror::Error;

use super::models::{Photo, ScanMode, Thumbnail};

/// Ошибки, возникающие при сканировании.
#[derive(Debug, Error)]
pub enum ScanError {
    /// Указанный путь не существует.
    #[error("указанный путь не существует: {0}")]
    PathNotFound(PathBuf),
    /// Указанный путь существует, но не является папкой.
    #[error("указанный путь не является папкой: {0}")]
    NotADirectory(PathBuf),
    /// Ошибка обхода файловой системы.
    #[error("ошибка обхода файловой системы: {0}")]
    WalkFailed(String),
}

/// Порт сканера фотографий.
///
/// Реализация — [`crate::scanner::FileSystemScanner`].
pub trait PhotoScanner {
    /// Сканирует файловую систему согласно `mode` и возвращает список фото.
    fn scan(&self, mode: &ScanMode) -> Result<Vec<Photo>, ScanError>;
}

/// Ошибки, возникающие при работе с миниатюрами.
#[derive(Debug, Error)]
pub enum ThumbnailError {
    /// Не удалось декодировать исходное изображение.
    #[error("не удалось декодировать изображение: {0}")]
    DecodeFailed(String),
    /// Не удалось изменить размер изображения.
    #[error("не удалось изменить размер: {0}")]
    ResizeFailed(String),
    /// Не удалось закодировать миниатюру для кэша.
    #[error("не удалось закодировать миниатюру: {0}")]
    EncodeFailed(String),
    /// Ошибка ввода-вывода (чтение/запись файла или кэша).
    #[error("ошибка ввода-вывода: {0}")]
    IoError(String),
}

/// Порт поставщика миниатюр.
///
/// Реализация — [`crate::thumbnails::FileSystemThumbnailProvider`].
pub trait ThumbnailProvider {
    /// Возвращает миниатюру для одного фото.
    fn thumbnail(&self, photo: &Photo) -> Result<Thumbnail, ThumbnailError>;

    /// Возвращает миниатюры для списка фото (параллельно, через `rayon`).
    ///
    /// Порядок результата совпадает с порядком входного списка.
    fn thumbnails(&self, photos: &[Photo]) -> Vec<Result<Thumbnail, ThumbnailError>>;
}

/// Ошибки, возникающие при аутентификации и защите доступа.
#[derive(Debug, Error)]
pub enum SecurityError {
    /// PIN не задан (попытка проверить, когда хеш не установлен).
    #[error("PIN не задан")]
    PinNotSet,
    /// PIN неверен.
    #[error("PIN неверен")]
    PinMismatch,
    /// Ошибка вычисления/проверки хеша Argon2.
    #[error("ошибка хеширования PIN: {0}")]
    HashError(String),
    /// Ошибка ввода-вывода (чтение/запись хранилища PIN).
    #[error("ошибка ввода-вывода: {0}")]
    IoError(String),
    /// Биометрия недоступна (нет сканера / не настроена / не Windows).
    #[error("биометрия недоступна: {0}")]
    BiometricUnavailable(String),
    /// Биометрия отклонена пользователем или прервана.
    #[error("биометрия отклонена: {0}")]
    BiometricDenied(String),
}

/// Результат попытки аутентификации.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    /// Доступ разрешён.
    Granted,
    /// Доступ отклонён (неверный PIN / отказ биометрии).
    Denied,
}

/// Порт аутентификатора: проверка PIN и биометрии.
///
/// Реализация — [`crate::security::SecurityService`].
pub trait Authenticator {
    /// Задать PIN (сохраняется только хеш Argon2, никогда сам PIN).
    fn set_pin(&self, pin: &str) -> Result<(), SecurityError>;

    /// Проверить PIN. Возвращает [`AuthOutcome::Granted`] или [`AuthOutcome::Denied`].
    fn verify_pin(&self, pin: &str) -> Result<AuthOutcome, SecurityError>;

    /// Установлен ли PIN (есть ли сохранённый хеш).
    fn has_pin(&self) -> bool;

    /// Запросить биометрию (Windows Hello). Возвращает Granted/Denied или ошибку,
    /// если биометрия недоступна.
    fn authenticate_biometric(&self) -> Result<AuthOutcome, SecurityError>;
}

/// Порт наблюдателя блокировки сессии Windows.
///
/// Реализация — [`crate::security::WindowsSessionLockWatcher`] (только на Windows).
pub trait SessionLockWatcher {
    /// Запустить наблюдение. `on_lock` вызывается при блокировке сессии (Win+L),
    /// `on_unlock` — при разблокировке.
    ///
    /// Блокирует текущий поток до остановки наблюдения.
    fn watch<F>(&self, on_lock: F, on_unlock: F) -> Result<(), SecurityError>
    where
        F: Fn() + Send + 'static;
}
