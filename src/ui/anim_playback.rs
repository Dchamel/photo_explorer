//! 4.7d — состояние проигрывания анимации: мост UI ⇄ поток-аниматор.
//!
//! UI (бегунки/кнопки панели) пишет сюда запросы (перемотка, пауза,
//! скорость), аниматор читает их на каждом шаге и пишет обратно текущий
//! кадр (бегунок перемотки двигается за анимацией). Всё на атомиках —
//! никаких блокировок на горячем пути, Send для потока-аниматора.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};

/// 4.7d — общее состояние проигрывания ОДНОЙ анимации.
/// `seek` — почтовый ящик одного запроса: UI кладёт номер кадра,
/// аниматор забирает (swap на −1). Скорость НЕ сбрасывается при смене
/// фото — это настройка пользователя и живёт всю сессию; пауза и кадр
/// сбрасываются новым аниматором.
pub(crate) struct AnimPlayback {
    /// Текущий кадр (0-based). Аниматор пишет, UI читает (бегунок) и
    /// сверяет в seek (подавление петли «аниматор → слайдер → seek»).
    frame: AtomicUsize,
    /// Всего кадров. Аниматор пишет на старте (слайдер знает максимум).
    total: AtomicUsize,
    /// Запрос перемотки: −1 = пусто, N = прыгнуть на кадр N.
    seek: AtomicI32,
    /// Пауза: true = кадр висит, аниматор крутится вхолостую (тики 30 мс).
    paused: AtomicBool,
    /// Скорость в ДЕСЯТЫХ множителя: 10 = ×1.0, 1 = ×0.1, 20 = ×2.0.
    /// Шаг 0.1 = ровно единица — целочисленно, без дробей с плавающей
    /// точкой в атомике.
    speed_tenths: AtomicU32,
}

impl AnimPlayback {
    pub(crate) fn new() -> Self {
        Self {
            frame: AtomicUsize::new(0),
            total: AtomicUsize::new(0),
            seek: AtomicI32::new(-1),
            paused: AtomicBool::new(false),
            speed_tenths: AtomicU32::new(10), // ×1.0 — родная скорость
        }
    }

    /// Текущий кадр (для сверки seek в UI-потоке).
    pub(crate) fn current_frame(&self) -> usize {
        self.frame.load(Ordering::SeqCst)
    }

    /// Аниматор записывает показанный кадр (бегунок двигается за ним).
    pub(crate) fn set_frame(&self, frame: usize) {
        self.frame.store(frame, Ordering::SeqCst);
    }

    /// Аниматор объявляет число кадров на старте.
    pub(crate) fn set_total(&self, total: usize) {
        self.total.store(total, Ordering::SeqCst);
    }

    /// UI просит перемотку на кадр (0-based). Лишние запросы того же
    /// кадра UI не шлёт (сверка в колбэке), но и повтор не вреден.
    pub(crate) fn request_seek(&self, frame: i32) {
        self.seek.store(frame, Ordering::SeqCst);
    }

    /// Аниматор забирает запрос перемотки (атомарно, один раз).
    pub(crate) fn take_seek(&self) -> Option<i32> {
        match self.seek.swap(-1, Ordering::SeqCst) {
            -1 => None,
            n => Some(n),
        }
    }

    pub(crate) fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub(crate) fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
    }

    /// Скорость в десятых множителя (1..=20 = ×0.1..×2.0).
    pub(crate) fn speed_tenths(&self) -> u32 {
        self.speed_tenths.load(Ordering::SeqCst).clamp(1, 20)
    }

    pub(crate) fn set_speed_tenths(&self, tenths: u32) {
        self.speed_tenths
            .store(tenths.clamp(1, 20), Ordering::SeqCst);
    }
}

/// 4.7d — эффективная пауза кадра с учётом скорости: родная задержка,
/// делённая на множитель (×2.0 = вдвое быстрее, ×0.1 = в 10 раз
/// медленнее). Чистая функция — легко тестировать.
pub(crate) fn effective_delay_ms(delay_ms: u64, speed_tenths: u32) -> u64 {
    delay_ms * 10 / u64::from(speed_tenths.clamp(1, 20))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_speed_keeps_delay() {
        assert_eq!(effective_delay_ms(40, 10), 40); // ×1.0 — как в файле
    }

    #[test]
    fn double_speed_halves_delay() {
        assert_eq!(effective_delay_ms(100, 20), 50); // ×2.0
    }

    #[test]
    fn tenth_speed_slows_tenfold() {
        assert_eq!(effective_delay_ms(100, 1), 1000); // ×0.1
    }

    #[test]
    fn zero_speed_is_clamped_to_minimum() {
        // Защита от мусора: скорость 0 не делит на ноль.
        assert_eq!(effective_delay_ms(100, 0), 1000); // clamp → ×0.1
    }

    #[test]
    fn seek_roundtrip_and_reset() {
        let p = AnimPlayback::new();
        assert_eq!(p.take_seek(), None, "изначально запросов нет");
        p.request_seek(7);
        assert_eq!(p.take_seek(), Some(7), "запрос достаётся один раз");
        assert_eq!(p.take_seek(), None, "второй раз — пусто (swap на −1)");
    }

    #[test]
    fn speed_persists_and_clamps() {
        let p = AnimPlayback::new();
        assert_eq!(p.speed_tenths(), 10); // ×1.0 по умолчанию
        p.set_speed_tenths(16); // ×1.6
        assert_eq!(p.speed_tenths(), 16);
        p.set_speed_tenths(99); // мусор — кламп в 20 (×2.0)
        assert_eq!(p.speed_tenths(), 20);
    }

    #[test]
    fn frame_tracks_playback() {
        let p = AnimPlayback::new();
        p.set_frame(42);
        assert_eq!(p.current_frame(), 42);
    }
}
