//! JSON deserialization for the LDBWS REST API (20220120 version).
//!
//! API: GET https://api1.raildata.org.uk/.../GetDepBoardWithDetails/{crs}
//! Auth: `x-apikey` request header.

use crate::darwin::Service;
use anyhow::Result;
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Wire types (private — callers get Vec<Service>)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct StationBoard {
    #[serde(rename = "trainServices")]
    train_services: Option<Vec<ServiceItem>>,
}

#[derive(Deserialize)]
struct ServiceItem {
    #[serde(rename = "serviceID", default)]
    service_id: String,
    #[serde(default)]
    std: String,
    #[serde(default)]
    etd: String,
    platform: Option<String>,
    #[serde(rename = "isCancelled", default)]
    is_cancelled: bool,
    #[serde(rename = "cancelReason")]
    cancel_reason: Option<String>,
    #[serde(rename = "delayReason")]
    delay_reason: Option<String>,
    #[serde(default)]
    destination: Vec<ServiceLocation>,
    /// Subsequent calling points — one group per train leg (for split services).
    #[serde(rename = "subsequentCallingPoints", default)]
    subsequent_calling_points: Vec<CallingPointGroup>,
}

#[derive(Deserialize)]
struct ServiceLocation {
    #[serde(rename = "locationName", default)]
    location_name: String,
    #[serde(default)]
    crs: String,
}

#[derive(Deserialize, Default)]
struct CallingPointGroup {
    #[serde(rename = "callingPoint", default)]
    calling_points: Vec<CallingPoint>,
}

#[derive(Deserialize)]
struct CallingPoint {
    #[serde(default)]
    crs: String,
}

// ---------------------------------------------------------------------------
// Public parse entry point
// ---------------------------------------------------------------------------

pub fn parse_departure_board(json: &str) -> Result<Vec<Service>> {
    let board: StationBoard = serde_json::from_str(json)?;
    let services = board
        .train_services
        .unwrap_or_default()
        .into_iter()
        .filter_map(service_item_to_domain)
        .collect();
    Ok(services)
}

fn service_item_to_domain(item: ServiceItem) -> Option<Service> {
    if item.service_id.is_empty() {
        return None;
    }
    let (dest_name, dest_crs) = item
        .destination
        .into_iter()
        .next()
        .map(|d| (d.location_name, d.crs))
        .unwrap_or_default();

    // Collect CRS codes from all calling-point groups (handles split services).
    let calling_point_crs: Vec<String> = item
        .subsequent_calling_points
        .into_iter()
        .flat_map(|g| g.calling_points)
        .filter_map(|cp| {
            let crs = cp.crs.trim().to_uppercase();
            if crs.is_empty() { None } else { Some(crs) }
        })
        .collect();

    Some(Service {
        service_id: item.service_id,
        origin_crs: String::new(), // set by the DepartureSource impl after parsing
        std: item.std,
        etd: if item.etd.is_empty() {
            "Unknown".to_string()
        } else {
            item.etd
        },
        platform: item.platform,
        destination_crs: dest_crs,
        destination_name: dest_name,
        is_cancelled: item.is_cancelled,
        cancel_reason: item.cancel_reason,
        delay_reason: item.delay_reason,
        calling_point_crs,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json() -> &'static str {
        r#"{
            "generatedAt": "2024-01-15T12:30:00+00:00",
            "locationName": "London Waterloo",
            "crs": "WAT",
            "platformAvailable": true,
            "areServicesAvailable": true,
            "trainServices": [
                {
                    "serviceID": "ABC123",
                    "std": "12:34",
                    "etd": "On time",
                    "platform": "4",
                    "isCancelled": false,
                    "destination": [
                        {"locationName": "Reading", "crs": "RDG"}
                    ]
                },
                {
                    "serviceID": "DEF456",
                    "std": "12:50",
                    "etd": "13:05",
                    "platform": "7",
                    "isCancelled": false,
                    "delayReason": "signalling problems",
                    "destination": [
                        {"locationName": "Woking", "crs": "WOK"}
                    ]
                }
            ]
        }"#
    }

    #[test]
    fn parses_two_services() {
        let services = parse_departure_board(sample_json()).expect("parse");
        assert_eq!(services.len(), 2);

        let s = &services[0];
        assert_eq!(s.service_id, "ABC123");
        assert_eq!(s.std, "12:34");
        assert_eq!(s.etd, "On time");
        assert_eq!(s.platform, Some("4".to_string()));
        assert_eq!(s.destination_crs, "RDG");
        assert_eq!(s.destination_name, "Reading");
        assert!(!s.is_cancelled);
        assert!(s.delay_reason.is_none());

        let s2 = &services[1];
        assert_eq!(s2.service_id, "DEF456");
        assert_eq!(s2.etd, "13:05");
        assert_eq!(s2.delay_reason.as_deref(), Some("signalling problems"));
    }

    #[test]
    fn empty_train_services_returns_empty_vec() {
        let json = r#"{"locationName":"Waterloo","crs":"WAT"}"#;
        let services = parse_departure_board(json).expect("parse");
        assert!(services.is_empty());
    }

    #[test]
    fn null_train_services_returns_empty_vec() {
        let json = r#"{"locationName":"Waterloo","crs":"WAT","trainServices":null}"#;
        let services = parse_departure_board(json).expect("parse");
        assert!(services.is_empty());
    }

    #[test]
    fn cancelled_service_parses() {
        let json = r#"{
            "trainServices": [{
                "serviceID": "CAN001",
                "std": "14:00",
                "etd": "Cancelled",
                "isCancelled": true,
                "cancelReason": "a train fault",
                "destination": [{"locationName": "Brighton", "crs": "BTN"}]
            }]
        }"#;
        let services = parse_departure_board(json).expect("parse");
        assert_eq!(services.len(), 1);
        assert!(services[0].is_cancelled);
        assert_eq!(services[0].cancel_reason.as_deref(), Some("a train fault"));
    }

    #[test]
    fn service_without_id_is_skipped() {
        let json = r#"{
            "trainServices": [
                {"serviceID": "", "std": "10:00", "etd": "On time", "destination": []},
                {"serviceID": "OK001", "std": "10:05", "etd": "On time", "destination": [{"locationName": "York", "crs": "YRK"}]}
            ]
        }"#;
        let services = parse_departure_board(json).expect("parse");
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].service_id, "OK001");
    }

    #[test]
    fn missing_etd_becomes_unknown() {
        let json = r#"{
            "trainServices": [{
                "serviceID": "X1",
                "std": "09:00",
                "destination": [{"locationName": "Bristol", "crs": "BRI"}]
            }]
        }"#;
        let services = parse_departure_board(json).expect("parse");
        assert_eq!(services[0].etd, "Unknown");
    }
}
