use rusqlite::{Connection, params};
use serde::Serialize;
use thiserror::Error;

use crate::db::{DbError, System, normalize_name};

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("central system not found")]
    CenterNotFound,
    #[error("radius must be a finite positive number")]
    InvalidRadius,
    #[error("database error: {0}")]
    Database(#[from] DbError),
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NearbySystem {
    #[serde(flatten)]
    pub system: System,
    pub distance: f64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchResponse {
    pub center: System,
    pub radius: f64,
    pub count: usize,
    pub systems: Vec<NearbySystem>,
}

fn system_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<System> {
    Ok(System {
        edsm_id: row.get(0)?,
        id64: row.get(1)?,
        name: row.get(2)?,
        x: row.get(3)?,
        y: row.get(4)?,
        z: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

pub fn find_center(conn: &Connection, requested: &str) -> Result<System, SearchError> {
    let mut candidate = normalize_name(requested);
    if candidate.is_empty() {
        return Err(SearchError::CenterNotFound);
    }

    if let Some(system) = conn.query_row(
        "SELECT edsm_id, id64, name, x, y, z, updated_at
         FROM systems WHERE name_norm = ?1
         ORDER BY name_norm ASC, edsm_id ASC LIMIT 1",
        [&candidate],
        system_from_row,
    ).optional()? {
        return Ok(system);
    }

    loop {
        let pattern = format!("{}%", escape_like(&candidate));
        if let Some(system) = conn.query_row(
            "SELECT edsm_id, id64, name, x, y, z, updated_at
             FROM systems WHERE name_norm LIKE ?1 ESCAPE '\\'
             ORDER BY name_norm ASC, edsm_id ASC LIMIT 1",
            [&pattern],
            system_from_row,
        ).optional()? {
            return Ok(system);
        }

        let Some((prefix, _)) = candidate.rsplit_once(' ') else {
            break;
        };
        candidate = prefix.trim_end().to_owned();
        if candidate.is_empty() {
            break;
        }
    }

    Err(SearchError::CenterNotFound)
}

fn escape_like(value: &str) -> String {
    value.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

pub fn search_radius(
    conn: &Connection,
    center_name: &str,
    radius: f64,
) -> Result<SearchResponse, SearchError> {
    if !radius.is_finite() || radius <= 0.0 {
        return Err(SearchError::InvalidRadius);
    }
    let center = find_center(conn, center_name)?;
    let radius_squared = radius * radius;

    let mut stmt = conn.prepare(
        "SELECT s.edsm_id, s.id64, s.name, s.x, s.y, s.z, s.updated_at
         FROM systems_rtree r
         JOIN systems s ON s.edsm_id = r.edsm_id
         WHERE r.min_x <= ?1 AND r.max_x >= ?2
           AND r.min_y <= ?3 AND r.max_y >= ?4
           AND r.min_z <= ?5 AND r.max_z >= ?6",
    )?;

    let rows = stmt.query_map(
        params![
            center.x + radius, center.x - radius,
            center.y + radius, center.y - radius,
            center.z + radius, center.z - radius
        ],
        system_from_row,
    )?;

    let mut systems = Vec::new();
    for row in rows {
        let system = row?;
        let dx = system.x - center.x;
        let dy = system.y - center.y;
        let dz = system.z - center.z;
        let distance_squared = dx * dx + dy * dy + dz * dz;
        if distance_squared <= radius_squared {
            systems.push(NearbySystem {
                distance: distance_squared.sqrt(),
                system,
            });
        }
    }

    systems.sort_by(|a, b| {
        a.distance.total_cmp(&b.distance)
            .then_with(|| normalize_name(&a.system.name).cmp(&normalize_name(&b.system.name)))
            .then_with(|| a.system.edsm_id.cmp(&b.system.edsm_id))
    });

    let count = systems.len();
    Ok(SearchResponse { center, radius, count, systems })
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{initialize_database, upsert_system};

    fn add(conn: &Connection, id: i64, name: &str, x: f64, y: f64, z: f64) {
        upsert_system(conn, &System {
            edsm_id: id, id64: None, name: name.into(), x, y, z, updated_at: None
        }).unwrap();
    }

    #[test]
    fn exact_center_wins_and_radius_is_a_true_sphere() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_database(&conn).unwrap();
        add(&conn, 10, "Sol", 0.0, 0.0, 0.0);
        add(&conn, 11, "Sol A", 1.0, 1.0, 0.0);
        add(&conn, 12, "Sol B", 1.0, 0.0, 0.0);

        let result = search_radius(&conn, "Sol", 1.0).unwrap();
        assert_eq!(result.center.edsm_id, 10);
        assert_eq!(result.count, 2);
        assert_eq!(result.systems[0].system.edsm_id, 10);
        assert_eq!(result.systems[1].system.edsm_id, 12);
    }

    #[test]
    fn fallback_trims_trailing_components_and_picks_first_match() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_database(&conn).unwrap();
        add(&conn, 20, "Alpha Centauri AB 1", 0.0, 0.0, 0.0);
        add(&conn, 10, "Alpha Centauri AB 2", 1.0, 0.0, 0.0);

        let center = find_center(&conn, "Alpha Centauri AB 1 B").unwrap();
        assert_eq!(center.edsm_id, 10);
        assert_eq!(center.name, "Alpha Centauri AB 2");
    }

    #[test]
    fn invalid_radius_is_rejected() {
        let conn = Connection::open_in_memory().unwrap();
        initialize_database(&conn).unwrap();
        assert!(matches!(search_radius(&conn, "Sol", f64::NAN), Err(SearchError::InvalidRadius)));
    }
}
