use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tower_http::trace::TraceLayer;

use crate::service::{SearchLimits, SearchService, SearchServiceError};
use crate::spansh::{SpanshClient, SpanshError};

#[derive(Clone)]
pub struct AppState {
    service: Arc<SearchService>,
    slots: Arc<Semaphore>,
}

impl AppState {
    pub fn new(service: SearchService) -> Self {
        Self {
            service: Arc::new(service),
            slots: Arc::new(Semaphore::new(2)),
        }
    }

    pub fn from_spansh(client: SpanshClient, limits: SearchLimits) -> Self {
        Self::new(SearchService::new(client, limits))
    }
}

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub center: String,
    pub radius: f64,
}

#[derive(Debug, Deserialize)]
pub struct SearchRequest {
    pub center: String,
    pub radius: f64,
    #[serde(default = "empty_filters")]
    pub filters: Value,
}

fn empty_filters() -> Value {
    json!({})
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

impl From<SearchServiceError> for ApiError {
    fn from(error: SearchServiceError) -> Self {
        match error {
            SearchServiceError::InvalidCenter => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_center",
                message: error.to_string(),
            },
            SearchServiceError::InvalidRadius => Self {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_radius",
                message: error.to_string(),
            },
            SearchServiceError::Spansh(SpanshError::SystemNotFound(_)) => Self {
                status: StatusCode::NOT_FOUND,
                code: "center_not_found",
                message: error.to_string(),
            },
            SearchServiceError::Spansh(_) => Self {
                status: StatusCode::BAD_GATEWAY,
                code: "spansh_upstream_error",
                message: error.to_string(),
            },
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/search", get(search_get).post(search_post))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

async fn health() -> Json<Value> {
    Json(json!({"status":"ok","upstream":"spansh"}))
}

async fn search_get(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<crate::service::BodySearchResult>, ApiError> {
    execute_search(state, query.center, query.radius, empty_filters()).await
}

async fn search_post(
    State(state): State<AppState>,
    Json(request): Json<SearchRequest>,
) -> Result<Json<crate::service::BodySearchResult>, ApiError> {
    if !request.filters.is_object() {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_filters",
            message: "filters must be a JSON object".into(),
        });
    }
    execute_search(state, request.center, request.radius, request.filters).await
}

async fn execute_search(
    state: AppState,
    center: String,
    radius: f64,
    filters: Value,
) -> Result<Json<crate::service::BodySearchResult>, ApiError> {
    let _permit = state.slots.clone().acquire_owned().await.map_err(|error| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "search_capacity_exhausted",
        message: error.to_string(),
    })?;
    let result = state.service.search_bodies(&center, radius, filters).await.map_err(ApiError::from)?;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_does_not_depend_on_a_database() {
        let client = SpanshClient::with_base_url("http://127.0.0.1:9/api").unwrap();
        let app = router(AppState::from_spansh(client, SearchLimits::default()));
        let response = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder().uri("/health").body(axum::body::Body::empty()).unwrap(),
        ).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn get_search_query_deserializes_center_and_radius() {
        let query: SearchQuery = serde_urlencoded::from_str("center=Sol&radius=25").unwrap();
        assert_eq!(query.center, "Sol");
        assert_eq!(query.radius, 25.0);
    }
}
