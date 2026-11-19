use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use edsm_system_search::api::{AppState, router};
use edsm_system_search::db::{initialize_database, open_database};
use edsm_system_search::importer::import_gzip_file;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "edsm-system-search", version, about = "Search EDSM star systems by name and 3D radius")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create the SQLite schema and spatial index.
    InitDb {
        #[arg(long, env = "EDSM_DB", default_value = "data/systems.db")]
        db: PathBuf,
    },
    /// Import a gzip-compressed EDSM JSON dump.
    Import {
        #[arg(long, env = "EDSM_DB", default_value = "data/systems.db")]
        db: PathBuf,
        #[arg(long)]
        file: PathBuf,
        #[arg(long, default_value_t = 1000)]
        batch_size: usize,
    },
    /// Start the HTTP API.
    Serve {
        #[arg(long, env = "EDSM_DB", default_value = "data/systems.db")]
        db: PathBuf,
        #[arg(long, env = "EDSM_BIND", default_value = "0.0.0.0:8080")]
        bind: SocketAddr,
        #[arg(long, env = "EDSM_MAX_RADIUS", default_value_t = 10_000.0)]
        max_radius: f64,
    },
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    match Cli::parse().command {
        Command::InitDb { db } => {
            let conn = open_database(db)?;
            initialize_database(&conn)?;
            tracing::info!("database schema initialized");
        }
        Command::Import { db, file, batch_size } => {
            let conn = open_database(db)?;
            initialize_database(&conn)?;
            let stats = import_gzip_file(file, &conn, batch_size)?;
            println!(
                "records_seen={} imported={} skipped={}",
                stats.records_seen, stats.imported, stats.skipped
            );
        }
        Command::Serve { db, bind, max_radius } => {
            if !max_radius.is_finite() || max_radius <= 0.0 {
                return Err("EDSM_MAX_RADIUS must be a finite positive number".into());
            }
            let conn = open_database(&db)?;
            initialize_database(&conn)?;
            drop(conn);

            let listener = tokio::net::TcpListener::bind(bind).await?;
            tracing::info!(%bind, %max_radius, "HTTP server listening");
            axum::serve(listener, router(AppState {
                database_path: db,
                max_radius,
            }))
            .with_graceful_shutdown(shutdown_signal())
            .await?;
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut signal) = signal(SignalKind::terminate()) {
            signal.recv().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
