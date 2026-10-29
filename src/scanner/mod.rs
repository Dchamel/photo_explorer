//! Адаптер сканера фотографий: конкретная реализация порта [`PhotoScanner`].
//!
//! Использует `walkdir` для обхода файловой системы. Поддерживаемые форматы
//! изображений определены в [`SUPPORTED_EXTENSIONS`].

use std::path::Path;

use walkdir::WalkDir;

use crate::core::models::{Photo, ScanMode};
use crate::core::ports::{PhotoScanner, ScanError};

/// Поддерживаемые расширения изображений (в нижнем регистре, без точки).
/// Согласовано с заказчиком для MVP.
const SUPPORTED_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "bmp", "webp"];

/// Возвращает `true`, если путь указывает на файл с поддерживаемым расширением.
fn is_supported_image(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|&supported| ext.eq_ignore_ascii_case(supported))
        })
        .unwrap_or(false)
}

/// Превращает путь к файлу в доменную модель [`Photo`].
///
/// Возвращает `None`, если файл не существует или метаданные недоступны.
/// 4.7 — заполняем `modified` (unix-секунды) для fingerprint папки.
fn photo_from_path(path: &Path) -> Option<Photo> {
    if !path.is_file() {
        return None;
    }
    let metadata = std::fs::metadata(path).ok()?;
    let file_name = path.file_name()?.to_string_lossy().into_owned();
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(Photo {
        path: path.to_path_buf(),
        file_name,
        size_bytes: metadata.len(),
        modified,
    })
}

/// Проверяет, что путь существует и является папкой.
fn validate_folder(folder: &Path) -> Result<(), ScanError> {
    if !folder.exists() {
        return Err(ScanError::PathNotFound(folder.to_path_buf()));
    }
    if !folder.is_dir() {
        return Err(ScanError::NotADirectory(folder.to_path_buf()));
    }
    Ok(())
}

/// Собирает фото из одной папки без захода в подпапки.
fn photos_in_dir_flat(folder: &Path) -> Vec<Photo> {
    let mut photos = Vec::new();
    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten() {
            let path = entry.path();
            if is_supported_image(&path) {
                if let Some(photo) = photo_from_path(&path) {
                    photos.push(photo);
                }
            }
        }
    }
    photos
}

/// Собирает фото из дерева папок, начиная с `root`, рекурсивно.
fn photos_in_tree_recursive(root: &Path) -> Vec<Photo> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| is_supported_image(e.path()))
        .filter_map(|e| photo_from_path(e.path()))
        .collect()
}

// NOTE: режим AllDrives временно отключён по решению заказчика (медленный,
// пока не нужен — KISS). Код ниже сохранён как задел на будущее.
//
// /// Возвращает корни доступных дисков (Windows: `C:\`, `D:\` и т.д.).
// fn available_drives() -> Vec<PathBuf> {
//     let mut drives = Vec::new();
//     for letter in b'A'..=b'Z' {
//         let drive = PathBuf::from(format!("{}:\\", letter as char));
//         if drive.is_dir() {
//             drives.push(drive);
//         }
//     }
//     drives
// }
//
// /// Сканирует все доступные диски рекурсивно.
// ///
// /// Ошибки обхода отдельных файлов/папок (нет прав доступа и т.п.) пропускаются.
// fn scan_all_drives() -> Vec<Photo> {
//     available_drives()
//         .iter()
//         .flat_map(|drive| photos_in_tree_recursive(drive))
//         .collect()
// }

/// Адаптер сканера: реализация порта [`PhotoScanner`] поверх реальной ФС.
pub struct FileSystemScanner;

impl FileSystemScanner {
    /// Создаёт новый экземпляр сканера.
    pub fn new() -> Self {
        Self
    }
}

impl Default for FileSystemScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl PhotoScanner for FileSystemScanner {
    fn scan(&self, mode: &ScanMode) -> Result<Vec<Photo>, ScanError> {
        match mode {
            // ScanMode::AllDrives => Ok(scan_all_drives()),
            ScanMode::SingleFolder(folder) => {
                validate_folder(folder)?;
                Ok(photos_in_dir_flat(folder))
            }
            ScanMode::FolderRecursive(folder) => {
                validate_folder(folder)?;
                Ok(photos_in_tree_recursive(folder))
            }
        }
    }
}
