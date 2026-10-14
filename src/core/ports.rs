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
