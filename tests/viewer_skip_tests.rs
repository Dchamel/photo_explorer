//! Тесты авто-скипа битых файлов в просмотрщике (4.3e).
//!
//! `next_valid_index` — чистая функция выбора следующего индекса при скипе:
//!dir = +1 (вперёд) / −1 (назад), кламп по границам, −1 = «годных больше нет».
//! Дополнительно: реальный файл .png с мусорным содержимым не декодируется
//! (image::open → Err) — это и есть условие скипа в воркере.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use photo_explorer::ui::{clamp_offset, next_valid_index};

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let n = DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("photo_explorer_{prefix}_{stamp}_{n}"))
}

#[test]
fn next_valid_index_forward_in_bounds() {
    // 5 файлов, скип с index=2 вперёд → 3.
    assert_eq!(next_valid_index(2, 1, 5), 3);
}

#[test]
fn next_valid_index_backward_in_bounds() {
    // 5 файлов, скип с index=2 назад → 1.
    assert_eq!(next_valid_index(2, -1, 5), 1);
}

#[test]
fn next_valid_index_forward_at_edge_returns_minus_one() {
    // index=4 (последний), dir=+1 → за край → −1 (годных больше нет).
    assert_eq!(next_valid_index(4, 1, 5), -1);
}

#[test]
fn next_valid_index_backward_at_start_returns_minus_one() {
    // index=0, dir=−1 → за край → −1.
    assert_eq!(next_valid_index(0, -1, 5), -1);
}

#[test]
fn next_valid_index_empty_list_returns_minus_one() {
    assert_eq!(next_valid_index(0, 1, 0), -1);
}

#[test]
fn next_valid_index_single_file_no_neighbors() {
    // 1 файл: любой скип → за край.
    assert_eq!(next_valid_index(0, 1, 1), -1);
    assert_eq!(next_valid_index(0, -1, 1), -1);
}

#[test]
fn broken_png_does_not_decode() {
    // Файл с расширением .png, но мусорным содержимым — НЕ валидный PNG.
    // image::open должен вернуть Err (как в логах: Invalid PNG signature).
    let dir = unique_temp_dir("broken");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("не удалось создать тестовую папку");
    let broken = dir.join("broken.png");
    // 8 байт мусора — не PNG-сигнатура (137 80 78 71 13 10 26 10).
    fs::write(&broken, b"NOTAPNG!").expect("не удалось записать битый файл");

    let result = image::open(&broken);
    assert!(
        result.is_err(),
        "битый .png не должен декодироваться, а получили Ok"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn clamp_offset_center_stays() {
    // Смещение 0 (центр) — кламп не трогает.
    assert_eq!(clamp_offset(0.0, 2.0, 1000.0), 0.0);
}

#[test]
fn clamp_offset_zoom_one_forces_zero() {
    // zoom=1: вписанное фото нельзя двигать — максимум 0, любой offset → 0.
    assert_eq!(clamp_offset(50.0, 1.0, 1000.0), 0.0);
    assert_eq!(clamp_offset(-50.0, 1.0, 1000.0), 0.0);
}

#[test]
fn clamp_offset_edge_limit() {
    // viewport 1000, zoom 2 → max = (1000*2 − 1000)/2 = 500.
    // Смещение 700 (за край) клампится к 500 — фото упирается краем в край.
    assert_eq!(clamp_offset(700.0, 2.0, 1000.0), 500.0);
    assert_eq!(clamp_offset(-700.0, 2.0, 1000.0), -500.0);
}

#[test]
fn clamp_offset_inside_bounds_untouched() {
    // Смещение внутри допустимого диапазона не меняется.
    assert_eq!(clamp_offset(300.0, 2.0, 1000.0), 300.0);
    assert_eq!(clamp_offset(-300.0, 2.0, 1000.0), -300.0);
}

#[test]
fn clamp_offset_negative_viewport_safe() {
    // Защита от вырожденных размеров: viewport ≤ 0 → максимум 0.
    assert_eq!(clamp_offset(123.0, 2.0, 0.0), 0.0);
    assert_eq!(clamp_offset(123.0, 2.0, -10.0), 0.0);
}
