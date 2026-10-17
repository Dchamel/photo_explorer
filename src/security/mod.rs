//! Адаптер защиты доступа: реализация портов [`Authenticator`] и [`SessionLockWatcher`].
//!
//! Состоит из двух частей:
//! - `pin` — чистая, тестируемая логика хеширования PIN через Argon2 (без ОС);
//! - `windows_hello` / `session_lock` — Windows-специфика, изолирована за `cfg(windows)`.

pub mod pin;

#[cfg(windows)]
pub mod session_lock;

#[cfg(windows)]
pub mod windows_hello;

#[cfg(windows)]
pub use session_lock::WindowsSessionLockWatcher;

use std::path::PathBuf;

use crate::core::ports::{AuthOutcome, Authenticator, SecurityError};

/// Сервис защиты доступа: хранит хеш PIN в файле и (на Windows) обращается к Windows Hello.
///
/// Хеш PIN хранится в `pin_file` в виде строки Argon2 (PHC-формат). Сам PIN никогда
/// не сохраняется.
pub struct SecurityService {
    /// Путь к файлу с хешем PIN.
    pin_file: PathBuf,
}

impl SecurityService {
    /// Создаёт сервис, хранящий хеш PIN в указанном файле.
    pub fn new(pin_file: PathBuf) -> Self {
        Self { pin_file }
    }
}

impl Authenticator for SecurityService {
    fn set_pin(&self, pin: &str) -> Result<(), SecurityError> {
        // PIN задаётся один раз при установке; сброс — только удалением файла.
        // Валидация: ровно 4 цифры. Пустой/короткий/с буквами не принимается.
        pin::validate_pin(pin)?;
        let hashed = pin::hash_pin(pin)?;
        std::fs::write(&self.pin_file, hashed.as_bytes())
            .map_err(|e| SecurityError::IoError(e.to_string()))
    }

    fn verify_pin(&self, pin: &str) -> Result<AuthOutcome, SecurityError> {
        let Ok(stored) = std::fs::read_to_string(&self.pin_file) else {
            return Err(SecurityError::PinNotSet);
        };
        let stored = stored.trim();
        if stored.is_empty() {
            return Err(SecurityError::PinNotSet);
        }
        pin::verify_pin(pin, stored)
    }

    fn has_pin(&self) -> bool {
        std::fs::read_to_string(&self.pin_file)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    }

    fn authenticate_biometric(&self) -> Result<AuthOutcome, SecurityError> {
        #[cfg(windows)]
        {
            windows_hello::request_user_consent()
        }
        #[cfg(not(windows))]
        {
            Err(SecurityError::BiometricUnavailable(
                "биометрия доступна только на Windows".into(),
            ))
        }
    }
}
