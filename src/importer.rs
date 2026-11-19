use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use flate2::read::GzDecoder;
use rusqlite::Connection;
use serde::de::{DeserializeSeed, SeqAccess, Visitor};
use serde_json::Value;
use thiserror::Error;

use crate::db::{DbError, System, upsert_system};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportStats {
    pub records_seen: u64,
    pub imported: u64,
    pub skipped: u64,
}

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("database error: {0}")]
    Database(#[from] DbError),
    #[error("batch size must be greater than zero")]
    InvalidBatchSize,
}

pub fn import_gzip_file(
    path: impl AsRef<Path>,
    conn: &Connection,
    batch_size: usize,
) -> Result<ImportStats, ImportError> {
    let file = File::open(path)?;
    import_json(GzDecoder::new(BufReader::new(file)), conn, batch_size)
}

pub fn import_json<R: Read>(
    reader: R,
    conn: &Connection,
    batch_size: usize,
) -> Result<ImportStats, ImportError> {
    if batch_size == 0 {
        return Err(ImportError::InvalidBatchSize);
    }
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let seed = ImportSeed { conn, batch_size };
    let stats = seed.deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(stats)
}

struct ImportSeed<'a> {
    conn: &'a Connection,
    batch_size: usize,
}

impl<'de> DeserializeSeed<'de> for ImportSeed<'_> {
    type Value = ImportStats;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(ImportVisitor {
            conn: self.conn,
            batch_size: self.batch_size,
        })
    }
}

struct ImportVisitor<'a> {
    conn: &'a Connection,
    batch_size: usize,
}

impl<'de> Visitor<'de> for ImportVisitor<'_> {
    type Value = ImportStats;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an EDSM JSON array of system records")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut stats = ImportStats::default();
        let mut batch = Vec::with_capacity(self.batch_size.min(4096));

        while let Some(value) = seq.next_element::<Value>()? {
            stats.records_seen += 1;
            match parse_system(&value) {
                Some(system) => batch.push(system),
                None => stats.skipped += 1,
            }

            if batch.len() >= self.batch_size {
                flush_batch(self.conn, &batch).map_err(serde::de::Error::custom)?;
                stats.imported += batch.len() as u64;
                batch.clear();
            }
        }

        if !batch.is_empty() {
            flush_batch(self.conn, &batch).map_err(serde::de::Error::custom)?;
            stats.imported += batch.len() as u64;
        }

        Ok(stats)
    }
}

fn parse_system(value: &Value) -> Option<System> {
    let edsm_id = value.get("id")?.as_i64()?;
    let name = value.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    let coords = value.get("coords")?;
    let x = coords.get("x")?.as_f64()?;
    let y = coords.get("y")?.as_f64()?;
    let z = coords.get("z")?.as_f64()?;
    if !(x.is_finite() && y.is_finite() && z.is_finite()) {
        return None;
    }

    Some(System {
        edsm_id,
        id64: value.get("id64").and_then(Value::as_i64),
        name: name.to_owned(),
        x,
        y,
        z,
        updated_at: value.get("date").and_then(Value::as_str).map(str::to_owned),
    })
}

fn flush_batch(conn: &Connection, systems: &[System]) -> Result<(), DbError> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    for system in systems {
        if let Err(error) = upsert_system(conn, system) {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    if let Err(error) = conn.execute_batch("COMMIT") {
        let _ = conn.execute_batch("ROLLBACK");
        return Err(error.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{get_system, initialize_database};

    #[test]
    fn imports_json_array_in_batches_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_database(&conn).unwrap();
        let json = br#"[
            {"id":1,"id64":100,"name":"Sol","coords":{"x":0,"y":0,"z":0},"date":"2026-01-01"},
            {"id":2,"name":"Bad","coords":{"x":"bad","y":0,"z":0}},
            {"id":3,"id64":null,"name":"Achenar","coords":{"x":1.5,"y":2,"z":3}}
        ]"#;

        let first = import_json(&json[..], &conn, 1).unwrap();
        assert_eq!(first.records_seen, 3);
        assert_eq!(first.imported, 2);
        assert_eq!(first.skipped, 1);
        let second = import_json(&json[..], &conn, 2).unwrap();
        assert_eq!(second.imported, 2);

        let sol = get_system(&conn, 1).unwrap().unwrap();
        assert_eq!(sol.name, "Sol");
        let count: i64 = conn.query_row("SELECT count(*) FROM systems", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn rejects_non_array_json() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_database(&conn).unwrap();
        assert!(import_json(br#"{"id":1}"#.as_slice(), &conn, 10).is_err());
    }
}
