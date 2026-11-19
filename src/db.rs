use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct System {
    pub edsm_id: i64,
    pub id64: Option<i64>,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub updated_at: Option<String>,
}

#[derive(Debug, Error)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database path has no parent directory")]
    InvalidPath,
}

pub fn open_database(path: impl AsRef<Path>) -> Result<Connection, DbError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|_| DbError::InvalidPath)?;
        }
    }
    let conn = Connection::open(path)?;
    configure_connection(&conn)?;
    Ok(conn)
}

pub fn configure_connection(conn: &Connection) -> Result<(), DbError> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -16384;
         PRAGMA temp_store = MEMORY;",
    )?;
    Ok(())
}

pub fn initialize_database(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS systems (
            edsm_id INTEGER PRIMARY KEY,
            id64 INTEGER,
            name TEXT NOT NULL,
            name_norm TEXT NOT NULL,
            x REAL NOT NULL,
            y REAL NOT NULL,
            z REAL NOT NULL,
            updated_at TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_systems_name
            ON systems(name_norm, edsm_id);
        CREATE INDEX IF NOT EXISTS idx_systems_id64
            ON systems(id64);

        CREATE VIRTUAL TABLE IF NOT EXISTS systems_rtree USING rtree(
            edsm_id,
            min_x, max_x,
            min_y, max_y,
            min_z, max_z
        );

        CREATE TRIGGER IF NOT EXISTS systems_ai AFTER INSERT ON systems BEGIN
            INSERT OR REPLACE INTO systems_rtree
            VALUES (new.edsm_id, new.x, new.x, new.y, new.y, new.z, new.z);
        END;

        CREATE TRIGGER IF NOT EXISTS systems_au AFTER UPDATE OF x, y, z ON systems BEGIN
            INSERT OR REPLACE INTO systems_rtree
            VALUES (new.edsm_id, new.x, new.x, new.y, new.y, new.z, new.z);
        END;

        CREATE TRIGGER IF NOT EXISTS systems_ad AFTER DELETE ON systems BEGIN
            DELETE FROM systems_rtree WHERE edsm_id = old.edsm_id;
        END;",
    )?;
    Ok(())
}

pub fn normalize_name(name: &str) -> String {
    name.trim().to_ascii_lowercase()
}

pub fn upsert_system(conn: &Connection, system: &System) -> Result<(), DbError> {
    let name_norm = normalize_name(&system.name);
    conn.execute(
        "INSERT INTO systems
            (edsm_id, id64, name, name_norm, x, y, z, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(edsm_id) DO UPDATE SET
            id64 = excluded.id64,
            name = excluded.name,
            name_norm = excluded.name_norm,
            x = excluded.x,
            y = excluded.y,
            z = excluded.z,
            updated_at = excluded.updated_at",
        params![
            system.edsm_id,
            system.id64,
            system.name,
            name_norm,
            system.x,
            system.y,
            system.z,
            system.updated_at
        ],
    )?;
    Ok(())
}

pub fn get_system(conn: &Connection, edsm_id: i64) -> Result<Option<System>, DbError> {
    conn.query_row(
        "SELECT edsm_id, id64, name, x, y, z, updated_at
         FROM systems WHERE edsm_id = ?1",
        [edsm_id],
        |row| {
            Ok(System {
                edsm_id: row.get(0)?,
                id64: row.get(1)?,
                name: row.get(2)?,
                x: row.get(3)?,
                y: row.get(4)?,
                z: row.get(5)?,
                updated_at: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(DbError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: i64, name: &str, x: f64) -> System {
        System {
            edsm_id: id,
            id64: None,
            name: name.to_owned(),
            x,
            y: 0.0,
            z: 0.0,
            updated_at: None,
        }
    }

    #[test]
    fn rtree_tracks_insert_update_and_delete() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_database(&conn).unwrap();

        upsert_system(&conn, &sample(1, "Sol", 1.0)).unwrap();
        let first: i64 = conn.query_row(
            "SELECT count(*) FROM systems_rtree WHERE edsm_id = 1 AND min_x <= 1.0 AND max_x >= 1.0",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(first, 1);

        upsert_system(&conn, &sample(1, "Sol", 42.0)).unwrap();
        let moved: i64 = conn.query_row(
            "SELECT count(*) FROM systems_rtree WHERE edsm_id = 1 AND min_x <= 42.0 AND max_x >= 42.0",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(moved, 1);

        conn.execute("DELETE FROM systems WHERE edsm_id = 1", []).unwrap();
        let deleted: i64 = conn.query_row(
            "SELECT count(*) FROM systems_rtree WHERE edsm_id = 1",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(deleted, 0);
    }
}
