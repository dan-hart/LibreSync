use std::path::PathBuf;

use libresync::{AdapterCache, DataAdapter, SqliteFileAdapter, State};
use rusqlite::{params, Connection};

fn wal_path(db_path: &PathBuf) -> PathBuf {
    PathBuf::from(format!("{}-wal", db_path.to_string_lossy()))
}

fn shm_path(db_path: &PathBuf) -> PathBuf {
    PathBuf::from(format!("{}-shm", db_path.to_string_lossy()))
}

#[test]
fn sqlite_adapter_loads_real_wal_and_shm() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("db.sqlite");

    let conn = Connection::open(&db_path).expect("open");
    conn.pragma_update(None, "journal_mode", &"WAL")
        .expect("wal");
    conn.pragma_update(None, "wal_autocheckpoint", &0)
        .expect("checkpoint");
    conn.execute(
        "CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY, name TEXT)",
        [],
    )
    .expect("create");
    conn.execute("INSERT INTO items (name) VALUES (?)", params!["alpha"])
        .expect("insert");
    let conn2 = Connection::open(&db_path).expect("open second");
    let _: i64 = conn2
        .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
        .expect("select");

    let wal = wal_path(&db_path);
    let shm = shm_path(&db_path);
    assert!(wal.exists(), "wal file should exist");
    assert!(shm.exists(), "shm file should exist");

    drop(conn2);
    drop(conn);

    let adapter = SqliteFileAdapter::new("db", &db_path);
    let mut state = State::new("device");
    adapter.load_into_state(&mut state).expect("load");

    assert!(state.get("db").is_some());
    assert!(state.get("db:wal").is_some());
    assert!(state.get("db:shm").is_some());
}

#[test]
fn sqlite_adapter_emits_page_delta_for_large_db() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("db.sqlite");

    let conn = Connection::open(&db_path).expect("open");
    conn.execute(
        "CREATE TABLE data (id INTEGER PRIMARY KEY, payload BLOB)",
        [],
    )
    .expect("create");

    let payload = vec![0u8; 2048];
    for _ in 0..200 {
        conn.execute("INSERT INTO data (payload) VALUES (?)", params![payload])
            .expect("insert");
    }
    drop(conn);

    let base_bytes = std::fs::read(&db_path).expect("read base");
    let adapter = SqliteFileAdapter::new("db", &db_path).with_page_delta(4096);
    let mut state = State::new("device");
    state.set("db", base_bytes);

    let conn = Connection::open(&db_path).expect("open update");
    let mut changed = vec![0u8; 2048];
    changed[0] = 1;
    conn.execute("UPDATE data SET payload = ? WHERE id = 1", params![changed])
        .expect("update");
    drop(conn);

    adapter.load_into_state(&mut state).expect("load delta");
    let delta = state.get("db:delta").expect("delta");
    assert!(!delta.is_empty(), "delta should be present");

    let mut cache = AdapterCache::default();
    adapter.sync_tick(&mut state, &mut cache).expect("sync");
}
