//! Интеграционные тесты модуля `thumbnails`.
//!
//! Создают настоящие PNG-файлы и проверяют генерацию миниатюр,
//! кэширование и параллельную пакетную обработку.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use image::{ImageBuffer, Rgba};

use photo_explorer::core::models::Photo;
use photo_explorer::core::ports::ThumbnailProvider;
use photo_explorer::thumbnails::FileSystemThumbnailProvider;

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let n = DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("photo_explorer_thumb_{prefix}_{stamp}_{n}"))
}

/// Создаёт PNG-файл заданного размера и возвращает путь к нему.
fn make_png(path: &Path, width: u32, height: u32) {
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_fn(width, height, |x, y| {
        Rgba([(x % 255) as u8, (y % 255) as u8, 128, 255])
    });
    img.save(path).expect("не удалось сохранить PNG");
}

fn make_photo(path: &Path) -> Photo {
    let metadata = fs::metadata(path).expect("файл должен существовать");
    Photo {
        path: path.to_path_buf(),
        file_name: path.file_name().unwrap().to_string_lossy().into_owned(),
        size_bytes: metadata.len(),
    }
}

#[test]
fn generates_thumbnail_smaller_than_source() {
    let dir = unique_temp_dir("gen");
    fs::create_dir_all(&dir).unwrap();
    let cache_dir = dir.join("cache");

    let source = dir.join("big.png");
    make_png(&source, 800, 600);
    let photo = make_photo(&source);

    let provider = FileSystemThumbnailProvider::new(cache_dir, 200);
    let thumb = provider
        .thumbnail(&photo)
        .expect("миниатюра должна сгенерироваться");

    assert_eq!(thumb.source_path, source);
    // При resize(200,200) с сохранением пропорций 800x600 -> 200x150.
    assert_eq!(thumb.width, 200);
    assert_eq!(thumb.height, 150);
    assert_eq!(thumb.data.len(), (200 * 150 * 4) as usize);

    fs::remove_dir_all(dir).ok();
}

#[test]
fn caches_thumbnail_to_disk() {
    let dir = unique_temp_dir("cache");
    fs::create_dir_all(&dir).unwrap();
    let cache_dir = dir.join("cache");

    let source = dir.join("img.png");
    make_png(&source, 400, 400);
    let photo = make_photo(&source);

    let provider = FileSystemThumbnailProvider::new(cache_dir.clone(), 100);

    // Первый вызов — генерация и запись в кэш.
    let thumb1 = provider.thumbnail(&photo).expect("первый вызов");
    assert!(cache_dir.exists(), "папка кэша должна быть создана");
    let cache_files: Vec<_> = fs::read_dir(&cache_dir).unwrap().collect();
    assert!(!cache_files.is_empty(), "в кэше должен быть файл");

    // Второй вызов — чтение из кэша (результат должен совпадать).
    let thumb2 = provider.thumbnail(&photo).expect("второй вызов");
    assert_eq!(thumb1, thumb2);

    fs::remove_dir_all(dir).ok();
}

#[test]
fn batch_thumbnails_preserve_order() {
    let dir = unique_temp_dir("batch");
    fs::create_dir_all(&dir).unwrap();
    let cache_dir = dir.join("cache");

    let mut photos = Vec::new();
    for i in 0..3 {
        let path = dir.join(format!("img{i}.png"));
        make_png(&path, 300, 200);
        photos.push(make_photo(&path));
    }

    let provider = FileSystemThumbnailProvider::new(cache_dir, 150);
    let results = provider.thumbnails(&photos);

    assert_eq!(results.len(), 3);
    for (i, res) in results.iter().enumerate() {
        let thumb = res.as_ref().expect("каждая миниатюра должна быть Ok");
        assert_eq!(thumb.source_path, photos[i].path);
    }

    fs::remove_dir_all(dir).ok();
}

#[test]
fn returns_error_for_corrupted_image() {
    let dir = unique_temp_dir("corrupt");
    fs::create_dir_all(&dir).unwrap();
    let cache_dir = dir.join("cache");

    let source = dir.join("not_an_image.png");
    fs::write(&source, b"this is not a png").unwrap();
    let photo = make_photo(&source);

    let provider = FileSystemThumbnailProvider::new(cache_dir, 100);
    let result = provider.thumbnail(&photo);
    assert!(result.is_err(), "повреждённый файл должен дать ошибку");

    fs::remove_dir_all(dir).ok();
}

#[test]
fn returns_error_for_nonexistent_source() {
    let dir = unique_temp_dir("missing");
    fs::create_dir_all(&dir).unwrap();
    let cache_dir = dir.join("cache");

    let photo = Photo {
        path: dir.join("ghost.png"),
        file_name: "ghost.png".to_string(),
        size_bytes: 0,
    };

    let provider = FileSystemThumbnailProvider::new(cache_dir, 100);
    let result = provider.thumbnail(&photo);
    assert!(result.is_err(), "несуществующий файл должен дать ошибку");

    fs::remove_dir_all(dir).ok();
}
