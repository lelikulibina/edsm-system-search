use std::path::PathBuf;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task;
use tower_http::trace::TraceLayer;

use crate::db::open_database;
use crate::search::{SearchError, search_radius};

#[derive(Clone)]
pub struct AppState {
    pub database_path: PathBuf,
    pub max_radius: f64,
    db_slots: Arc<Semaphore>,
}

impl AppState {
    pub fn new(database_path: PathBuf, max_radius: f64) -> Self {
        Self {
            database_path,
            max_radius,
            db_slots: Arc::new(Semaphore::new(8)),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub center: String,
    pub radius: f64,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({
            "error": {
                "code": self.code,
                "message": self.message
            }
        }))).into_response()
    }
}

impl From<SearchError> for ApiError {
    fn from(error: SearchError) -> Self {
        match error {
            SearchError::CenterNotFound => Self {
                status: StatusCode::NOT_FOUND,
                code: "center_not_found",
                message: error.to_string(),
            },
            SearchError::InvalidRadius => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_request",
                message: error.to_string(),
            },
            other => Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "database_error",
                message: other.to_string(),
            },
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/search", get(search))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    let permit = state.db_slots.clone().acquire_owned().await.map_err(|e| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "database_unavailable",
        message: e.to_string(),
    })?;
    let path = state.database_path;
    task::spawn_blocking(move || -> Result<(), String> {
        let _permit = permit;
        let conn = open_database(path).map_err(|e| e.to_string())?;
        let _: i64 = conn.query_row("SELECT count(*) FROM systems", [], |row| row.get(0)).map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "database_error",
        message: e.to_string(),
    })?
    .map_err(|message| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "database_unavailable",
        message,
    })?;

    Ok(Json(HealthResponse { status: "ok" }))
}

async fn search(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<crate::search::SearchResponse>, ApiError> {
    if query.center.trim().is_empty()
        || !query.radius.is_finite()
        || query.radius <= 0.0
        || query.radius > state.max_radius
    {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_request",
            message: format!("center must be non-empty and radius must be in (0, {}]", state.max_radius),
        });
    }

    let permit = state.db_slots.clone().acquire_owned().await.map_err(|e| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "database_unavailable",
        message: e.to_string(),
    })?;
    let path = state.database_path;
    let center = query.center;
    let radius = query.radius;
    let result = task::spawn_blocking(move || {
        let _permit = permit;
        let conn: Connection = open_database(path)?;
        search_radius(&conn, &center, radius)
    })
    .await
    .map_err(|e| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "database_error",
        message: e.to_string(),
    })?
    .map_err(ApiError::from)?;

    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use rusqlite::Connection;
    use tempfile::tempdir;
    use tower::ServiceExt;

    use crate::db::{System, initialize_database, upsert_system};

    #[tokio::test]
    async fn search_endpoint_returns_json_results() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let conn = Connection::open(&db_path).unwrap();
        initialize_database(&conn).unwrap();
        upsert_system(&conn, &System {
            edsm_id: 1, id64: None, name: "Sol".into(),
            x: 0.0, y: 0.0, z: 0.0, updated_at: None,
        }).unwrap();
        drop(conn);

        let app = router(AppState::new(db_path, 10.0));
        let response = app.oneshot(
            axum::http::Request::builder()
                .uri("/search?center=Sol&radius=2")
                .body(Body::empty()).unwrap()
        ).await.unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["count"], 1);
        assert_eq!(value["center"]["name"], "Sol");
    }

    #[tokio::test]
    async fn search_endpoint_rejects_radius_above_limit() {
        let dir = tempdir().unwrap();
        let app = router(AppState::new(dir.path().join("missing.db"), 10.0));
        let response = app.oneshot(
            axum::http::Request::builder()
                .uri("/search?center=Sol&radius=11")
                .body(Body::empty()).unwrap()
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
