//! Наблюдатель блокировки сессии Windows.
//!
//! Полная реализация с циклом сообщений Win32 (`WTSRegisterSessionNotification` +
//! `WM_WTSSESSION_CHANGE`) требует интерактивной графической сессии и не может быть
//! протестирована в CI. Поэтому сейчас здесь заглушка, а полноценная реализация
//! будет подключена на Этапе 3 (UI) вместе с окном приложения, где её можно
//! проверить на реальной машине.
//!
//! Только для Windows (`cfg(windows)`).

use crate::core::ports::SecurityError;

/// Наблюдатель блокировки сессии Windows.
pub struct WindowsSessionLockWatcher;

impl WindowsSessionLockWatcher {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WindowsSessionLockWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl crate::core::ports::SessionLockWatcher for WindowsSessionLockWatcher {
    fn watch<F>(&self, _on_lock: F, _on_unlock: F) -> Result<(), SecurityError>
    where
        F: Fn() + Send + 'static,
    {
        // TODO(Этап 3, UI): подключить цикл сообщений Win32 с
        // WTSRegisterSessionNotification и WM_WTSSESSION_CHANGE,
        // чтобы вызывать on_lock/on_unlock при Win+L. Требует окна приложения.
        Err(SecurityError::BiometricUnavailable(
            "наблюдение блокировки сессии будет подключено на Этапе 3 (UI)".into(),
        ))
    }
}
