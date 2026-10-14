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
    // NOTE: режим AllDrives временно отключён по решению заказчика (медленный,
    // пока не нужен — KISS). Код сохранён в scanner/mod.rs под комментарием,
    // чтобы можно было включить позже. Рабочие режимы — SingleFolder и FolderRecursive.
    // /// Скан всех папок по доступным дискам.
    // AllDrives,
    /// Скан только конкретной папки (без подпапок).
    SingleFolder(PathBuf),
    /// Скан конкретной папки и всех её подпапок.
    FolderRecursive(PathBuf),
}

/// Миниатюра изображения (готова для показа в UI).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    /// Путь к исходному файлу (для связи с [`Photo`]).
    pub source_path: PathBuf,
    /// Ширина миниатюры в пикселях.
    pub width: u32,
    /// Высота миниатюры в пикселях.
    pub height: u32,
    /// Пиксельные данные в формате RGBA (4 байта на пиксель: R, G, B, A).
    pub data: Vec<u8>,
}
