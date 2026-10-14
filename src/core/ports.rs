//! Порты (трейты-абстракции) ядра.
//!
//! Порт описывает ЧТО нужно уметь, но не КАК.
//! Реализация (КАК) находится в адаптерах: `scanner`, `thumbnails`, `security`.

use std::path::PathBuf;

use thiserror::Error;

use super::models::{Photo, ScanMode};

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
