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
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

/// Глобальные настройки в памяти: обёртки-атомики для доступа из
/// воркер-потока без блокировок. UI-поток меняет значения через
/// `store()` (сохранение на диск + запись в атомики).
pub struct GlobalSettings {
    /// «Ультра-скорость» включена?
    pub ultra_speed: AtomicBool,
    /// Сколько ядер использовать при включённой «Ультра-скорости».
    /// 0 = «половина ядер» (значение по умолчанию вычисляется при загрузке).
    pub threads: AtomicUsize,
    /// 4.6c-fix3 — «Детальная статистика в логах» (вкладка «Отладка»):
    /// под флагом воркер пишет число реально задействованных полос rayon
    /// и прочие замеры. ВЫКЛ по умолчанию у всех — обычному пользователю
    /// детальный лог не нужен (чисто отладочный инструмент).
    /// 4.8 — заменяется выбором уровня логов (см. log_level), оставлен
    /// для чтения старых файлов настроек; UI больше его не показывает.
    pub debug_stats: AtomicBool,
    /// 4.8 — уровень подробности лога: 0 «Важное» (Error/Warn),
    /// 1 «Обычный» (+Info — ход работы), 2 «Подробный» (+Debug/Trace —
    /// покадровый [ANIM], [CONC]-детали). Лог пишется ВСЕГДА; уровень
    /// регулирует только подробность. По умолчанию 1 — обычный.
    pub log_level: AtomicU8,
    /// 4.7a — «Зациклить анимацию» (GIF/WEBP/APNG): ВКЛ — проигрывать
    /// по кругу (как в браузерах, по умолчанию); ВЫКЛ — один проход,
    /// стоп на последнем кадре. Глобальная настройка (про поведение
    /// просмотра, не про конкретный проект); поток-аниматор читает её
    /// на каждом проходе — смена применяется сразу.
    pub loop_animation: AtomicBool,
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
            debug_stats: AtomicBool::new(false),
            // 4.7a — цикл по умолчанию: естественное поведение просмотрщика.
            loop_animation: AtomicBool::new(true),
            // 4.8 — «Обычный» по умолчанию: ключевые события и ход
            // работы видны, покадровая трассировка — нет.
            log_level: AtomicU8::new(1),
        }
    }

    /// Текущие значения кортежем (удобно для UI и логов).
    pub fn snapshot(&self) -> (bool, usize) {
        (
            self.ultra_speed.load(Ordering::Relaxed),
            self.threads.load(Ordering::Relaxed),
        )
    }

    /// 4.6c-fix3 — детальная статистика включена? (вкладка «Отладка»).
    pub fn debug_stats_on(&self) -> bool {
        self.debug_stats.load(Ordering::Relaxed)
    }

    /// 4.7a — зациклить анимацию? (глобальная настройка просмотра).
    pub fn loop_animation_on(&self) -> bool {
        self.loop_animation.load(Ordering::Relaxed)
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
    /// 4.6c-fix3 — отсутствует в старых файлах → serde default (false).
    #[serde(default)]
    debug_stats: bool,
    /// 4.7a — отсутствует в старых файлах → serde default (true, цикл).
    #[serde(default = "default_loop_animation")]
    loop_animation: bool,
    /// 4.8 — отсутствует в старых файлах → serde default (1, обычный).
    #[serde(default = "default_log_level")]
    log_level: u8,
}

/// 4.8 — уровень логов по умолчанию для serde («Обычный»).
fn default_log_level() -> u8 {
    1
}

/// 4.7a — значение loop_animation по умолчанию для serde (цикл ВКЛ).
fn default_loop_animation() -> bool {
    true
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
                settings.debug_stats.store(f.debug_stats, Ordering::Relaxed);
                settings
                    .loop_animation
                    .store(f.loop_animation, Ordering::Relaxed);
                // 4.8 — уровень логов (кламп 0..2 от мусора в файле).
                settings
                    .log_level
                    .store(f.log_level.min(2), Ordering::Relaxed);
                log::info!(
                    "settings: загружены (ultra_speed={}, threads={}, debug_stats={}, loop_animation={}, log_level={})",
                    f.ultra_speed,
                    f.threads.max(1),
                    f.debug_stats,
                    f.loop_animation,
                    f.log_level.min(2)
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
        debug_stats: settings.debug_stats_on(),
        loop_animation: settings.loop_animation_on(),
        // 4.8 — уровень подробности лога.
        log_level: settings.log_level.load(Ordering::Relaxed),
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
        // Детальная статистика ВЫКЛ по умолчанию — тихий лог у всех.
        assert!(!s.debug_stats_on());
        // Анимация ЗАЦИКЛЕНА по умолчанию — как в браузерах.
        assert!(s.loop_animation_on());
        let (_, threads) = s.snapshot();
        assert!(threads >= 1);
    }
}
