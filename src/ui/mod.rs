//! Слой интерфейса на Slint.
//!
//! Сейчас: экран блокировки (`LockScreen`) и его связка с `SecurityService`.
//! Главный экран галереи будет добавлен на следующем под-шаге Этапа 3.

slint::include_modules!();

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::core::ports::{AuthOutcome, Authenticator};
use crate::security::SecurityService;

/// Результат попытки входа с экрана блокировки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockResult {
    /// Доступ разрешён — можно показывать главный экран.
    Granted,
    /// Доступ отклонён (неверный PIN / отказ биометрии).
    Denied,
}

/// Контроллер экрана блокировки: связывает Slint-разметку с сервисом безопасности.
pub struct LockController {
    service: Arc<SecurityService>,
    /// Признак того, что вход уже выполнен в этом запуске.
    granted: Mutex<bool>,
}

impl LockController {
    /// Создаёт контроллер. PIN хранится в файле рядом с приложением (KISS).
    pub fn new(pin_file: PathBuf) -> Self {
        Self {
            service: Arc::new(SecurityService::new(pin_file)),
            granted: Mutex::new(false),
        }
    }

    /// Установлен ли PIN (для выбора режима: вход или создание).
    pub fn has_pin(&self) -> bool {
        self.service.has_pin()
    }

    /// Обработка нажатия «Войти» — проверка PIN.
    pub fn on_login(&self, pin: &str) -> LockResult {
        match self.service.verify_pin(pin) {
            Ok(AuthOutcome::Granted) => {
                *self.granted.lock().unwrap() = true;
                LockResult::Granted
            }
            Ok(AuthOutcome::Denied) => LockResult::Denied,
            Err(_) => LockResult::Denied,
        }
    }

    /// Обработка нажатия «Создать PIN».
    pub fn on_create_pin(&self, pin: &str) -> Result<(), String> {
        self.service.set_pin(pin).map_err(|e| e.to_string())
    }

    /// Обработка нажатия «Отпечаток пальца» — Windows Hello.
    pub fn on_biometric(&self) -> LockResult {
        match self.service.authenticate_biometric() {
            Ok(AuthOutcome::Granted) => {
                *self.granted.lock().unwrap() = true;
                LockResult::Granted
            }
            _ => LockResult::Denied,
        }
    }

    /// Доступ уже был разрешён в этом запуске.
    pub fn is_granted(&self) -> bool {
        *self.granted.lock().unwrap()
    }
}

/// Запускает экран блокировки и блокирует поток до успешного входа.
///
/// Возвращает `Ok(())` после успешной аутентификации (далее — главный экран).
pub fn run_lock_screen(controller: Arc<LockController>) -> Result<(), slint::PlatformError> {
    let ui = LockScreen::new()?;

    // Начальный режим: создание PIN, если его ещё нет.
    let creating = !controller.has_pin();
    ui.set_is_creating_pin(creating);
    ui.set_status_text(if creating {
        "Создайте PIN (4 цифры)".into()
    } else {
        "Введите PIN".into()
    });
    // Биометрия доступна только на Windows; для MVP показываем кнопку всегда на Windows.
    #[cfg(windows)]
    ui.set_biometric_available(true);
    #[cfg(not(windows))]
    ui.set_biometric_available(false);

    // Колбэк «Войти».
    {
        let ctrl = controller.clone();
        let ui_weak = ui.as_weak();
        ui.on_login(move || {
            let pin = ui_weak.unwrap().get_pin_text().to_string();
            match ctrl.on_login(&pin) {
                LockResult::Granted => {
                    let _ = ui_weak.unwrap().hide();
                }
                LockResult::Denied => {
                    let w = ui_weak.unwrap();
                    w.set_error_text("Неверный PIN".into());
                    w.set_pin_text("".into());
                }
            }
        });
    }

    // Колбэк «Создать PIN».
    {
        let ctrl = controller.clone();
        let ui_weak = ui.as_weak();
        ui.on_create_pin(move || {
            let pin = ui_weak.unwrap().get_pin_text().to_string();
            match ctrl.on_create_pin(&pin) {
                Ok(()) => {
                    let w = ui_weak.unwrap();
                    w.set_error_text("".into());
                    w.set_is_creating_pin(false);
                    w.set_status_text("PIN создан. Введите его для входа.".into());
                    w.set_pin_text("".into());
                }
                Err(e) => {
                    let w = ui_weak.unwrap();
                    w.set_error_text(format!("Ошибка: {e}").into());
                }
            }
        });
    }

    // Колбэк «Отпечаток пальца».
    {
        let ctrl = controller.clone();
        let ui_weak = ui.as_weak();
        ui.on_biometric(move || match ctrl.on_biometric() {
            LockResult::Granted => {
                let _ = ui_weak.unwrap().hide();
            }
            LockResult::Denied => {
                let w = ui_weak.unwrap();
                w.set_error_text("Биометрия отклонена или недоступна".into());
            }
        });
    }

    ui.run()
}
