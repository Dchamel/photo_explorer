//! Чистая логика хеширования и проверки PIN через Argon2.
//!
//! Не зависит от ОС и файловой системы — поэтому полностью покрывается автотестами.
//! PIN хранится/передаётся только в виде хеша (PHC-строка Argon2).

use argon2::password_hash::{
    Error as HashError, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::Argon2;
use rand_core::OsRng;

use crate::core::ports::{AuthOutcome, SecurityError};

/// Ожидаемая длина PIN (цифр).
pub const PIN_LENGTH: usize = 4;

/// Проверяет, что PIN состоит ровно из [`PIN_LENGTH`] цифр (0-9).
///
/// Возвращает `Ok(())` для корректного PIN и `Err` с понятным сообщением иначе.
pub fn validate_pin(pin: &str) -> Result<(), SecurityError> {
    if pin.is_empty() {
        return Err(SecurityError::HashError("PIN пустой".into()));
    }
    if pin.len() != PIN_LENGTH {
        return Err(SecurityError::HashError(format!(
            "PIN должен содержать ровно {PIN_LENGTH} цифры, введено {}",
            pin.len()
        )));
    }
    if !pin.chars().all(|c| c.is_ascii_digit()) {
        return Err(SecurityError::HashError(
            "PIN должен состоять только из цифр".into(),
        ));
    }
    Ok(())
}

/// Хеширует PIN и возвращает строку в PHC-формате (с солью и параметрами).
///
/// Соль генерируется случайно при каждом вызове, поэтому два одинаковых PIN
/// дадут разные хеши.
pub fn hash_pin(pin: &str) -> Result<String, SecurityError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|e| SecurityError::HashError(e.to_string()))
}

/// Проверяет PIN против сохранённого хеша (PHC-строка).
///
/// Возвращает [`AuthOutcome::Granted`] при совпадении, [`AuthOutcome::Denied`] при несовпадении.
pub fn verify_pin(pin: &str, stored_hash: &str) -> Result<AuthOutcome, SecurityError> {
    let parsed_hash = PasswordHash::new(stored_hash)
        .map_err(|e: HashError| SecurityError::HashError(e.to_string()))?;
    match Argon2::default().verify_password(pin.as_bytes(), &parsed_hash) {
        Ok(()) => Ok(AuthOutcome::Granted),
        Err(HashError::Password) => Ok(AuthOutcome::Denied),
        Err(e) => Err(SecurityError::HashError(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correct_pin_verifies_as_granted() {
        let hash = hash_pin("1234").unwrap();
        assert_eq!(verify_pin("1234", &hash).unwrap(), AuthOutcome::Granted);
    }

    #[test]
    fn wrong_pin_verifies_as_denied() {
        let hash = hash_pin("1234").unwrap();
        assert_eq!(verify_pin("0000", &hash).unwrap(), AuthOutcome::Denied);
    }

    #[test]
    fn hash_is_not_the_plain_pin() {
        let hash = hash_pin("1234").unwrap();
        assert!(
            !hash.contains("1234"),
            "хеш не должен содержать исходный PIN"
        );
    }

    #[test]
    fn same_pin_produces_different_hashes() {
        let h1 = hash_pin("1234").unwrap();
        let h2 = hash_pin("1234").unwrap();
        assert_ne!(h1, h2, "разная соль => разные хеши для одинакового PIN");
    }

    #[test]
    fn empty_pin_still_hashes_and_verifies() {
        let hash = hash_pin("").unwrap();
        assert_eq!(verify_pin("", &hash).unwrap(), AuthOutcome::Granted);
        assert_eq!(verify_pin("x", &hash).unwrap(), AuthOutcome::Denied);
    }

    #[test]
    fn malformed_hash_returns_error() {
        let result = verify_pin("1234", "not-a-valid-argon2-hash");
        assert!(result.is_err(), "битый хеш должен дать ошибку, а не Denied");
    }

    #[test]
    fn validate_accepts_four_digits() {
        assert!(validate_pin("1234").is_ok());
        assert!(validate_pin("0000").is_ok());
        assert!(validate_pin("9999").is_ok());
    }

    #[test]
    fn validate_rejects_empty() {
        assert!(validate_pin("").is_err(), "пустой PIN недопустим");
    }

    #[test]
    fn validate_rejects_wrong_length() {
        assert!(validate_pin("123").is_err(), "3 цифры недопустимы");
        assert!(validate_pin("12345").is_err(), "5 цифр недопустимы");
    }

    #[test]
    fn validate_rejects_non_digits() {
        assert!(validate_pin("12a4").is_err(), "буквы недопустимы");
        assert!(validate_pin("12 4").is_err(), "пробелы недопустимы");
        assert!(validate_pin("1.34").is_err(), "точки недопустимы");
    }
}
