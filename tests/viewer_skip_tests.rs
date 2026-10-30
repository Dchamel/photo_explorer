//! Тесты авто-скипа битых файлов в просмотрщике (4.3e) и режимов листания (4.4a).
//!
//! `next_valid_index` — чистая функция выбора следующего индекса при скипе:
//!dir = +1 (вперёд) / −1 (назад), кламп по границам, −1 = «годных больше нет».
//! `nav_next` (4.4a) — следующий индекс по режиму проекта: edge (стоп на краю),
//! cycle (заворачивание), random (случайный кадр; обе кнопки случайные).
//! Дополнительно: реальный файл .png с мусорным содержимым не декодируется
//! (image::open → Err) — это и есть условие скипа в воркере.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use photo_explorer::ui::{
    clamp_offset, nav_next, nav_random_next, next_valid_index, preload_neighbors, shuffled_order,
    NavMode,
};

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
    // Квадратная картинка 1000x1000, viewport 1000x1000, zoom 2: центр не трогаем.
    assert_eq!(clamp_offset(0.0, 2.0, 1000.0, 1000.0, 1000.0, 1000.0), 0.0);
}

#[test]
fn clamp_offset_zoom_one_forces_zero() {
    // zoom=1: вписанное фото нельзя двигать — максимум 0, любой offset → 0.
    assert_eq!(clamp_offset(50.0, 1.0, 1000.0, 1000.0, 1000.0, 1000.0), 0.0);
    assert_eq!(
        clamp_offset(-50.0, 1.0, 1000.0, 1000.0, 1000.0, 1000.0),
        0.0
    );
}

#[test]
fn clamp_offset_edge_limit() {
    // Квадрат 1000x1000 в viewport 1000x1000, zoom 2 → размер 2000, max = 500.
    // Смещение 700 (за край) клампится к 500 — фото упирается краем в край.
    assert_eq!(
        clamp_offset(700.0, 2.0, 1000.0, 1000.0, 1000.0, 1000.0),
        500.0
    );
    assert_eq!(
        clamp_offset(-700.0, 2.0, 1000.0, 1000.0, 1000.0, 1000.0),
        -500.0
    );
}

#[test]
fn clamp_offset_inside_bounds_untouched() {
    // Смещение внутри допустимого диапазона не меняется.
    assert_eq!(
        clamp_offset(300.0, 2.0, 1000.0, 1000.0, 1000.0, 1000.0),
        300.0
    );
    assert_eq!(
        clamp_offset(-300.0, 2.0, 1000.0, 1000.0, 1000.0, 1000.0),
        -300.0
    );
}

#[test]
fn clamp_offset_narrow_image_x_axis_locked() {
    // 4.3g — УЗКАЯ картинка 480x600, viewport 1536x864 (как в логе).
    // contain-scale = min(1536/480, 864/600) = min(3.2, 1.44) = 1.44.
    // Ширина на экране = 480*1.44*zoom. При zoom=10: 6912 > 1536 → двигать МОЖНО,
    // max = (6912−1536)/2 = 2688. По Y: 600*1.44*10 = 8640 > 864 → max = 3888.
    // Проверяем X: смещение 5000 клампится к 2688.
    // float-погрешность f32 — сравниваем приближённо (abs diff < 1).
    let x = clamp_offset(5000.0, 10.0, 1536.0, 480.0, 600.0, 864.0);
    assert!((x - 2688.0).abs() < 1.0, "ожидали ~2688, получили {x}");
    // По Y (та же картинка, оси меняются местами): 5000 → ~3888.
    let y = clamp_offset(5000.0, 10.0, 864.0, 600.0, 480.0, 1536.0);
    assert!((y - 3888.0).abs() < 1.0, "ожидали ~3888, получили {y}");
}

#[test]
fn clamp_offset_narrow_image_small_zoom_locked() {
    // Узкая картинка при МАЛОМ zoom: 480x600, viewport 1536x864, zoom 1.5.
    // Ширина на экране = 480*1.44*1.5 = 1036.8 < 1536 → по X двигать НЕЛЬЗЯ (max=0).
    // Раньше (кламп по элементу) можно было утащить за экран — теперь нет.
    assert_eq!(clamp_offset(500.0, 1.5, 1536.0, 480.0, 600.0, 864.0), 0.0);
}

#[test]
fn clamp_offset_negative_viewport_safe() {
    // Защита от вырожденных размеров: viewport ≤ 0 → contain_scale = 1, size = img,
    // max = (img − vp)/2 > 0 — но фото меньше экрана, двигать нельзя → max 0.
    // img=1000 ≤ vp=0 → size=1000*1*2=2000, max=(2000−0)/2=1000? Нет: contain_scale
    // при vp=0 → 0/480=0 → size=0 → max=0. Проверяем фактическое поведение.
    assert_eq!(clamp_offset(123.0, 2.0, 0.0, 1000.0, 1000.0, 0.0), 0.0);
}

// --- 4.4a — nav_next: режимы листания (edge / cycle / random) ---
// 4.4c-3 — все функции принимают broken: множество битых индексов,
// которые навигация ПРОПУСКАЕТ (не грузит заведомо битое).

use std::collections::HashSet;

/// Пустое множество битых (ничего не исключено).
fn no_broken() -> HashSet<i32> {
    HashSet::new()
}

/// Множество из перечисленных индексов.
fn broken_of(items: &[i32]) -> HashSet<i32> {
    items.iter().copied().collect()
}

#[test]
fn nav_next_edge_stops_at_last_photo() {
    // 5 фото, стоим на последнем (4), next → None (стоп, не прыгаем на 0).
    assert_eq!(nav_next(4, 5, 1, NavMode::Edge, &no_broken()), None);
}

#[test]
fn nav_next_edge_stops_at_first_photo() {
    // 5 фото, стоим на первом (0), prev → None (стоп — фикс «листает в минус»).
    assert_eq!(nav_next(0, 5, -1, NavMode::Edge, &no_broken()), None);
}

#[test]
fn nav_next_edge_moves_in_bounds() {
    assert_eq!(nav_next(2, 5, 1, NavMode::Edge, &no_broken()), Some(3));
    assert_eq!(nav_next(2, 5, -1, NavMode::Edge, &no_broken()), Some(1));
}

#[test]
fn nav_next_edge_skips_broken_forward() {
    // 4.4c-3: 3 и 4 битые → next с 2 перепрыгивает их на 5.
    let broken = broken_of(&[3, 4]);
    assert_eq!(nav_next(2, 6, 1, NavMode::Edge, &broken), Some(5));
}

#[test]
fn nav_next_edge_skips_broken_backward() {
    // 4.4c-3: 1 и 0 битые → prev с 2 перепрыгивает на… край → None (стоп).
    let broken = broken_of(&[0, 1]);
    assert_eq!(nav_next(2, 6, -1, NavMode::Edge, &broken), None);
}

#[test]
fn nav_next_cycle_skips_broken_with_wrap() {
    // 4.4c-3: 0 битый → cycle next с 4 заворачивает на 0, видит битый, идёт на 1.
    let broken = broken_of(&[0]);
    assert_eq!(nav_next(4, 5, 1, NavMode::Cycle, &broken), Some(1));
}

#[test]
fn nav_next_cycle_all_broken_returns_none() {
    // Весь список битый → None (нечего показывать).
    let broken = broken_of(&[0, 1, 2, 3, 4]);
    assert_eq!(nav_next(2, 5, 1, NavMode::Cycle, &broken), None);
}

#[test]
fn nav_next_cycle_wraps_forward() {
    // Последний (4) + next → первый (0).
    assert_eq!(nav_next(4, 5, 1, NavMode::Cycle, &no_broken()), Some(0));
}

#[test]
fn nav_next_cycle_wraps_backward() {
    // Первый (0) + prev → последний (4).
    assert_eq!(nav_next(0, 5, -1, NavMode::Cycle, &no_broken()), Some(4));
}

#[test]
fn nav_next_cycle_in_bounds() {
    assert_eq!(nav_next(2, 5, 1, NavMode::Cycle, &no_broken()), Some(3));
    assert_eq!(nav_next(2, 5, -1, NavMode::Cycle, &no_broken()), Some(1));
}

#[test]
fn nav_next_random_single_file_returns_zero() {
    // 4.4c — random идёт по перемешанному списку (nav_random_next);
    // nav_next для Random — edge-семантика (страховочная ветка). 1 фото:
    // соседних позиций нет → None (стоп).
    assert_eq!(nav_next(0, 1, 1, NavMode::Random, &no_broken()), None);
    assert_eq!(nav_next(0, 1, -1, NavMode::Random, &no_broken()), None);
}

#[test]
fn nav_next_empty_list_returns_none_any_mode() {
    // Пустой список — листать нечего ни в каком режиме.
    assert_eq!(nav_next(0, 0, 1, NavMode::Edge, &no_broken()), None);
    assert_eq!(nav_next(0, 0, 1, NavMode::Cycle, &no_broken()), None);
    assert_eq!(nav_next(0, 0, 1, NavMode::Random, &no_broken()), None);
}

#[test]
fn nav_next_edge_single_file_never_moves() {
    // 1 фото в edge: next и prev → None (соседей нет, стоп).
    assert_eq!(nav_next(0, 1, 1, NavMode::Edge, &no_broken()), None);
    assert_eq!(nav_next(0, 1, -1, NavMode::Edge, &no_broken()), None);
}

// --- 4.4c — random через перемешанный список (shuffle) ---

#[test]
fn shuffled_order_is_bijection() {
    // Перестановка: каждый индекс 0..count встречается РОВНО один раз.
    for count in [0, 1, 2, 5, 100] {
        let order = shuffled_order(count);
        assert_eq!(order.len(), count as usize);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(sorted, (0..count).collect::<Vec<i32>>());
    }
}

#[test]
fn nav_random_next_walks_order_back_and_forth() {
    // Фиксированный порядок: prev/next ходят по нему честно — «назад»
    // возвращает предыдущий элемент, а не новый случайный.
    let order = vec![7, 2, 9, 0, 4];
    // next с позиции 1 (фото 2) → позиция 2 (фото 9).
    assert_eq!(nav_random_next(&order, 1, 1, &no_broken()), Some((2, 9)));
    // prev с позиции 2 → позиция 1 (фото 2) — честный возврат.
    assert_eq!(nav_random_next(&order, 2, -1, &no_broken()), Some((1, 2)));
    // prev с позиции 0 → None (начало списка — стоп).
    assert_eq!(nav_random_next(&order, 0, -1, &no_broken()), None);
    // next с последней позиции 4 → None (конец списка — стоп).
    assert_eq!(nav_random_next(&order, 4, 1, &no_broken()), None);
}

#[test]
fn nav_random_next_skips_broken_positions() {
    // 4.4c-3: фото 9 (позиция 2) битое → next с позиции 1 перепрыгивает
    // на позицию 3 (фото 0), не показывая битое.
    let order = vec![7, 2, 9, 0, 4];
    let broken = broken_of(&[9]);
    assert_eq!(nav_random_next(&order, 1, 1, &broken), Some((3, 0)));
    // prev с позиции 3 → тоже перепрыгивает битое 9 → позиция 1 (фото 2).
    assert_eq!(nav_random_next(&order, 3, -1, &broken), Some((1, 2)));
}

#[test]
fn nav_random_next_empty_order_returns_none() {
    assert_eq!(nav_random_next(&[], 0, 1, &no_broken()), None);
    assert_eq!(nav_random_next(&[], 0, -1, &no_broken()), None);
}

// --- 4.4c-4 — предзагрузка соседей ПО ТЕКУЩЕМУ ПОРЯДКУ ---

#[test]
fn preload_neighbors_linear_order() {
    // Обычный порядок: 3 соседа в каждую сторону от index=5.
    let got = preload_neighbors(5, 10, 3, None, 0, &no_broken());
    assert_eq!(got, vec![6, 4, 7, 3, 8, 2]);
}

#[test]
fn preload_neighbors_linear_clamps_edges() {
    // index=1: вперёд 2,3,4; назад только 0 (дальше край).
    let got = preload_neighbors(1, 10, 3, None, 0, &no_broken());
    assert_eq!(got, vec![2, 0, 3, 4]);
}

#[test]
fn preload_neighbors_random_follows_shuffle_order() {
    // Random: соседи — фото с СОСЕДНИХ ПОЗИЦИЙ перемешанного списка,
    // не линейные index±1. Порядок [4,2,3,0,1] (перестановка 0..5),
    // позиция 2 (фото 3): соседи = фото с позиций 3,1,4,0 → 0,2,1,4.
    let order = vec![4, 2, 3, 0, 1];
    let got = preload_neighbors(3, 5, 3, Some(&order), 2, &no_broken());
    assert_eq!(got, vec![0, 2, 1, 4]);
}

#[test]
fn preload_neighbors_skips_broken() {
    // Битые (3, 7) не предзагружаем — не тратим время воркера.
    // index=5, radius=2: step1 → 6,4; step2 → 7(битый, скип),3(битый, скип).
    let broken = broken_of(&[3, 7]);
    let got = preload_neighbors(5, 10, 2, None, 0, &broken);
    assert_eq!(got, vec![6, 4]);
}

#[test]
fn nav_mode_parses_strings() {
    // FromStr-трейт: явные значения; пустая строка (старые JSON) и мусор
    // → Edge (по умолчанию).
    let parse = |s: &str| -> NavMode { s.parse().unwrap_or(NavMode::Edge) };
    assert_eq!(parse("edge"), NavMode::Edge);
    assert_eq!(parse("cycle"), NavMode::Cycle);
    assert_eq!(parse("random"), NavMode::Random);
    assert_eq!(parse(""), NavMode::Edge);
    assert_eq!(parse("мусор"), NavMode::Edge);
    // Обратное преобразование — стабильные строки для сессии.
    assert_eq!(NavMode::Edge.as_str(), "edge");
    assert_eq!(NavMode::Cycle.as_str(), "cycle");
    assert_eq!(NavMode::Random.as_str(), "random");
}
