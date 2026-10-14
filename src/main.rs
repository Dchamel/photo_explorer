//! Photo Explorer — точка входа приложения.
//!
//! На данном этапе (Этап 2) демонстрируется работа модуля сканера:
//! сканируется папка проекта (рекурсивно) и выводится количество найденных
//! изображений. Полный UI появится на Этапе 3.

use photo_explorer::core::models::ScanMode;
use photo_explorer::core::ports::PhotoScanner;
use photo_explorer::scanner::FileSystemScanner;

fn main() {
    println!("📷 Photo Explorer — Этап 2: модуль scanner");

    let scanner = FileSystemScanner::new();
    let project_dir = std::env::current_dir().expect("не удалось получить текущую папку");
    let mode = ScanMode::FolderRecursive(project_dir.clone());

    match scanner.scan(&mode) {
        Ok(photos) => {
            println!("Папка: {}", project_dir.display());
            println!("Найдено изображений: {}", photos.len());
            for photo in photos.iter().take(10) {
                println!("  - {} ({} байт)", photo.file_name, photo.size_bytes);
            }
            if photos.len() > 10 {
                println!("  ... и ещё {} файлов", photos.len() - 10);
            }
        }
        Err(err) => {
            eprintln!("Ошибка сканирования: {err}");
        }
    }
}
