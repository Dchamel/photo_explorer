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
}
