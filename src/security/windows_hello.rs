//! Windows Hello: запрос биометрического подтверждения владельца.
//!
//! Использует `UserConsentVerifier` из WinRT. Приложение НЕ получает сам отпечаток —
//! ОС возвращает только ответ «подтверждено / отклонено». Это правильно и безопасно.
//!
//! Только для Windows (`cfg(windows)`).

use crate::core::ports::{AuthOutcome, SecurityError};

/// Запрашивает у Windows подтверждение пользователя (отпечаток/PIN Windows Hello).
///
/// Возвращает [`AuthOutcome::Granted`], если ОС подтвердила владельца,
/// [`AuthOutcome::Denied`] при отказе/отмене.
pub fn request_user_consent() -> Result<AuthOutcome, SecurityError> {
    use windows::core::HSTRING;
    use windows::Security::Credentials::UI::{UserConsentVerificationResult, UserConsentVerifier};

    let op = UserConsentVerifier::RequestVerificationAsync(&HSTRING::new())
        .map_err(|e| SecurityError::BiometricUnavailable(e.to_string()))?;
    let result: UserConsentVerificationResult = op
        .get()
        .map_err(|e| SecurityError::BiometricUnavailable(e.to_string()))?;

    if result == UserConsentVerificationResult::Verified {
        Ok(AuthOutcome::Granted)
    } else if result == UserConsentVerificationResult::Canceled {
        Ok(AuthOutcome::Denied)
    } else if result == UserConsentVerificationResult::DeviceNotPresent {
        Err(SecurityError::BiometricUnavailable(
            "сканер отпечатка не найден".into(),
        ))
    } else if result == UserConsentVerificationResult::NotConfiguredForUser {
        Err(SecurityError::BiometricUnavailable(
            "Windows Hello не настроен для текущего пользователя".into(),
        ))
    } else {
        Err(SecurityError::BiometricDenied(format!(
            "Windows Hello вернул статус {0}",
            result.0
        )))
    }
}
