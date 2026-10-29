//! Интеграционные тесты модуля `scanner`.
//!
//! Проверяют публичный API сканера через порт [`PhotoScanner`]:
//! три режима скана, фильтрацию по форматам, обработку ошибок.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use photo_explorer::core::models::{Photo, ScanMode};
use photo_explorer::core::ports::{PhotoScanner, ScanError};
use photo_explorer::scanner::FileSystemScanner;

/// Счётчик для гарантированно уникальных имён тестовых папок при параллельном запуске.
static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Создаёт временную папку с уникальным именем.
fn unique_temp_dir(prefix: &str) -> PathBuf {
    let n = DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("photo_explorer_{prefix}_{stamp}_{n}"))
}

/// Создаёт временную папку с тестовыми файлами и возвращает её путь.
/// Расширения: поддерживаемые (jpg, png, webp) и неподдерживаемые (txt, pdf).
fn make_test_dir() -> PathBuf {
    let dir = unique_temp_dir("test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("не удалось создать тестовую папку");

    // Поддерживаемые форматы (должны найтись)
    for name in ["a.jpg", "b.JPEG", "c.png", "d.WEBP"] {
        fs::write(dir.join(name), b"fake-image").expect("не удалось записать файл");
    }
    // Неподдерживаемые (должны быть пропущены)
    for name in ["notes.txt", "doc.pdf", "noext"] {
        fs::write(dir.join(name), b"not an image").expect("не удалось записать файл");
    }

    // Подпапка с фото (для проверки рекурсии)
    let sub = dir.join("sub");
    fs::create_dir_all(&sub).expect("не удалось создать подпапку");
    fs::write(sub.join("nested.gif"), b"gif-data").expect("не удалось записать файл");

    dir
}

#[test]
fn single_folder_finds_only_supported_images_without_subfolders() {
    let dir = make_test_dir();
    let scanner = FileSystemScanner::new();
    let photos = scanner
        .scan(&ScanMode::SingleFolder(dir.clone()))
        .expect("скан должен быть успешным");

    // Только 4 файла в корне, без подпапки
    assert_eq!(
        photos.len(),
        4,
        "должны найтись ровно 4 изображения в корне"
    );

    let names: Vec<&str> = photos.iter().map(|p| p.file_name.as_str()).collect();
    assert!(names.contains(&"a.jpg"));
    assert!(names.contains(&"b.JPEG"));
    assert!(names.contains(&"c.png"));
    assert!(names.contains(&"d.WEBP"));

    // Подпапка не должна быть просканирована в режиме SingleFolder
    assert!(!names.contains(&"nested.gif"));

    // Поля модели заполнены
    for photo in &photos {
        assert!(!photo.file_name.is_empty());
        assert!(photo.size_bytes > 0);
        assert!(photo.path.exists());
    }

    fs::remove_dir_all(dir).ok();
}

#[test]
fn folder_recursive_includes_subfolders() {
    let dir = make_test_dir();
    let scanner = FileSystemScanner::new();
    let photos = scanner
        .scan(&ScanMode::FolderRecursive(dir.clone()))
        .expect("скан должен быть успешным");

    // 4 в корне + 1 в подпапке
    assert_eq!(
        photos.len(),
        5,
        "рекурсивный скан должен найти 5 изображений"
    );
    let names: Vec<&str> = photos.iter().map(|p| p.file_name.as_str()).collect();
    assert!(names.contains(&"nested.gif"));

    fs::remove_dir_all(dir).ok();
}

#[test]
fn scan_returns_error_for_nonexistent_path() {
    let scanner = FileSystemScanner::new();
    let missing = PathBuf::from("Z:\\definitely_does_not_exist_12345");
    let result = scanner.scan(&ScanMode::SingleFolder(missing));

    assert!(matches!(result, Err(ScanError::PathNotFound(_))));
}

#[test]
fn scan_returns_error_when_path_is_a_file() {
    let dir = make_test_dir();
    let file_path = dir.join("a.jpg");

    let scanner = FileSystemScanner::new();
    let result = scanner.scan(&ScanMode::SingleFolder(file_path));

    assert!(matches!(result, Err(ScanError::NotADirectory(_))));

    fs::remove_dir_all(dir).ok();
}

// NOTE: тест all_drives удалён — режим AllDrives временно отключён по решению заказчика.
// Когда режим вернётся, тест можно восстановить (он был помечен #[ignore]).
#[test]
fn photo_model_is_clone_and_eq() {
    let p1 = Photo {
        path: PathBuf::from("C:/test.jpg"),
        file_name: "test.jpg".to_string(),
        size_bytes: 100,
        modified: 1700000000,
    };
    let p2 = p1.clone();
    assert_eq!(p1, p2);
}

#[test]
fn empty_folder_yields_empty_list() {
    let dir = unique_temp_dir("empty");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("не удалось создать пустую папку");

    let scanner = FileSystemScanner::new();
    let photos = scanner
        .scan(&ScanMode::SingleFolder(dir.clone()))
        .expect("скан пустой папки должен быть успешным");
    assert!(photos.is_empty(), "пустая папка должна дать пустой список");

    fs::remove_dir_all(dir).ok();
}
