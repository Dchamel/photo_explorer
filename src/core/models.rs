//! Доменные модели приложения.

use std::path::PathBuf;

/// Фотография (минимум для MVP).
///
/// Расширяемость (EXIF, дата съёмки, геолокация) — в бэклоге.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Photo {
    /// Абсолютный путь к файлу изображения.
    pub path: PathBuf,
    /// Имя файла (без пути).
    pub file_name: String,
    /// Размер файла в байтах.
    pub size_bytes: u64,
}

/// Режим сканирования папок.
#[derive(Debug, Clone)]
pub enum ScanMode {
    /// Скан всех папок по доступным дискам.
    AllDrives,
    /// Скан только конкретной папки (без подпапок).
    SingleFolder(PathBuf),
    /// Скан конкретной папки и всех её подпапок.
    FolderRecursive(PathBuf),
}
