//! Тесты расчёта задержки кадров анимации (4.7a-fix4).
//!
//! Контракт единиц: `image::Delay::numer_denom_ms()` отдаёт дробь, значение
//! которой УЖЕ миллисекунды (кадр GIF «три сотых секунды» → (30, 1) = 30 мс;
//! в крейте `impl From<Delay> for Duration` кладёт ratio прямо в
//! `from_millis`). Ошибка 4.7a (умножение на 1000) трактовала дробь как
//! СЕКУНДЫ: кадр 30 мс спал 30 СЕКУНД — «GIF замер на первом кадре».

use photo_explorer::ui::anim_delay_ms;

#[test]
fn typical_gif_frame_is_30ms_not_30s() {
    // «Три сотых секунды» GIF → дробь (30, 1): 30/1 мс.
    // Регрессия: старая формула (×1000) давала 30 000 мс.
    assert_eq!(anim_delay_ms((30, 1)), 30);
}

#[test]
fn fractional_delay_floors_to_minimum() {
    // 0.5 мс — меньше минимальной кадровой паузы, поднимаем до 20 мс.
    assert_eq!(anim_delay_ms((1, 2)), 20);
}

#[test]
fn zero_delay_floors_to_minimum() {
    assert_eq!(anim_delay_ms((0, 1)), 20);
}

#[test]
fn zero_denominator_falls_back_to_100ms() {
    // Кадр без указанной задержки — показываем 100 мс, не проскок.
    assert_eq!(anim_delay_ms((30, 0)), 100);
}

#[test]
fn long_pause_frame_is_not_cut() {
    // Легитимный кадр-пауза (10 с) не обрезается — это часть анимации.
    assert_eq!(anim_delay_ms((10_000, 1)), 10_000);
}

#[test]
fn common_gif_delays_stay_in_milliseconds() {
    // Типичные задержки GIF: 20/50/70/100/200 мс.
    assert_eq!(anim_delay_ms((20, 1)), 20);
    assert_eq!(anim_delay_ms((50, 1)), 50);
    assert_eq!(anim_delay_ms((70, 1)), 70);
    assert_eq!(anim_delay_ms((100, 1)), 100);
    assert_eq!(anim_delay_ms((200, 1)), 200);
}
