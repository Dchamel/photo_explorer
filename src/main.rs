//! Photo Explorer — точка входа приложения.
//!
//! Этап 3: показываем экран блокировки. После успешного входа — главный экран галереи.

use std::sync::Arc;

use photo_explorer::ui::{run_gallery, run_lock_screen, LockController};

fn main() {
    // Логирование — САМОЕ ПЕРВОЕ: до любых операций, чтобы любая паника
    // попала в лог. Файл: %TEMP%/photo_explorer/photo_explorer.log
    let log_path = photo_explorer::logging::init();
    log::info!("📷 Photo Explorer — запуск; лог: {log_path:?}");

    // PIN хранится в temp-папке пользователя (KISS). В продакшене — рядом с приложением.
    let pin_file = std::env::temp_dir().join("photo_explorer_pin.hash");
    log::debug!("PIN-файл: {pin_file:?}");
    let controller = Arc::new(LockController::new(pin_file));

    match run_lock_screen(controller.clone()) {
        Ok(()) => {
            if controller.is_granted() {
                log::info!("✅ Доступ разрешён. Открываю галерею.");
                if let Err(e) = run_gallery() {
                    log::error!("Ошибка галереи: {e}");
                }
            } else {
                log::info!("Окно закрыто без входа.");
            }
        }
        Err(e) => {
            log::error!("Ошибка интерфейса блокировки: {e}");
        }
    }
    log::info!("Photo Explorer — завершение.");
}
