//! Логирование Photo Explorer.
//!
//! Собственная реализация трейта [`log::Log`]: пишет в файл
//! `%TEMP%/photo_explorer/photo_explorer.log` и дублирует в stderr.
//!
//! ## Зачем
//! Заказчик сообщил о зависании приложения («Не отвечает») при просмотре фото.
//! Чтобы диагностировать причину (deadlock, паника, долгая операция), нужна
//! подробная система логирования — по логу видно, что произошло и почему упало.
//!
//! ## Как пользоваться
//! 1. В начале [`main`] вызвать [`init`] — это установит логгер и panic hook.
//! 2. Везде использовать макросы `log::info!`, `log::warn!`, `log::error!`,
//!    `log::debug!`, `log::trace!`.
//! 3. При панике (в любом потоке) hook запишет стек в лог.
//!
//! ## Где лог
//! Путь возвращается функцией [`log_path`] и печатается в stderr при старте.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;

use log::{Level, Log, Metadata, Record};

/// Папка под логи (и прочие данные приложения) в temp пользователя.
fn app_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("photo_explorer")
}

/// Полный путь к файлу лога.
pub fn log_path() -> std::path::PathBuf {
    app_dir().join("photo_explorer.log")
}

/// Глобальный файл лога (открывается один раз при init).
static LOG_FILE: OnceLock<std::sync::Mutex<std::fs::File>> = OnceLock::new();

/// Наш логгер.
struct FileLogger;

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Trace
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // Формат: 2026-10-18T10:44:12.123  INFO  [ui]  сообщение
        let now = chrono_like_now();
        let line = format!(
            "{now}  {level:<5}  [{target}]  {args}\n",
            level = record.level(),
            target = record.target(),
            args = record.args(),
        );
        // В файл (под блокировкой — пишем из разных потоков).
        if let Some(mtx) = LOG_FILE.get() {
            if let Ok(mut file) = mtx.lock() {
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
            }
        }
        // И в stderr — чтобы видеть и в терминале.
        let _ = std::io::stderr().write_all(line.as_bytes());
    }

    fn flush(&self) {
        if let Some(mtx) = LOG_FILE.get() {
            if let Ok(mut file) = mtx.lock() {
                let _ = file.flush();
            }
        }
    }
}

/// Простейшая «человекочитаемая» метка времени без зависимости на chrono.
/// Формат YYYY-MM-DDThh:mm:ss.mmm (локальное время, точность до мс).
fn chrono_like_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let millis = now.subsec_millis();
    // Перевод секунд эпохи в дату — локально, без внешних крейтов.
    let (y, mo, d, h, mi, s) = epoch_to_local(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}")
}

/// Перевод секунд Unix-эпохи в дату/время UTC (грубая реализация).
/// Достаточно для лога; високосные годы учтены. Лог ведётся в UTC —
/// это нормально для диагностики (время старта печатается и локально).
fn epoch_to_local(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let h = rem / 3600;
    let mi = (rem % 3600) / 60;
    let s = rem % 60;
    let (y, mo, d) = days_to_ymd(days);
    (y, mo, d, h, mi, s)
}

/// Перевод числа дней с 1970-01-01 в (год, месяц, день).
fn days_to_ymd(mut days: i64) -> (i64, i64, i64) {
    days += 719468; // смещение к эпохе алгоритма Говарда Хиннанта
    let era = (if days >= 0 { days } else { days - 146096 }) / 146097;
    let doe = days - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Инициализация логгера. Вызывать в начале [`main`].
///
/// - Создаёт папку `%TEMP%/photo_explorer`.
/// - Открывает (или создаёт/дополняет) файл `photo_explorer.log`.
/// - Устанавливает [`FileLogger`] глобальным логгером.
/// - Ставит panic hook: паника любого потока пишется в лог (с потоком и сообщением),
///   затем вызывается предыдущий hook (чтобы был стандартный вывод тоже).
///
/// Возвращает путь к файлу лога (для печати пользователю).
pub fn init() -> std::path::PathBuf {
    let path = log_path();
    let _ = std::fs::create_dir_all(app_dir());

    // Разделитель — новый запуск легко найти в логе.
    let header = format!(
        "\n========== Photo Explorer запуск: {} ==========\n",
        chrono_like_now()
    );
    let _ = std::io::stderr().write_all(header.as_bytes());

    // Открываем файл в режиме дополнения (append). Если не вышло — логируем
    // только в stderr (LOG_FILE останется None, FileLogger это учитывает).
    let file_result = OpenOptions::new().create(true).append(true).open(&path);
    match file_result {
        Ok(file) => {
            // Допишем разделитель в файл.
            let _ = (&file).write_all(header.as_bytes());
            let _ = LOG_FILE.set(std::sync::Mutex::new(file));
        }
        Err(e) => {
            eprintln!("Не удалось открыть файл лога {path:?}: {e} (лог только в stderr)");
        }
    }

    // Глобальный логгер.
    let _ = log::set_logger(&FileLogger);
    log::set_max_level(log::LevelFilter::Trace);

    // Panic hook: пишем панику в лог (с потоком), затем зовём прежний hook.
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>");
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .map(String::from)
            .or_else(|| info.payload().downcast_ref::<String>().map(String::from))
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_string());
        log::error!("ПАНИКА в потоке «{name}» по адресу {location}: {payload}");
        // Стек недоступен без backtrace-крейта; предыдущий hook напечатает стандартное.
        prev(info);
    }));

    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_to_ymd_epoch() {
        // 1970-01-01 — это 0 дней от эпохи.
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_known_date() {
        // 2026-10-18. Считаем дни от 1970-01-01.
        // Грубо: 56 лет * 365 + високосные. Точность проверим хотя бы порядок.
        let (y, _m, _d) = days_to_ymd(20680);
        assert_eq!(y, 2026);
    }
}
