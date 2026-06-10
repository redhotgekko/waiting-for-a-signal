mod rest;
pub mod simulate;

use crate::config::DarwinConfig;
use anyhow::Result;
use std::path::{Path, PathBuf};
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
    /// Non-None only when `[darwin.debug_capture] enabled = true`.
    debug_capture_dir: Option<PathBuf>,
}

impl DarwinClient {
    pub fn new(cfg: &DarwinConfig) -> Self {
        let debug_capture_dir = if cfg.debug_capture.enabled {
            let dir = cfg
                .debug_capture
                .dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("darwin-debug"));
            match std::fs::create_dir_all(&dir) {
                Ok(()) => {
                    debug!(dir = %dir.display(), "Darwin debug capture enabled");
                    Some(dir)
                }
                Err(e) => {
                    warn!(dir = %dir.display(), err = %e, "Failed to create Darwin debug capture directory — capture disabled");
                    None
                }
            }
        } else {
            None
        };

        Self {
            endpoint: cfg.endpoint.clone(),
            token: cfg.token.clone(),
            http: reqwest::Client::new(),
            debug_capture_dir,
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

        if let Some(dir) = &self.debug_capture_dir {
            save_debug_capture(dir, crs, num_rows, filter_crs, &resp);
        }

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

/// Writes `json` as a gzip-compressed file inside `dir`.
///
/// Filename encodes the UTC timestamp, origin CRS, row count, and optional
/// destination filter so each file is unique and self-describing.
/// Example: `2026-06-10T15-30-00.123456Z_WAT_rows149_filterRDG.json.gz`
///
/// Failures are logged as warnings; they never propagate to the caller.
fn save_debug_capture(dir: &Path, crs: &str, num_rows: u32, filter_crs: Option<&str>, json: &str) {
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    let now = chrono::Utc::now();
    let daily_dir = dir.join(now.format("%d%b%Y").to_string());
    if let Err(e) = std::fs::create_dir_all(&daily_dir) {
        warn!(path = %daily_dir.display(), err = %e, "Failed to create Darwin debug capture daily directory");
        return;
    }

    let ts = now.format("%Y-%m-%dT%H-%M-%S%.6fZ");
    let filter_part = filter_crs
        .map(|f| format!("_filter{f}"))
        .unwrap_or_default();
    let filename = format!("{ts}_{crs}_rows{num_rows}{filter_part}.json.gz");
    let path = daily_dir.join(&filename);

    let file = match std::fs::File::create(&path) {
        Ok(f) => f,
        Err(e) => {
            warn!(path = %path.display(), err = %e, "Failed to create Darwin debug capture file");
            return;
        }
    };

    let mut enc = GzEncoder::new(file, Compression::best());
    if let Err(e) = enc.write_all(json.as_bytes()) {
        warn!(path = %path.display(), err = %e, "Failed to write Darwin debug capture");
        return;
    }
    if let Err(e) = enc.finish() {
        warn!(path = %path.display(), err = %e, "Failed to finalise Darwin debug capture gzip stream");
        return;
    }

    debug!(path = %path.display(), raw_bytes = json.len(), "Darwin debug capture saved");
}
