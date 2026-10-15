//! Интеграционные тесты модуля `security`.
//!
//! Покрывают только чистую PIN-логику (Argon2) и сервис [`SecurityService`].
//! Windows Hello и блокировка сессии автотестами НЕ покрываются —
//! требуют интерактивной сессии со сканером отпечатка, которой нет в CI.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use photo_explorer::core::ports::{AuthOutcome, Authenticator, SecurityError};
use photo_explorer::security::SecurityService;

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_pin_file() -> PathBuf {
    let n = DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("photo_explorer_pin_{stamp}_{n}.txt"))
}

#[test]
fn set_and_verify_correct_pin() {
    let file = unique_pin_file();
    let svc = SecurityService::new(file.clone());

    assert!(!svc.has_pin(), "до установки PIN не задан");
    svc.set_pin("1234").expect("set_pin должен работать");
    assert!(svc.has_pin(), "после установки PIN задан");

    let outcome = svc.verify_pin("1234").expect("verify_pin должен работать");
    assert_eq!(outcome, AuthOutcome::Granted);

    fs::remove_file(file).ok();
}

#[test]
fn wrong_pin_is_denied() {
    let file = unique_pin_file();
    let svc = SecurityService::new(file.clone());

    svc.set_pin("1234").unwrap();
    let outcome = svc.verify_pin("0000").unwrap();
    assert_eq!(outcome, AuthOutcome::Denied);

    fs::remove_file(file).ok();
}

#[test]
fn pin_is_stored_as_hash_not_plaintext() {
    let file = unique_pin_file();
    let svc = SecurityService::new(file.clone());

    svc.set_pin("1234").unwrap();
    let stored = fs::read_to_string(&file).unwrap();
    assert!(
        !stored.contains("1234"),
        "на диске не должно быть исходного PIN"
    );
    assert!(!stored.is_empty(), "хеш должен быть сохранён");

    fs::remove_file(file).ok();
}

#[test]
fn verify_without_set_pin_returns_error() {
    let file = unique_pin_file();
    let svc = SecurityService::new(file.clone());

    let result = svc.verify_pin("1234");
    assert!(
        matches!(result, Err(SecurityError::PinNotSet)),
        "проверка без установки PIN должна дать PinNotSet"
    );

    fs::remove_file(file).ok();
}

#[test]
fn resetting_pin_overwrites_previous() {
    let file = unique_pin_file();
    let svc = SecurityService::new(file.clone());

    svc.set_pin("1234").unwrap();
    svc.set_pin("9999").unwrap();

    assert_eq!(svc.verify_pin("1234").unwrap(), AuthOutcome::Denied);
    assert_eq!(svc.verify_pin("9999").unwrap(), AuthOutcome::Granted);

    fs::remove_file(file).ok();
}
