//! Адаптер поставщика миниатюр: реализация порта [`ThumbnailProvider`].
//!
//! Декодирует исходное изображение (через crate `image`), уменьшает его до
//! заданного размера с сохранением пропорций и кэширует результат на диске
//! в формате PNG. Пакетная обработка идёт параллельно через `rayon`.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
use rayon::prelude::*;

use crate::core::models::{Photo, Thumbnail};
use crate::core::ports::{ThumbnailError, ThumbnailProvider};

/// Адаптер миниатюр над реальной файловой системой.
///
/// Кэш хранится в `cache_dir` в виде PNG-файлов. Имя файла кэша — это хеш
/// абсолютного пути исходного изображения. Кэш считается валидным, если
/// файл кэша новее или того же возраста, что и исходный файл (по mtime).
pub struct FileSystemThumbnailProvider {
    /// Папка для хранения кэша миниатюр.
    cache_dir: PathBuf,
    /// Максимальный размер стороны миниатюры в пикселях.
    max_size: u32,
}

impl FileSystemThumbnailProvider {
    /// Создаёт новый поставщик миниатюр.
    ///
    /// Папка `cache_dir` будет создана при необходимости при первой записи.
    pub fn new(cache_dir: PathBuf, max_size: u32) -> Self {
        Self {
            cache_dir,
            max_size,
        }
    }

    /// Возвращает путь к файлу кэша для данного исходного изображения.
    fn cache_file_for(&self, source: &Path) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        source.hash(&mut hasher);
        let key = format!("{:016x}", hasher.finish());
        self.cache_dir.join(format!("{key}.png"))
    }

    /// Признак того, что кэш валиден (существует и не старее исходного файла).
    fn cache_is_valid(cache_file: &Path, source: &Path) -> bool {
        let Ok(cache_meta) = fs::metadata(cache_file) else {
            return false;
        };
        let Ok(source_meta) = fs::metadata(source) else {
            return false;
        };
        let Ok(cache_mtime) = cache_meta.modified() else {
            return false;
        };
        let Ok(source_mtime) = source_meta.modified() else {
            return false;
        };
        cache_mtime >= source_mtime
    }
}

impl ThumbnailProvider for FileSystemThumbnailProvider {
    fn thumbnail(&self, photo: &Photo) -> Result<Thumbnail, ThumbnailError> {
        let cache_file = self.cache_file_for(&photo.path);

        // Попытка прочитать из кэша.
        if Self::cache_is_valid(&cache_file, &photo.path) {
            if let Ok(thumb) = load_thumbnail_from_png(&cache_file, &photo.path) {
                return Ok(thumb);
            }
            // Если кэш повреждён — перегенерируем.
        }

        // Генерация миниатюры из исходного файла.
        let thumb = generate_thumbnail(&photo.path, self.max_size)?;

        // Сохранение в кэш (ошибка записи кэша не ломает результат).
        let _ = save_thumbnail_to_cache(&cache_file, &thumb);

        Ok(thumb)
    }

    fn thumbnails(&self, photos: &[Photo]) -> Vec<Result<Thumbnail, ThumbnailError>> {
        photos
            .par_iter()
            .map(|photo| self.thumbnail(photo))
            .collect()
    }
}

/// Декодирует исходное изображение и строит миниатюру с сохранением пропорций.
fn generate_thumbnail(source: &Path, max_size: u32) -> Result<Thumbnail, ThumbnailError> {
    let img = image::open(source).map_err(|e| ThumbnailError::DecodeFailed(e.to_string()))?;
    let resized = img.resize(max_size, max_size, image::imageops::FilterType::Lanczos3);
    let rgba = resized.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Thumbnail {
        source_path: source.to_path_buf(),
        width,
        height,
        data: rgba.into_raw(),
    })
}

/// Кодирует миниатюру в PNG и сохраняет в файл кэша.
fn save_thumbnail_to_cache(cache_file: &Path, thumb: &Thumbnail) -> Result<(), ThumbnailError> {
    if let Some(parent) = cache_file.parent() {
        fs::create_dir_all(parent).map_err(|e| ThumbnailError::IoError(e.to_string()))?;
    }
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
        ImageBuffer::from_raw(thumb.width, thumb.height, thumb.data.clone())
            .ok_or_else(|| ThumbnailError::EncodeFailed("несогласованный размер буфера".into()))?;
    let mut cursor = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(img)
        .write_to(&mut cursor, ImageFormat::Png)
        .map_err(|e| ThumbnailError::EncodeFailed(e.to_string()))?;
    fs::write(cache_file, cursor.into_inner())
        .map_err(|e| ThumbnailError::IoError(e.to_string()))?;
    Ok(())
}

/// Читает миниатюру из PNG-файла кэша.
fn load_thumbnail_from_png(cache_file: &Path, source: &Path) -> Result<Thumbnail, ThumbnailError> {
    let img = image::open(cache_file).map_err(|e| ThumbnailError::IoError(e.to_string()))?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Ok(Thumbnail {
        source_path: source.to_path_buf(),
        width,
        height,
        data: rgba.into_raw(),
    })
}
