//! Слой интерфейса на Slint.
//!
//! Экран блокировки (LockScreen) + экранная клавиатура PIN (PinPad).
//! Связка с SecurityService. Автозапрос биометрии при старте.

slint::include_modules!();

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

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

    /// Проверка PIN (при входе).
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

    /// Биометрия (Windows Hello).
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

    // Колбэк «Войти по PIN» — открывает клавиатуру.
    {
        let ctrl = controller.clone();
        let ui_weak = ui.as_weak();
        ui.on_enter_pin(move || {
            let creating = !ctrl.has_pin();
            if let Err(e) = open_pin_pad(ctrl.clone(), ui_weak.clone(), creating) {
                eprintln!("Ошибка клавиатуры PIN: {e}");
            }
        });
    }

    // Колбэк «Отпечаток пальца».
    {
        let ctrl = controller.clone();
        let ui_weak = ui.as_weak();
        ui.on_biometric(move || match ctrl.biometric() {
            LockResult::Granted => {
                let _ = ui_weak.unwrap().hide();
            }
            LockResult::Denied => {
                ui_weak
                    .unwrap()
                    .set_error_text("Биометрия отклонена или недоступна".into());
            }
        });
    }

    // Автозапрос биометрии при старте (если доступна и PIN уже задан).
    #[cfg(windows)]
    {
        let ctrl = controller.clone();
        let ui_weak = ui.as_weak();
        let _ = ui_weak.clone();
        std::thread::spawn(move || {
            if ctrl.has_pin() && ctrl.biometric() == LockResult::Granted {
                if let Some(w) = ui_weak.upgrade() {
                    let _ = w.hide();
                }
            }
        });
    }

    ui.run()
}

/// Открывает экранную клавиатуру PIN как отдельное окно.
fn open_pin_pad(
    controller: Arc<LockController>,
    lock_ui: slint::Weak<LockScreen>,
    creating: bool,
) -> Result<(), slint::PlatformError> {
    let pad = PinPad::new()?;
    pad.set_is_creating_pin(creating);
    pad.set_status_text(if creating {
        "Создайте PIN (4 цифры)".into()
    } else {
        "Введите PIN".into()
    });

    // Накапливаем введённые цифры в RefCell (окно живёт в одном потоке).
    let entered = Rc::new(RefCell::new(String::new()));
    let max_len = 4usize;

    // Колбэк нажатия цифры.
    {
        let ctrl = controller.clone();
        let lock_ui = lock_ui.clone();
        let pad_weak = pad.as_weak();
        let entered = entered.clone();
        pad.on_digit_pressed(move |digit| {
            let mut buf = entered.borrow_mut();
            if buf.len() < max_len {
                buf.push(char::from_digit(digit as u32, 10).unwrap_or('0'));
            }
            let count = buf.len() as i32;
            if let Some(w) = pad_weak.upgrade() {
                w.set_entered_count(count);
                w.set_error_text("".into());
            }
            // Когда набрано 4 цифры — проверяем.
            if buf.len() == max_len {
                let pin = buf.clone();
                let creating = !ctrl.has_pin();
                if creating {
                    match ctrl.create_pin(&pin) {
                        Ok(()) => {
                            if let Some(w) = pad_weak.upgrade() {
                                w.set_status_text("PIN создан. Войдите по PIN.".into());
                                w.set_error_text("".into());
                            }
                            *buf = String::new();
                            if let Some(w) = pad_weak.upgrade() {
                                w.set_entered_count(0);
                            }
                        }
                        Err(e) => {
                            if let Some(w) = pad_weak.upgrade() {
                                w.set_error_text(e.into());
                            }
                            *buf = String::new();
                            if let Some(w) = pad_weak.upgrade() {
                                w.set_entered_count(0);
                            }
                        }
                    }
                } else {
                    match ctrl.verify_pin(&pin) {
                        LockResult::Granted => {
                            if let Some(w) = pad_weak.upgrade() {
                                let _ = w.hide();
                            }
                            if let Some(lock) = lock_ui.upgrade() {
                                let _ = lock.hide();
                            }
                        }
                        LockResult::Denied => {
                            if let Some(w) = pad_weak.upgrade() {
                                w.set_error_text("Неверный PIN".into());
                            }
                            *buf = String::new();
                            if let Some(w) = pad_weak.upgrade() {
                                w.set_entered_count(0);
                            }
                        }
                    }
                }
            }
        });
    }

    // Колбэк стирания.
    {
        let pad_weak = pad.as_weak();
        let entered = entered.clone();
        pad.on_backspace(move || {
            let mut buf = entered.borrow_mut();
            buf.pop();
            if let Some(w) = pad_weak.upgrade() {
                w.set_entered_count(buf.len() as i32);
                w.set_error_text("".into());
            }
        });
    }

    pad.run()
}
