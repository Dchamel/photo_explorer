//! 4.7c — кэш декодированных кадров анимаций (LRU по весу).
//!
//! Повторное открытие того же GIF — мгновенно: декод сотен кадров не
//! повторяется. Лимит — СУММАРНЫЙ вес (не число записей): одна
//! анимация 640×480 на 85 кадров ≈ 100 МБ, а 2560×2560 на 200 —
//! гигабайты, «N анимаций» такой лимит не удержит.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// 4.7a — кадр анимации: сырой RGBA (Send) + задержка до СЛЕДУЮЩЕГО
/// кадра в миллисекундах. Декодер image сам композитит частичные
/// GIF-кадры на полный холст (blend_and_dispose) — каждый кадр готов
/// к показу как есть, без ручной склейки.
/// 4.7c — Clone: кэш отдаёт аниматору КОПИЮ кадров (кэш не отдаёт
/// ссылки наружу — записи живут и вытесняются независимо).
#[derive(Clone)]
pub(crate) struct AnimFrame {
    pub(crate) data: Vec<u8>,
    pub(crate) w: u32,
    pub(crate) h: u32,
    pub(crate) delay_ms: u64,
}

impl AnimFrame {
    /// 4.7c — вес кадра в байтах для лимита кэша: RGBA-данные плюс
    /// накладные поля. Кэш ограничен СУММАРНЫМ весом, а не числом
    /// записей: один GIF 2560×2560 на 200 кадров весит гигабайты,
    /// «N анимаций» такой лимит не удержит.
    pub(crate) fn weight_bytes(&self) -> u64 {
        self.data.len() as u64 + 16 // + w/h/delay (накладные)
    }
}

/// 4.7c — бюджет кэша кадров: ~400 МБ. Хватает на 3-4 типичных GIF
/// 640×480; гигантские анимации не вытесняют всё разом.
pub(crate) const ANIM_CACHE_MAX_BYTES: u64 = 400 * 1024 * 1024;

/// 4.7c — LRU-кэш декодированных кадров анимаций по ПУТИ файла.
/// Лимит — суммарный вес ≤ max_bytes. Переполнение — выкидываем САМЫЕ
/// СТАРЫЕ по использованию (LRU). Порядок — счётчик обращений (KISS:
/// без двусвязного списка; записей единицы, обход дёшев). Обёртывается
/// в Arc<Mutex<...>> владельцем: читает/пишет поток-декодер (Send).
pub(crate) struct AnimFramesCache {
    max_bytes: u64,
    used_bytes: u64,
    tick: u64,
    map: HashMap<PathBuf, (Vec<AnimFrame>, u64)>, // кадры + тик использования
}

impl AnimFramesCache {
    pub(crate) fn new(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            used_bytes: 0,
            tick: 0,
            map: HashMap::new(),
        }
    }

    /// Кадры по пути + обновление «свежести» (LRU). None = мимо кэша.
    pub(crate) fn get(&mut self, path: &Path) -> Option<Vec<AnimFrame>> {
        self.tick += 1;
        let entry = self.map.get_mut(path)?;
        entry.1 = self.tick;
        Some(entry.0.clone())
    }

    /// Положить кадры; при переполнении выкинуть самые старые по
    /// использованию, пока не влезем. Один GIF больше лимита —
    /// храним его ОДНОГО (лучше один быстрый, чем ничего).
    pub(crate) fn put(&mut self, path: PathBuf, frames: Vec<AnimFrame>) {
        self.tick += 1;
        let weight: u64 = frames.iter().map(|f| f.weight_bytes()).sum();
        // Замена существующей записи: вычитаем её старый вес.
        if let Some((old, _)) = self.map.remove(&path) {
            self.used_bytes -= old.iter().map(|f| f.weight_bytes()).sum::<u64>();
        }
        self.map.insert(path, (frames, self.tick));
        self.used_bytes += weight;
        // Вытеснение LRU: пока не влезаем — выкидываем запись с
        // МИНимальным тиком (самая давно не использованная).
        while self.used_bytes > self.max_bytes && self.map.len() > 1 {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, (_, tick))| *tick)
                .map(|(k, _)| k.clone());
            match oldest {
                Some(k) => {
                    if let Some((old, _)) = self.map.remove(&k) {
                        self.used_bytes -= old.iter().map(|f| f.weight_bytes()).sum::<u64>();
                    }
                }
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Кадр с RGBA-данными заданного размера (вес = bytes + 16).
    fn frame(bytes: usize) -> AnimFrame {
        AnimFrame {
            data: vec![0u8; bytes],
            w: 1,
            h: 1,
            delay_ms: 40,
        }
    }

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("C:\\тест\\{name}.gif"))
    }

    #[test]
    fn put_then_get_returns_frames() {
        let mut c = AnimFramesCache::new(1000);
        c.put(path("a"), vec![frame(100), frame(100)]);
        let got = c.get(&path("a")).expect("кадры должны быть в кэше");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].data.len(), 100);
    }

    #[test]
    fn get_unknown_path_is_miss() {
        let mut c = AnimFramesCache::new(1000);
        assert!(c.get(&path("нет")).is_none());
    }

    #[test]
    fn lru_evicts_oldest_when_budget_exceeded() {
        // Лимит 300: a=116, b=116, c=116 → после c переполнение,
        // вытесняется САМАЯ СТАРАЯ (a).
        let mut c = AnimFramesCache::new(300);
        c.put(path("a"), vec![frame(100)]);
        c.put(path("b"), vec![frame(100)]);
        c.put(path("c"), vec![frame(100)]);
        assert!(c.get(&path("a")).is_none(), "a должна быть вытеснена");
        assert!(c.get(&path("b")).is_some());
        assert!(c.get(&path("c")).is_some());
    }

    #[test]
    fn get_refreshes_recency() {
        // a положена первой, но ПОСЛЕ b её трогали (get) — при
        // переполнении вытесняется b, не a.
        let mut c = AnimFramesCache::new(300);
        c.put(path("a"), vec![frame(100)]);
        c.put(path("b"), vec![frame(100)]);
        let _ = c.get(&path("a")); // a стала свежее
        c.put(path("c"), vec![frame(100)]); // переполнение → b старее
        assert!(
            c.get(&path("b")).is_none(),
            "b должна быть вытеснена (a свежее)"
        );
        assert!(c.get(&path("a")).is_some());
    }

    #[test]
    fn oversize_single_entry_is_kept_alone() {
        // Одна анимация больше лимита — храним её ОДНУ (лучше один
        // быстрый GIF, чем ничего).
        let mut c = AnimFramesCache::new(100);
        c.put(path("гигант"), vec![frame(150)]);
        assert!(c.get(&path("гигант")).is_some());
    }

    #[test]
    fn replace_updates_weight_without_double_count() {
        // Повторный put того же пути — ЗАМЕНА: старый вес вычитается,
        // двойного учёта нет.
        let mut c = AnimFramesCache::new(10_000);
        c.put(path("a"), vec![frame(100)]);
        c.put(path("a"), vec![frame(200)]);
        assert_eq!(c.used_bytes, 200 + 16);
    }
}
