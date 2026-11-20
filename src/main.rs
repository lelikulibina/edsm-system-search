use std::net::SocketAddr;

use clap::Parser;
use edsm_system_search::api::{AppState, router};
use edsm_system_search::service::SearchLimits;
use edsm_system_search::spansh::SpanshClient;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "edsm-system-search", version, about = "Search Elite Dangerous bodies using the Spansh API")]
struct Cli {
    #[arg(long, env = "EDSM_BIND", default_value = "0.0.0.0:8080")]
    bind: SocketAddr,
    #[arg(long, env = "EDSM_MAX_RADIUS", default_value_t = 500.0)]
    max_radius: f64,
    #[arg(long, env = "EDSM_PAGE_SIZE", default_value_t = 100)]
    page_size: u32,
    #[arg(long, env = "EDSM_MAX_PAGES", default_value_t = 20)]
    max_pages: u32,
    #[arg(long, env = "EDSM_MAX_RESULTS", default_value_t = 2_000)]
    max_results: usize,
    #[arg(long, env = "SPANSH_API_BASE_URL", default_value = "https://spansh.co.uk/api")]
    spansh_api_base_url: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let cli = Cli::parse();
    if !cli.max_radius.is_finite() || cli.max_radius <= 0.0 {
        return Err("EDSM_MAX_RADIUS must be a finite positive number".into());
    }
    if cli.page_size == 0 || cli.max_pages == 0 || cli.max_results == 0 {
        return Err("page size, max pages and max results must be positive".into());
    }

    let limits = SearchLimits {
        max_radius: cli.max_radius,
        page_size: cli.page_size.min(500),
        max_pages: cli.max_pages,
        max_results: cli.max_results,
    };
    let client = SpanshClient::with_base_url(cli.spansh_api_base_url)?;
    let listener = tokio::net::TcpListener::bind(cli.bind).await?;
    tracing::info!(bind = %cli.bind, max_radius = cli.max_radius, "HTTP server listening");
    axum::serve(listener, router(AppState::from_spansh(client, limits)))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
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
