mod rest;
pub mod simulate;

use crate::config::DarwinConfig;
use anyhow::Result;
use thiserror::Error;
use tracing::{debug, warn};

#[derive(Debug, Error)]
pub enum DarwinError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Parse error: {0}")]
    Parse(String),
}

/// A single service on the departure board.
#[derive(Debug, Clone)]
pub struct Service {
    /// Darwin stable service ID.
    pub service_id: String,
    /// 3-letter CRS code of the station this service departs from.
    pub origin_crs: String,
    /// Scheduled departure time (HH:MM).
    pub std: String,
    /// Estimated departure time or status string ("On time", "Delayed", "Cancelled").
    pub etd: String,
    pub platform: Option<String>,
    pub destination_crs: String,
    pub destination_name: String,
    pub is_cancelled: bool,
    pub cancel_reason: Option<String>,
    pub delay_reason: Option<String>,
    /// CRS codes of subsequent calling points (intermediate stops after origin).
    /// Used to match subscriptions that filter by an intermediate station.
    pub calling_point_crs: Vec<String>,
}

pub struct DarwinClient {
    endpoint: String,
    token: String,
    http: reqwest::Client,
}

impl DarwinClient {
    pub fn new(cfg: &DarwinConfig) -> Self {
        Self {
            endpoint: cfg.endpoint.clone(),
            token: cfg.token.clone(),
            http: reqwest::Client::new(),
        }
    }

    pub async fn get_departure_board(
        &self,
        crs: &str,
        num_rows: u32,
        filter_crs: Option<&str>,
    ) -> Result<Vec<Service>, DarwinError> {
        let filter_param = filter_crs
            .map(|f| format!("&filterCrs={f}"))
            .unwrap_or_default();
        let url = format!(
            "{}/GetDepBoardWithDetails/{}?numRows={}{}",
            self.endpoint, crs, num_rows, filter_param
        );

        debug!(
            crs,
            num_rows, filter_crs, "Requesting LDBWS departure board"
        );

        let resp = self
            .http
            .get(&url)
            .header("x-apikey", &self.token)
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;

        let crs_upper = crs.to_uppercase();
        let mut services = rest::parse_departure_board(&resp).map_err(|e| {
            warn!(crs, err = %e, "Failed to parse LDBWS response");
            DarwinError::Parse(e.to_string())
        })?;
        for svc in &mut services {
            svc.origin_crs = crs_upper.clone();
        }
        Ok(services)
    }
}

/// Thin abstraction for testing: anything that can fetch departure boards.
#[async_trait::async_trait]
pub trait DepartureSource: Send + Sync {
    async fn get_departure_board(
        &self,
        crs: &str,
        num_rows: u32,
        filter_crs: Option<&str>,
    ) -> Result<Vec<Service>, DarwinError>;
}

#[async_trait::async_trait]
impl DepartureSource for DarwinClient {
    async fn get_departure_board(
        &self,
        crs: &str,
        num_rows: u32,
        filter_crs: Option<&str>,
    ) -> Result<Vec<Service>, DarwinError> {
        self.get_departure_board(crs, num_rows, filter_crs).await
    }
}
