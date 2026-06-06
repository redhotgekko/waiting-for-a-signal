//! Simulated Darwin departure source for local testing.
//!
//! Returns plausible-looking UK train departures for any CRS code and
//! applies mutations on roughly every third poll so the poller generates
//! real Telegram notifications. No Darwin account or internet connection
//! is required.
//!
//! Enable with `simulate = true` in the `[darwin]` config section.

use crate::darwin::{DarwinError, DepartureSource, Service};
use async_trait::async_trait;
use chrono::Timelike;
use std::collections::HashMap;
use std::sync::Mutex;
use tracing::debug;

// ---------------------------------------------------------------------------
// Static data pools
// ---------------------------------------------------------------------------

const DESTINATIONS: &[(&str, &str)] = &[
    ("Reading", "RDG"),
    ("Surbiton", "SUR"),
    ("Wimbledon", "WIM"),
    ("Woking", "WOK"),
    ("Guildford", "GLD"),
    ("Basingstoke", "BSK"),
    ("Southampton Central", "SOU"),
    ("Portsmouth Harbour", "PMH"),
    ("Windsor & Eton Riverside", "WNR"),
    ("Clapham Junction", "CLJ"),
    ("London Waterloo", "WAT"),
    ("London Paddington", "PAD"),
    ("London Victoria", "VIC"),
    ("London Bridge", "LBG"),
    ("London Kings Cross", "KGX"),
    ("Cambridge", "CBG"),
    ("Brighton", "BTN"),
    ("York", "YRK"),
    ("Manchester Piccadilly", "MAN"),
    ("Edinburgh", "EDB"),
];

const PLATFORMS: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"];

const DELAY_REASONS: &[&str] = &[
    "signalling problems near Clapham Junction",
    "a train fault",
    "staff shortage",
    "earlier congestion on the line",
    "awaiting a late-running connection",
    "overhead line damage",
    "a person on the tracks",
];

const CANCEL_REASONS: &[&str] = &[
    "a train fault",
    "a member of staff being unavailable",
    "operational requirements",
    "an earlier incident causing knock-on delays",
];

// ---------------------------------------------------------------------------
// Deterministic pseudo-randomness (splitmix64) — no external dependency
// ---------------------------------------------------------------------------

fn rng(seed: u64) -> u64 {
    let x = seed.wrapping_add(0x9e3779b97f4a7c15);
    let x = (x ^ (x >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    let x = (x ^ (x >> 27)).wrapping_mul(0x94d049bb133111eb);
    x ^ (x >> 31)
}

fn pick_idx(pool_len: usize, seed: u64) -> usize {
    (rng(seed) as usize) % pool_len
}

// ---------------------------------------------------------------------------
// Per-service mutable state
// ---------------------------------------------------------------------------

struct SimService {
    service_id: String,
    std_h: u32,
    std_m: u32,
    /// Positive = delayed by this many minutes.
    etd_offset_min: u32,
    platform: String,
    destination_crs: String,
    destination_name: String,
    is_cancelled: bool,
    cancel_reason: Option<String>,
    delay_reason: Option<String>,
}

impl SimService {
    fn to_service(&self) -> Service {
        let std = format!("{:02}:{:02}", self.std_h, self.std_m);
        let etd = if self.is_cancelled {
            "Cancelled".to_string()
        } else if self.etd_offset_min == 0 {
            "On time".to_string()
        } else {
            let total = self.std_h * 60 + self.std_m + self.etd_offset_min;
            format!("{:02}:{:02}", (total / 60) % 24, total % 60)
        };
        Service {
            service_id: self.service_id.clone(),
            origin_crs: String::new(), // set by get_departure_board
            std,
            etd,
            platform: Some(self.platform.clone()),
            destination_crs: self.destination_crs.clone(),
            destination_name: self.destination_name.clone(),
            is_cancelled: self.is_cancelled,
            cancel_reason: self.cancel_reason.clone(),
            delay_reason: self.delay_reason.clone(),
            calling_point_crs: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Per-station state
// ---------------------------------------------------------------------------

struct StationState {
    services: Vec<SimService>,
    call_count: u64,
    /// Stable hash of the CRS code, used as a seed for all per-station decisions.
    crs_seed: u64,
}

impl StationState {
    fn new(crs: &str, num_rows: usize) -> Self {
        let crs_seed = crs_hash(crs);
        let services = generate_services(crs, num_rows, crs_seed);
        Self {
            services,
            call_count: 0,
            crs_seed,
        }
    }

    /// Optionally apply one mutation, then return the current service list.
    fn tick(&mut self) -> Vec<Service> {
        self.call_count += 1;

        // Apply a mutation on roughly every 3rd call.
        if self.call_count.is_multiple_of(3) && !self.services.is_empty() {
            let phase = self.call_count / 3;
            let svc_idx = pick_idx(self.services.len(), rng(self.crs_seed ^ phase));
            let svc = &mut self.services[svc_idx];

            // Cycle through mutation types.
            match phase % 5 {
                0 => {
                    // Delay
                    if !svc.is_cancelled {
                        let delay = (pick_idx(4, rng(self.crs_seed ^ phase ^ 1)) as u32 + 1) * 5;
                        svc.etd_offset_min = delay;
                        svc.delay_reason = Some(
                            DELAY_REASONS
                                [pick_idx(DELAY_REASONS.len(), rng(self.crs_seed ^ phase ^ 2))]
                            .to_string(),
                        );
                        debug!(
                            service_id = %svc.service_id,
                            delay_min = delay,
                            "Simulator: delayed service"
                        );
                    }
                }
                1 => {
                    // Platform change: pick a different platform by offsetting from the current one.
                    if !svc.is_cancelled {
                        let current_idx = PLATFORMS
                            .iter()
                            .position(|p| *p == svc.platform.as_str())
                            .unwrap_or(0);
                        // Offset by 1..len-1 so the result is always different.
                        let offset =
                            1 + pick_idx(PLATFORMS.len() - 1, rng(self.crs_seed ^ phase ^ 3));
                        svc.platform =
                            PLATFORMS[(current_idx + offset) % PLATFORMS.len()].to_string();
                        debug!(
                            service_id = %svc.service_id,
                            new_platform = %svc.platform,
                            "Simulator: platform changed"
                        );
                    }
                }
                2 => {
                    // Cancellation
                    if !svc.is_cancelled {
                        svc.is_cancelled = true;
                        svc.etd_offset_min = 0;
                        svc.delay_reason = None;
                        svc.cancel_reason = Some(
                            CANCEL_REASONS
                                [pick_idx(CANCEL_REASONS.len(), rng(self.crs_seed ^ phase ^ 4))]
                            .to_string(),
                        );
                        debug!(service_id = %svc.service_id, "Simulator: cancelled service");
                    }
                }
                3 => {
                    // Delay reason update (minor change to existing delay)
                    if svc.etd_offset_min > 0 {
                        let extra = (pick_idx(2, rng(self.crs_seed ^ phase ^ 5)) as u32 + 1) * 5;
                        svc.etd_offset_min += extra;
                        debug!(
                            service_id = %svc.service_id,
                            total_delay = svc.etd_offset_min,
                            "Simulator: delay extended"
                        );
                    }
                }
                _ => {
                    // Recovery: restore service to normal
                    svc.is_cancelled = false;
                    svc.etd_offset_min = 0;
                    svc.delay_reason = None;
                    svc.cancel_reason = None;
                    debug!(service_id = %svc.service_id, "Simulator: service recovered");
                }
            }
        }

        self.services.iter().map(SimService::to_service).collect()
    }
}

// ---------------------------------------------------------------------------
// Service generation
// ---------------------------------------------------------------------------

/// Generate `count` plausible fake services starting from ~now.
fn generate_services(crs: &str, count: usize, seed: u64) -> Vec<SimService> {
    let now = chrono::Local::now();
    let mut h = now.hour();
    let mut m = now.minute().saturating_add(2); // first train 2 min from now
    if m >= 60 {
        h = (h + 1) % 24;
        m -= 60;
    }

    // Build a list of destinations that excludes the origin itself.
    let filtered: Vec<(&str, &str)> = DESTINATIONS
        .iter()
        .copied()
        .filter(|(_, dest_crs)| !dest_crs.eq_ignore_ascii_case(crs))
        .collect();

    (0..count)
        .map(|i| {
            let dest_idx = pick_idx(filtered.len(), rng(seed ^ (i as u64 + 1)));
            let (dest_name, dest_crs) = filtered[dest_idx];
            let plat = PLATFORMS[pick_idx(PLATFORMS.len(), rng(seed ^ (i as u64 + 100)))];

            // Space trains ~8-12 minutes apart.
            let gap: u32 = (pick_idx(5, rng(seed ^ (i as u64 + 200))) as u32) + 8;
            let total_min = h * 60 + m + (i as u32) * gap;

            // Stable service ID: hash of CRS + position, formatted as 8 hex chars.
            let sid = format!("{:08X}", rng(seed ^ (i as u64 + 300)) as u32);

            SimService {
                service_id: sid,
                std_h: (total_min / 60) % 24,
                std_m: total_min % 60,
                etd_offset_min: 0,
                platform: plat.to_string(),
                destination_crs: dest_crs.to_string(),
                destination_name: dest_name.to_string(),
                is_cancelled: false,
                cancel_reason: None,
                delay_reason: None,
            }
        })
        .collect()
}

fn crs_hash(crs: &str) -> u64 {
    crs.bytes().fold(0xcbf29ce484222325u64, |acc, b| {
        acc.wrapping_mul(0x100000001b3).wrapping_add(u64::from(b))
    })
}

// ---------------------------------------------------------------------------
// Public struct
// ---------------------------------------------------------------------------

/// Simulates the Darwin departure board for local testing.
pub struct SimulatedDepartureSource {
    stations: Mutex<HashMap<String, StationState>>,
}

impl SimulatedDepartureSource {
    pub fn new() -> Self {
        Self {
            stations: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for SimulatedDepartureSource {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DepartureSource for SimulatedDepartureSource {
    async fn get_departure_board(
        &self,
        crs: &str,
        num_rows: u32,
        filter_crs: Option<&str>,
    ) -> Result<Vec<Service>, DarwinError> {
        let crs = crs.to_uppercase();
        let services = {
            let mut map = self
                .stations
                .lock()
                .map_err(|e| DarwinError::Parse(e.to_string()))?;
            let state = map
                .entry(crs.clone())
                .or_insert_with(|| StationState::new(&crs, num_rows as usize));
            state.tick()
        };
        let mut services: Vec<Service> = match filter_crs {
            Some(f) => services
                .into_iter()
                .filter(|s| s.destination_crs.eq_ignore_ascii_case(f))
                .collect(),
            None => services,
        };
        for svc in &mut services {
            svc.origin_crs = crs.clone();
        }
        debug!(
            crs = %crs,
            count = services.len(),
            filter_crs,
            "Simulator returned services"
        );
        Ok(services)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_services_for_any_crs() {
        let sim = SimulatedDepartureSource::new();
        let services = sim.get_departure_board("WAT", 5, None).await.expect("ok");
        assert_eq!(services.len(), 5);
        for svc in &services {
            assert!(!svc.service_id.is_empty());
            assert!(!svc.std.is_empty());
            assert!(!svc.destination_crs.is_empty());
        }
    }

    #[tokio::test]
    async fn destination_never_equals_origin() {
        let sim = SimulatedDepartureSource::new();
        let services = sim.get_departure_board("RDG", 10, None).await.expect("ok");
        for svc in &services {
            assert_ne!(
                svc.destination_crs, "RDG",
                "destination should not equal origin"
            );
        }
    }

    #[tokio::test]
    async fn mutation_changes_state_over_time() {
        let sim = SimulatedDepartureSource::new();
        let mut snapshots = Vec::new();
        for _ in 0..15 {
            let svcs = sim.get_departure_board("CLJ", 6, None).await.expect("ok");
            let etds: Vec<String> = svcs.iter().map(|s| s.etd.clone()).collect();
            snapshots.push(etds);
        }
        let changed = snapshots.windows(2).any(|w| w[0] != w[1]);
        assert!(
            changed,
            "simulator should introduce changes over multiple calls"
        );
    }

    #[tokio::test]
    async fn consistent_service_ids_across_calls() {
        let sim = SimulatedDepartureSource::new();
        let first = sim.get_departure_board("KGX", 5, None).await.expect("ok");
        let second = sim.get_departure_board("KGX", 5, None).await.expect("ok");
        let ids_first: Vec<&str> = first.iter().map(|s| s.service_id.as_str()).collect();
        let ids_second: Vec<&str> = second.iter().map(|s| s.service_id.as_str()).collect();
        assert_eq!(
            ids_first, ids_second,
            "service IDs should be stable across calls"
        );
    }

    #[tokio::test]
    async fn filter_crs_returns_only_matching_destinations() {
        let sim = SimulatedDepartureSource::new();
        // Generate enough rows that at least some will match the filter.
        let services = sim
            .get_departure_board("WAT", 20, Some("RDG"))
            .await
            .expect("ok");
        for svc in &services {
            assert_eq!(
                svc.destination_crs.to_uppercase(),
                "RDG",
                "filter_crs should exclude non-matching destinations"
            );
        }
    }
}
