//! Слой интерфейса на Slint.
//!
//! Экран блокировки (одно окно): точки PIN + встроенная клавиатура + автовход.
//! Биометрия запускается в ОТДЕЛЬНОМ потоке (op.get() блокирующий) и результат
//! возвращается в UI-поток через slint::invoke_from_event_loop. Флаг «идёт» защищает
//! от повторных запросов (иначе дедлок/зависание).

slint::include_modules!();

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::core::ports::{AuthOutcome, Authenticator};
use crate::security::SecurityService;

/// Результат попытки входа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockResult {
    Granted,
    Denied,
}

/// Контроллер экрана блокировки: связывает UI с сервисом безопасности.
pub struct LockController {
    service: Arc<SecurityService>,
    granted: Mutex<bool>,
}

impl LockController {
    pub fn new(pin_file: PathBuf) -> Self {
        Self {
            service: Arc::new(SecurityService::new(pin_file)),
            granted: Mutex::new(false),
        }
    }

    pub fn has_pin(&self) -> bool {
        self.service.has_pin()
    }

    /// Проверка PIN (при входе). Синхронная, но быстрая — можно в UI-потоке.
    pub fn verify_pin(&self, pin: &str) -> LockResult {
        match self.service.verify_pin(pin) {
            Ok(AuthOutcome::Granted) => {
                *self.granted.lock().unwrap() = true;
                LockResult::Granted
            }
            _ => LockResult::Denied,
        }
    }

    /// Создание PIN (при первом запуске). Валидация внутри сервиса.
    pub fn create_pin(&self, pin: &str) -> Result<(), String> {
        self.service.set_pin(pin).map_err(|e| e.to_string())
    }

    /// Биометрия (Windows Hello). БЛОКИРУЮЩАЯ — вызывать только в отдельном потоке.
    pub fn biometric(&self) -> LockResult {
        match self.service.authenticate_biometric() {
            Ok(AuthOutcome::Granted) => {
                *self.granted.lock().unwrap() = true;
                LockResult::Granted
            }
            _ => LockResult::Denied,
        }
    }

    pub fn is_granted(&self) -> bool {
        *self.granted.lock().unwrap()
    }
}

/// Запускает экран блокировки и блокирует поток до успешного входа.
pub fn run_lock_screen(controller: Arc<LockController>) -> Result<(), slint::PlatformError> {
    let ui = LockScreen::new()?;

    let creating = !controller.has_pin();
    ui.set_has_pin(!creating);
    ui.set_status_text(if creating {
        "Photo Explorer — создайте PIN".into()
    } else {
        "Photo Explorer".into()
    });
    #[cfg(windows)]
    ui.set_biometric_available(true);
    #[cfg(not(windows))]
    ui.set_biometric_available(false);

    // Состояние ввода PIN (живёт в UI-потоке, Rc безопасен).
    let entered = Rc::new(RefCell::new(String::new()));
    let max_len = 4usize;

    // Флаг «биометрия идёт» — защита от повторных запросов (дедлок).
    let biometric_in_progress = Arc::new(AtomicBool::new(false));

    /// Запускает биометрию в фоновом потоке; результат обрабатывается в UI-потоке.
    /// Если проверка уже идёт — игнорирует вызов.
    fn request_biometric(
        controller: &Arc<LockController>,
        ui_weak: slint::Weak<LockScreen>,
        flag: &Arc<AtomicBool>,
    ) {
        // Защита от повторного запуска: если уже идёт — выходим.
        if flag
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        let controller = controller.clone();
        let flag = flag.clone();
        let ui_weak = ui_weak.clone();
        std::thread::spawn(move || {
            let result = controller.biometric();
            // Возвращаем результат в UI-поток.
            let ui_weak = ui_weak.clone();
            let flag = flag.clone();
            let _ = slint::invoke_from_event_loop(move || {
                flag.store(false, Ordering::SeqCst);
                if let Some(w) = ui_weak.upgrade() {
                    match result {
                        LockResult::Granted => {
                            let _ = w.hide();
                        }
                        LockResult::Denied => {
                            w.set_error_text("Биометрия отклонена или недоступна".into());
                        }
                    }
                }
            });
        });
    }

    // Колбэк «Войти по PIN» — показать встроенную клавиатуру.
    {
        let ui_weak = ui.as_weak();
        ui.on_enter_pin(move || {
            if let Some(w) = ui_weak.upgrade() {
                w.set_show_keyboard(true);
                w.set_error_text("".into());
            }
        });
    }

    // Колбэк нажатия цифры — накопление + автовход на 4-й цифре.
    {
        let ctrl = controller.clone();
        let ui_weak = ui.as_weak();
        let entered = entered.clone();
        ui.on_digit_pressed(move |digit| {
            let mut buf = entered.borrow_mut();
            if buf.len() < max_len {
                buf.push(char::from_digit(digit as u32, 10).unwrap_or('0'));
            }
            let count = buf.len() as i32;
            if let Some(w) = ui_weak.upgrade() {
                w.set_entered_count(count);
                w.set_error_text("".into());
            }
            // На 4-й цифре — проверяем (автовход).
            if buf.len() == max_len {
                let pin = buf.clone();
                let creating = !ctrl.has_pin();
                if creating {
                    match ctrl.create_pin(&pin) {
                        Ok(()) => {
                            if let Some(w) = ui_weak.upgrade() {
                                w.set_status_text("PIN создан. Войдите по PIN.".into());
                                w.set_error_text("".into());
                                w.set_entered_count(0);
                            }
                            *buf = String::new();
                        }
                        Err(e) => {
                            if let Some(w) = ui_weak.upgrade() {
                                w.set_error_text(e.into());
                                w.set_entered_count(0);
                            }
                            *buf = String::new();
                        }
                    }
                } else {
                    match ctrl.verify_pin(&pin) {
                        LockResult::Granted => {
                            if let Some(w) = ui_weak.upgrade() {
                                let _ = w.hide();
                            }
                        }
                        LockResult::Denied => {
                            if let Some(w) = ui_weak.upgrade() {
                                w.set_error_text("Неверный PIN".into());
                                w.set_entered_count(0);
                            }
                            *buf = String::new();
                        }
                    }
                }
            }
        });
    }

    // Колбэк стирания.
    {
        let ui_weak = ui.as_weak();
        let entered = entered.clone();
        ui.on_backspace(move || {
            let mut buf = entered.borrow_mut();
            buf.pop();
            if let Some(w) = ui_weak.upgrade() {
                w.set_entered_count(buf.len() as i32);
                w.set_error_text("".into());
            }
        });
    }

    // Колбэк «Отпечаток пальца» — ручной повтор (уважает флаг «идёт»).
    {
        let ctrl = controller.clone();
        let ui_weak = ui.as_weak();
        let flag = biometric_in_progress.clone();
        ui.on_biometric(move || {
            request_biometric(&ctrl, ui_weak.clone(), &flag);
        });
    }

    // Автозапрос биометрии при старте — один раз, если PIN задан.
    // Через короткую задержку, чтобы окно успело показаться.
    #[cfg(windows)]
    {
        let ctrl = controller.clone();
        let ui_weak = ui.as_weak();
        let flag = biometric_in_progress.clone();
        slint::Timer::single_shot(Duration::from_millis(400), move || {
            if ctrl.has_pin() {
                request_biometric(&ctrl, ui_weak, &flag);
            }
        });
    }

    ui.run()
}
