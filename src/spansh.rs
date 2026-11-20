use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

const DEFAULT_BASE_URL: &str = "https://spansh.co.uk/api";
const MAX_PAGE_SIZE: u32 = 500;
const MAX_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Coordinates {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Coordinates {
    pub fn is_valid(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StarSystem {
    pub id64: u64,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl StarSystem {
    pub fn coordinates(&self) -> Coordinates {
        Coordinates { x: self.x, y: self.y, z: self.z }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Body {
    #[serde(default)]
    pub id64: Option<u64>,
    #[serde(default)]
    pub body_id: Option<u32>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub system_name: Option<String>,
    #[serde(default)]
    pub system_id64: Option<u64>,
    #[serde(default)]
    pub system_x: Option<f64>,
    #[serde(default)]
    pub system_y: Option<f64>,
    #[serde(default)]
    pub system_z: Option<f64>,
    #[serde(default)]
    pub distance: Option<f64>,
    #[serde(default)]
    pub distance_to_arrival: Option<f64>,
    #[serde(default)]
    pub is_landable: Option<bool>,
    #[serde(default)]
    pub gravity: Option<f64>,
    #[serde(default)]
    pub surface_temperature: Option<f64>,
    #[serde(default)]
    pub surface_pressure: Option<f64>,
    #[serde(default)]
    pub atmosphere: Option<String>,
    #[serde(default)]
    pub terraforming_state: Option<String>,
    #[serde(default)]
    pub volcanism_type: Option<String>,
    #[serde(default)]
    pub materials: Option<Value>,
    #[serde(default)]
    pub signals: Option<Value>,
    #[serde(default)]
    pub genuses: Option<Value>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchPage<T> {
    #[serde(default)]
    pub count: u64,
    #[serde(default)]
    pub from: u64,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub search_reference: Option<String>,
    #[serde(default)]
    pub results: Vec<T>,
}

impl<T> Default for SearchPage<T> {
    fn default() -> Self {
        Self { count: 0, from: 0, size: 0, search_reference: None, results: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BodySearchRequest {
    pub reference_coords: Coordinates,
    pub filters: Value,
    pub size: u32,
    pub page: u32,
}

impl BodySearchRequest {
    pub fn new(reference_coords: Coordinates, radius: f64, filters: Value, size: u32, page: u32) -> Self {
        let mut filters = filters.as_object().cloned().unwrap_or_default();
        filters.insert("distance".into(), json!({"min":"0","max":radius.to_string()}));
        Self {
            reference_coords,
            filters: Value::Object(filters),
            size: size.clamp(1, MAX_PAGE_SIZE),
            page,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "filters": self.filters,
            "sort": [{"distance": {"direction": "asc"}}],
            "size": self.size,
            "page": self.page,
            "reference_coords": self.reference_coords
        })
    }

    pub fn with_page(&self, page: u32) -> Self {
        let mut next = self.clone();
        next.page = page;
        next
    }
}

#[derive(Debug, Error)]
pub enum SpanshError {
    #[error("Spansh request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("Spansh returned HTTP {status}: {message}")]
    Http { status: StatusCode, message: String },
    #[error("central system was not found: {0}")]
    SystemNotFound(String),
    #[error("Spansh returned invalid data: {0}")]
    InvalidData(String),
}

#[derive(Clone)]
pub struct SpanshClient {
    http: Client,
    base_url: String,
}

impl SpanshClient {
    pub fn new() -> Result<Self, SpanshError> {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    pub fn with_base_url(base_url: impl Into<String>) -> Result<Self, SpanshError> {
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(45))
            .user_agent(concat!("edsm-system-search/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { http, base_url: base_url.into().trim_end_matches('/').to_owned() })
    }

    pub async fn resolve_system(&self, requested: &str) -> Result<StarSystem, SpanshError> {
        let mut candidate = requested.trim().to_owned();
        if candidate.is_empty() {
            return Err(SpanshError::SystemNotFound(requested.to_owned()));
        }

        loop {
            let response: SystemSearchResponse = self.get_json(
                &format!("{}/search/systems", self.base_url),
                &[("q", candidate.as_str())],
            ).await?;

            if let Some(system) = response.results.into_iter().find(|item| {
                item.name.eq_ignore_ascii_case(&candidate) && valid_system(item)
            }) {
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

        Err(SpanshError::SystemNotFound(requested.to_owned()))
    }

    pub async fn search_bodies(&self, request: &BodySearchRequest) -> Result<SearchPage<Body>, SpanshError> {
        let url = format!("{}/bodies/search", self.base_url);
        let mut delay = Duration::from_millis(400);

        for attempt in 0..MAX_ATTEMPTS {
            let result = self.http.post(&url).json(&request.to_json()).send().await;
            match result {
                Ok(response) if response.status().is_success() => {
                    return response.json::<SearchPage<Body>>().await.map_err(SpanshError::from);
                }
                Ok(response) => {
                    let status = response.status();
                    let message = response.text().await.unwrap_or_default();
                    if !retryable_status(status) || attempt + 1 == MAX_ATTEMPTS {
                        return Err(SpanshError::Http { status, message });
                    }
                }
                Err(error) if retryable_request(&error) && attempt + 1 < MAX_ATTEMPTS => {}
                Err(error) => return Err(SpanshError::Request(error)),
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(3));
        }

        unreachable!("retry loop returns on its final attempt")
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<T, SpanshError> {
        let response = self.http.get(url).query(query).send().await?;
        let status = response.status();
        if !status.is_success() {
            let message = response.text().await.unwrap_or_default();
            return Err(SpanshError::Http { status, message });
        }
        Ok(response.json().await?)
    }
}

fn valid_system(system: &StarSystem) -> bool {
    !system.name.trim().is_empty() && system.coordinates().is_valid()
}

fn retryable_status(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

fn retryable_request(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_body()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id64_above_javascript_safe_integer_is_preserved() {
        let json = r#"{"id64":2413933332548063962,"name":"Example","x":1.0,"y":2.0,"z":3.0}"#;
        let system: StarSystem = serde_json::from_str(json).unwrap();
        assert_eq!(system.id64, 2_413_933_332_548_063_962);
        assert_eq!(serde_json::to_string(&system).unwrap().contains("2413933332548063962"), true);
    }

    #[test]
    fn body_dto_accepts_sparse_records_and_unknown_fields() {
        let json = r#"{"id64":2413933332548063962,"system_id64":9007199254740993,"name":"A 1","future_field":"ignored"}"#;
        let body: Body = serde_json::from_str(json).unwrap();
        assert_eq!(body.id64, Some(2_413_933_332_548_063_962));
        assert_eq!(body.system_id64, Some(9_007_199_254_740_993));
        assert_eq!(body.gravity, None);
    }

    #[test]
    fn body_request_contains_coordinates_radius_and_zero_based_page() {
        let request = BodySearchRequest::new(
            Coordinates { x: 1.0, y: 2.0, z: 3.0 },
            50.0,
            json!({"is_landable":{"value":true}}),
            700,
            2,
        );
        let json = request.to_json();
        assert_eq!(json["reference_coords"], json!({"x":1.0,"y":2.0,"z":3.0}));
        assert_eq!(json["filters"]["distance"], json!({"min":"0","max":"50"}));
        assert_eq!(json["filters"]["is_landable"]["value"], true);
        assert_eq!(json["sort"][0]["distance"]["direction"], "asc");
        assert_eq!(json["page"], 2);
        assert_eq!(json["size"], 500);
    }

    #[test]
    fn page_copy_changes_only_page_number() {
        let request = BodySearchRequest::new(
            Coordinates { x: 0.0, y: 0.0, z: 0.0 }, 25.0, json!({}), 100, 0
        );
        let next = request.with_page(1);
        assert_eq!(next.page, 1);
        assert_eq!(next.filters, request.filters);
        assert_eq!(next.size, request.size);
    }
}
