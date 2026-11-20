use std::collections::HashSet;

use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use crate::spansh::{Body, BodySearchRequest, SpanshClient, SpanshError, StarSystem};

#[derive(Debug, Clone)]
pub struct SearchLimits {
    pub max_radius: f64,
    pub page_size: u32,
    pub max_pages: u32,
    pub max_results: usize,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self { max_radius: 500.0, page_size: 100, max_pages: 20, max_results: 2_000 }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct NearbyBody {
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Clone, Serialize)]
pub struct BodySearchResult {
    pub center: StarSystem,
    pub radius: f64,
    pub count: u64,
    pub returned: usize,
    pub truncated: bool,
    pub count_capped: bool,
    pub bodies: Vec<NearbyBody>,
}

#[derive(Debug, Error)]
pub enum SearchServiceError {
    #[error("invalid search radius")]
    InvalidRadius,
    #[error("invalid center system name")]
    InvalidCenter,
    #[error(transparent)]
    Spansh(#[from] SpanshError),
}

#[derive(Clone)]
pub struct SearchService {
    client: SpanshClient,
    limits: SearchLimits,
}

impl SearchService {
    pub fn new(client: SpanshClient, limits: SearchLimits) -> Self {
        Self { client, limits }
    }

    pub fn limits(&self) -> &SearchLimits {
        &self.limits
    }

    pub async fn search_bodies(
        &self,
        center_name: &str,
        radius: f64,
        filters: Value,
    ) -> Result<BodySearchResult, SearchServiceError> {
        if center_name.trim().is_empty() {
            return Err(SearchServiceError::InvalidCenter);
        }
        if !radius.is_finite() || radius <= 0.0 || radius > self.limits.max_radius {
            return Err(SearchServiceError::InvalidRadius);
        }

        let center = self.client.resolve_system(center_name).await?;
        let coords = center.coordinates();
        if !coords.is_valid() {
            return Err(SearchServiceError::Spansh(SpanshError::InvalidData(
                "central system has invalid coordinates".into(),
            )));
        }

        let first_request = BodySearchRequest::new(
            coords,
            radius,
            filters,
            self.limits.page_size,
            0,
        );
        let mut page = 0_u32;
        let mut upstream_count = 0_u64;
        let mut count_capped = false;
        let mut truncated = false;
        let mut bodies = Vec::new();
        let mut seen = HashSet::new();

        loop {
            let response = self.client.search_bodies(&first_request.with_page(page)).await?;
            if page == 0 {
                upstream_count = response.count;
                count_capped = response.count >= 10_000;
            }

            let page_len = response.results.len();
            let page_from = response.from;
            let page_count = response.count;
            for body in response.results {
                if bodies.len() >= self.limits.max_results {
                    truncated = true;
                    break;
                }
                if seen.insert(body_identity(&body)) {
                    bodies.push(NearbyBody { body });
                }
            }

            if truncated {
                break;
            }

            let next_offset = page_from.saturating_add(page_len as u64);
            let has_more = next_offset < page_count;
            if !has_more {
                break;
            }
            if page + 1 >= self.limits.max_pages {
                truncated = true;
                break;
            }
            if page_len == 0 {
                break;
            }
            page += 1;
        }

        bodies.sort_by(|a, b| {
            a.body.distance.unwrap_or(f64::INFINITY)
                .total_cmp(&b.body.distance.unwrap_or(f64::INFINITY))
                .then_with(|| a.body.system_name.cmp(&b.body.system_name))
                .then_with(|| a.body.system_id64.cmp(&b.body.system_id64))
                .then_with(|| a.body.body_id.cmp(&b.body.body_id))
        });

        let returned = bodies.len();
        Ok(BodySearchResult {
            center,
            radius,
            count: upstream_count,
            returned,
            truncated,
            count_capped,
            bodies,
        })
    }
}

fn body_identity(body: &Body) -> (Option<u64>, Option<u32>, Option<u64>, Option<String>) {
    if body.id64.is_some() || body.body_id.is_some() {
        (body.system_id64, body.body_id, body.id64, None)
    } else {
        (body.system_id64, None, None, body.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_identity_is_scoped_to_parent_system() {
        let first: Body = serde_json::from_value(serde_json::json!({
            "system_id64": 100, "body_id": 1, "name": "Alpha 1"
        })).unwrap();
        let duplicate = first.clone();
        let other_system: Body = serde_json::from_value(serde_json::json!({
            "system_id64": 200, "body_id": 1, "name": "Beta 1"
        })).unwrap();

        assert_eq!(body_identity(&first), body_identity(&duplicate));
        assert_ne!(body_identity(&first), body_identity(&other_system));
    }

    #[test]
    fn fallback_identity_uses_name_when_numeric_ids_are_missing() {
        let a: Body = serde_json::from_value(serde_json::json!({
            "system_id64": 100, "name": "Alpha 1"
        })).unwrap();
        let b = a.clone();
        assert_eq!(body_identity(&a), body_identity(&b));
    }

    #[test]
    fn default_limits_bound_upstream_work_and_response_size() {
        let limits = SearchLimits::default();
        assert!(limits.page_size <= 500);
        assert!(limits.max_pages > 0);
        assert!(limits.max_results > 0);
        assert!(limits.max_radius.is_finite());
    }
}
