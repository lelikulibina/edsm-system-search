use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use rusqlite::Connection;
use thiserror::Error;
use tracing::{info, warn};

use crate::db::{DbError, get_metadata, initialize_database, open_database, set_metadata};
use crate::importer::{ImportError, import_gzip_reader};

pub const FULL_DUMP_URL: &str = "https://www.edsm.net/dump/systemsWithCoordinates.json.gz";
pub const WEEKLY_DUMP_URL: &str =
    "https://www.edsm.net/dump/systemsWithCoordinates7days.json.gz";
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const IMPORT_BATCH_SIZE: usize = 1000;
const MAX_DOWNLOAD_ATTEMPTS: u32 = 5;
const INITIAL_RETRY_DELAY: Duration = Duration::from_secs(30);
const FULL_IMPORT_KEY: &str = "full_import_completed_at";
const INCREMENTAL_IMPORT_KEY: &str = "last_incremental_import_at";

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("database error: {0}")]
    Database(#[from] DbError),
    #[error("import error: {0}")]
    Import(#[from] ImportError),
    #[error("download error: {0}")]
    Download(#[from] reqwest::Error),
}

pub fn synchronize_on_startup(db_path: impl AsRef<Path>) -> Result<Duration, UpdateError> {
    let conn = open_database(db_path)?;
    initialize_database(&conn)?;

    if get_metadata(&conn, FULL_IMPORT_KEY)?.is_none() {
        info!("first startup: downloading the complete EDSM systems dump");
        let stats = import_from_url(&conn, FULL_DUMP_URL)?;
        let now = unix_timestamp();
        set_metadata(&conn, INCREMENTAL_IMPORT_KEY, &now.to_string())?;
        set_metadata(&conn, FULL_IMPORT_KEY, &now.to_string())?;
        info!(
            records_seen = stats.records_seen,
            imported = stats.imported,
            skipped = stats.skipped,
            "initial EDSM import completed"
        );
        return Ok(REFRESH_INTERVAL);
    }

    let last_update = get_metadata(&conn, INCREMENTAL_IMPORT_KEY)?
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let now = unix_timestamp();

    if is_refresh_due(last_update, now, REFRESH_INTERVAL) {
        info!("seven-day EDSM refresh is due");
        match import_from_url(&conn, WEEKLY_DUMP_URL) {
            Ok(stats) => {
                let completed_at = unix_timestamp();
                set_metadata(&conn, INCREMENTAL_IMPORT_KEY, &completed_at.to_string())?;
                info!(
                    records_seen = stats.records_seen,
                    imported = stats.imported,
                    skipped = stats.skipped,
                    "incremental EDSM import completed"
                );
                Ok(REFRESH_INTERVAL)
            }
            Err(error) => {
                warn!(%error, "incremental EDSM import failed; existing database will remain available");
                Ok(Duration::from_secs(6 * 60 * 60))
            }
        }
    } else {
        Ok(remaining_until_refresh(last_update, now, REFRESH_INTERVAL))
    }
}

fn import_from_url(
    conn: &Connection,
    url: &str,
) -> Result<crate::importer::ImportStats, UpdateError> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(12 * 60 * 60))
        .user_agent(concat!("edsm-system-search/", env!("CARGO_PKG_VERSION")))
        .build()?;

    let mut delay = INITIAL_RETRY_DELAY;
    for attempt in 1..=MAX_DOWNLOAD_ATTEMPTS {
        match download_and_import(&client, conn, url) {
            Ok(stats) => return Ok(stats),
            Err(error) if is_retryable(&error) && attempt < MAX_DOWNLOAD_ATTEMPTS => {
                warn!(
                    %error,
                    attempt,
                    max_attempts = MAX_DOWNLOAD_ATTEMPTS,
                    retry_in_seconds = delay.as_secs(),
                    "EDSM dump stream failed; restarting the download from the beginning"
                );
                thread::sleep(delay);
                delay = delay.saturating_mul(2).min(Duration::from_secs(10 * 60));
            }
            Err(error) => return Err(error),
        }
    }

    unreachable!("the download retry loop always returns")
}

fn download_and_import(
    client: &Client,
    conn: &Connection,
    url: &str,
) -> Result<crate::importer::ImportStats, UpdateError> {
    let response = client.get(url).send()?.error_for_status()?;
    Ok(import_gzip_reader(response, conn, IMPORT_BATCH_SIZE)?)
}

fn is_retryable(error: &UpdateError) -> bool {
    match error {
        UpdateError::Download(error) => {
            error.is_timeout()
                || error.is_connect()
                || error.is_body()
                || error.status().is_some_and(|status| {
                    status.as_u16() == 429 || status.is_server_error()
                })
        }
        UpdateError::Import(ImportError::Io(_)) => true,
        UpdateError::Import(ImportError::Json(error)) => error.is_io(),
        UpdateError::Database(_) | UpdateError::Import(_) => false,
    }
}

pub fn spawn_refresh_loop(db_path: PathBuf, initial_delay: Duration) {
    tokio::spawn(async move {
        let mut delay = initial_delay;
        loop {
            tokio::time::sleep(delay).await;
            let path = db_path.clone();
            match tokio::task::spawn_blocking(move || synchronize_on_startup(path)).await {
                Ok(Ok(next_delay)) => delay = next_delay,
                Ok(Err(error)) => {
                    warn!(%error, "automatic EDSM refresh failed; will retry in six hours");
                    delay = Duration::from_secs(6 * 60 * 60);
                }
                Err(error) => {
                    warn!(%error, "automatic EDSM refresh task failed; will retry in six hours");
                    delay = Duration::from_secs(6 * 60 * 60);
                }
            }
        }
    });
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn is_refresh_due(last_update: u64, now: u64, interval: Duration) -> bool {
    now.saturating_sub(last_update) >= interval.as_secs()
}

fn remaining_until_refresh(last_update: u64, now: u64, interval: Duration) -> Duration {
    Duration::from_secs(interval.as_secs().saturating_sub(now.saturating_sub(last_update)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_refresh_becomes_due_after_seven_days() {
        let interval = Duration::from_secs(7 * 24 * 60 * 60);
        assert!(!is_refresh_due(1_000, 1_000 + interval.as_secs() - 1, interval));
        assert!(is_refresh_due(1_000, 1_000 + interval.as_secs(), interval));
        assert!(is_refresh_due(1_000, 1_000 + interval.as_secs() + 1, interval));
    }

    #[test]
    fn clock_going_backwards_does_not_make_refresh_due() {
        assert!(!is_refresh_due(2_000, 1_000, REFRESH_INTERVAL));
        assert_eq!(
            remaining_until_refresh(2_000, 1_000, REFRESH_INTERVAL),
            REFRESH_INTERVAL
        );
    }
}
