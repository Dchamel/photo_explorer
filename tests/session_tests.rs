//! Интеграционные тесты сессионного хранилища (4.7 «Проекты и недавние»).
//!
//! Проверяют порт [`SessionStore`] через реализацию [`JsonSessionStore`]:
//! сохранение/чтение/удаление сессий, лёгкий index.json, поиск по папке.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use photo_explorer::core::models::{Photo, Session};
use photo_explorer::core::ports::{SessionError, SessionStore};
use photo_explorer::session::JsonSessionStore;

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_dir(prefix: &str) -> PathBuf {
    let n = DIR_COUNTER.fetch_add(1, Ordering::SeqCst);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("photo_explorer_{prefix}_{stamp}_{n}"))
}

fn photo(path: &str, size: u64, modified: u64) -> Photo {
    Photo {
        path: PathBuf::from(path),
        file_name: path.rsplit(['\\', '/']).next().unwrap_or(path).to_string(),
        size_bytes: size,
        modified,
    }
}

fn sample_session(id: &str, folder: &str) -> Session {
    Session {
        id: id.to_string(),
        name: "Мой проект".to_string(),
        folder: PathBuf::from(folder),
        mode: "recursive".to_string(),
        fingerprint: "abc123".to_string(),
        photos: vec![photo("C:/x/1.jpg", 10, 100), photo("C:/x/2.jpg", 20, 200)],
        last_opened: 1700000000,
        is_project: true,
        nav_mode: "edge".to_string(), // 4.4a — режим листания по умолчанию
    }
}

#[test]
fn save_then_load_roundtrip() {
    let dir = unique_temp_dir("sessions");
    let _ = fs::remove_dir_all(&dir);
    let store = JsonSessionStore::new(dir.clone());

    let session = sample_session("s1", "C:/work/photos");
    store.save(&session).expect("save");
    let loaded = store.load("s1").expect("load");
    assert_eq!(loaded, session);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn summaries_list_sessions_sorted_fresh_first() {
    let dir = unique_temp_dir("sessions");
    let _ = fs::remove_dir_all(&dir);
    let store = JsonSessionStore::new(dir.clone());

    let mut older = sample_session("older", "C:/a");
    older.last_opened = 100;
    let mut newer = sample_session("newer", "C:/b");
    newer.last_opened = 200;
    store.save(&older).expect("save older");
    store.save(&newer).expect("save newer");

    let list = store.summaries().expect("summaries");
    assert_eq!(list.len(), 2);
    // Свежие сверху.
    assert_eq!(list[0].id, "newer");
    assert_eq!(list[1].id, "older");
    // В summary нет списка фото (лёгкий индекс).
    // (поле photos отсутствует в типе — проверяем компиляцией.)

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn save_upsert_updates_existing_session() {
    let dir = unique_temp_dir("sessions");
    let _ = fs::remove_dir_all(&dir);
    let store = JsonSessionStore::new(dir.clone());

    store.save(&sample_session("s1", "C:/a")).expect("save 1");
    let mut updated = sample_session("s1", "C:/a");
    updated.name = "Переименован".to_string();
    updated.last_opened = 999;
    store.save(&updated).expect("save 2");

    let list = store.summaries().expect("summaries");
    assert_eq!(list.len(), 1, "upsert не должен плодить дубли");
    assert_eq!(list[0].name, "Переименован");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn delete_removes_session_and_index_entry() {
    let dir = unique_temp_dir("sessions");
    let _ = fs::remove_dir_all(&dir);
    let store = JsonSessionStore::new(dir.clone());

    store.save(&sample_session("s1", "C:/a")).expect("save");
    store.delete("s1").expect("delete");

    assert!(matches!(store.load("s1"), Err(SessionError::NotFound(_))));
    assert!(store.summaries().expect("summaries").is_empty());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn find_by_folder_matches_path_and_mode() {
    let dir = unique_temp_dir("sessions");
    let _ = fs::remove_dir_all(&dir);
    let store = JsonSessionStore::new(dir.clone());

    store
        .save(&sample_session("s1", "C:/work/photos"))
        .expect("save");

    // Тот же путь и режим — находим.
    let found = store
        .find_by_folder(std::path::Path::new("C:/work/photos"), "recursive")
        .expect("find");
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, "s1");

    // Другой режим — не находим (одна папка = одна сессия на режим).
    let wrong_mode = store
        .find_by_folder(std::path::Path::new("C:/work/photos"), "flat")
        .expect("find flat");
    assert!(wrong_mode.is_none());

    // Чужой путь — не находим.
    let wrong_path = store
        .find_by_folder(std::path::Path::new("C:/other"), "recursive")
        .expect("find other");
    assert!(wrong_path.is_none());

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn load_missing_session_returns_not_found() {
    let dir = unique_temp_dir("sessions");
    let _ = fs::remove_dir_all(&dir);
    let store = JsonSessionStore::new(dir.clone());
    assert!(matches!(store.load("nope"), Err(SessionError::NotFound(_))));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn corrupted_index_is_reported_not_panicked() {
    let dir = unique_temp_dir("sessions");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("mkdir");
    fs::write(dir.join("index.json"), "{ не json").expect("write broken");
    let store = JsonSessionStore::new(dir.clone());
    assert!(matches!(store.summaries(), Err(SessionError::Corrupted(_))));
    let _ = fs::remove_dir_all(&dir);
}
