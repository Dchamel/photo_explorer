//! Photo Explorer — точка входа приложения.
//!
//! Этап 3: показываем экран блокировки. После успешного входа — главный экран галереи.

use std::sync::Arc;

use photo_explorer::ui::{run_gallery, run_lock_screen, LockController};

fn main() {
    println!("📷 Photo Explorer — запуск");

    // PIN хранится в temp-папке пользователя (KISS). В продакшене — рядом с приложением.
    let pin_file = std::env::temp_dir().join("photo_explorer_pin.hash");
    let controller = Arc::new(LockController::new(pin_file));

    match run_lock_screen(controller.clone()) {
        Ok(()) => {
            if controller.is_granted() {
                println!("✅ Доступ разрешён. Открываю галерею.");
                if let Err(e) = run_gallery() {
                    eprintln!("Ошибка галереи: {e}");
                }
            } else {
                println!("Окно закрыто без входа.");
            }
        }
        Err(e) => {
            eprintln!("Ошибка интерфейса блокировки: {e}");
        }
    }
}
