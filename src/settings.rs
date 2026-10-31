//! Глобальные настройки приложения (4.6c, Фаза 2).
//!
//! Отличие от настроек проекта (nav_mode в сессии): эти настройки —
//! про ЖЕЛЕЗО (ядра/батарея), а не про конкретную коллекцию фото,
//! поэтому они ГЛОБАЛЬНЫЕ и живут отдельно от сессий:
//! `%LOCALAPPDATA%/photo_explorer/settings.json`.
//!
//! «Ультра-скорость» (по умолчанию ВЫКЛ) — тяжёлые фото (4K+)
//! декодируются/ресайзятся на N ядрах через fast_image_resize + rayon.
//! Выключена — один поток, как раньше: бережёт батарею ноутбука
//! (i5-1235U — энергоэффективный 15-ваттный чип, все ядра под
//! нагрузкой греют его и жгут заряд).
//!
//! Воркер-поток читает настройки через атомики (Arc<AtomicBool> /
//! Arc<AtomicUsize>) — без мьютексов и без гонок: UI пишет, воркер
//! читает на каждом декоде, актуальность мгновенная.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Глобальные настройки в памяти: обёртки-атомики для доступа из
/// воркер-потока без блокировок. UI-поток меняет значения через
/// `store()` (сохранение на диск + запись в атомики).
pub struct GlobalSettings {
    /// «Ультра-скорость» включена?
    pub ultra_speed: AtomicBool,
    /// Сколько ядер использовать при включённой «Ультра-скорости».
    /// 0 = «половина ядер» (значение по умолчанию вычисляется при загрузке).
    pub threads: AtomicUsize,
}

impl GlobalSettings {
    /// Значения по умолчанию: ВЫКЛ + половина ядер системы.
    /// Половина — компромисс заказчика: заметно быстрее одного ядра,
    /// но не захватывает весь процессор целиком.
    pub fn new() -> Self {
        let threads = default_threads();
        Self {
            ultra_speed: AtomicBool::new(false),
            threads: AtomicUsize::new(threads),
        }
    }

    /// Текущие значения одним кортежем (удобно для UI и логов).
    pub fn snapshot(&self) -> (bool, usize) {
        (
            self.ultra_speed.load(Ordering::Relaxed),
            self.threads.load(Ordering::Relaxed),
        )
    }
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self::new()
    }
}

/// Половина логических ядер системы, минимум 1 (i5-1235U: 10/2 = 5).
fn default_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get() / 2)
        .unwrap_or(1)
        .max(1)
}

/// Путь к файлу настроек: %LOCALAPPDATA%/photo_explorer/settings.json
/// (рядом с сессиями — приватные данные пользователя, в репозиторий
/// и программу не попадают).
fn settings_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("photo_explorer")
        .join("settings.json")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SettingsFile {
    ultra_speed: bool,
    threads: usize,
}

/// Загрузить настройки с диска (или значения по умолчанию, если файла
/// нет/битый). Возвращает GlobalSettings с применёнными значениями.
pub fn load() -> GlobalSettings {
    let settings = GlobalSettings::new();
    let path = settings_path();
    let file = std::fs::read_to_string(&path);
    match file {
        Ok(text) => match serde_json::from_str::<SettingsFile>(&text) {
            Ok(f) => {
                settings.ultra_speed.store(f.ultra_speed, Ordering::Relaxed);
                settings.threads.store(f.threads.max(1), Ordering::Relaxed);
                log::info!(
                    "settings: загружены (ultra_speed={}, threads={})",
                    f.ultra_speed,
                    f.threads.max(1)
                );
            }
            Err(e) => {
                log::warn!("settings: файл битый ({e}), беру значения по умолчанию");
            }
        },
        Err(_) => {
            log::info!("settings: файла нет, значения по умолчанию (ultra_speed=false)");
        }
    }
    settings
}

/// Сохранить настройки на диск. Ошибки не фатальны (настройки — не
/// критичные данные), логируем и продолжаем.
pub fn store(settings: &GlobalSettings) {
    let (ultra_speed, threads) = settings.snapshot();
    let file = SettingsFile {
        ultra_speed,
        threads,
    };
    let path = settings_path();
    // Создаём каталог, если его ещё нет (первый запуск).
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::error!("settings: не создать каталог {}: {e}", path.display());
            return;
        }
    }
    match serde_json::to_string_pretty(&file) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                log::error!("settings: не удалось сохранить {}: {e}", path.display());
            }
        }
        Err(e) => log::error!("settings: не удалось сериализовать: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_threads_at_least_one() {
        let t = default_threads();
        assert!(t >= 1);
    }

    #[test]
    fn defaults_are_conservative() {
        let s = GlobalSettings::new();
        // Ультра-скорость ВЫКЛ по умолчанию — бережём батарею.
        assert!(!s.ultra_speed.load(Ordering::Relaxed));
        let (_, threads) = s.snapshot();
        assert!(threads >= 1);
    }
}
