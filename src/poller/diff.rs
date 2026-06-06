use crate::darwin::Service;
use crate::domain::Subscription;
use std::collections::{HashMap, HashSet};

fn col_width(lengths: impl Iterator<Item = usize>) -> usize {
    let mut lens: Vec<usize> = lengths.collect();
    if lens.is_empty() {
        return 0;
    }
    lens.sort_unstable();
    let median = lens[lens.len() / 2];
    let threshold = median.saturating_mul(2).saturating_add(10);
    lens.iter()
        .copied()
        .filter(|&v| v <= threshold)
        .max()
        .unwrap_or(0)
}

/// Maximum services shown per origin station in a digest.
/// The poller truncates each user's view to this many rows (sorted by STD)
/// before diffing, so changes to services outside this window don't fire
/// spurious notifications.
pub const DISPLAY_ROWS: usize = 10;

/// A snapshot of a single service at a point in time.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceSnapshot {
    pub service_id: String,
    pub origin_crs: String,
    pub std: String,
    pub etd: String,
    pub platform: Option<String>,
    pub destination_crs: String,
    pub destination_name: String,
    pub is_cancelled: bool,
    pub cancel_reason: Option<String>,
    pub delay_reason: Option<String>,
    pub calling_point_crs: Vec<String>,
}

impl From<&Service> for ServiceSnapshot {
    fn from(s: &Service) -> Self {
        Self {
            service_id: s.service_id.clone(),
            origin_crs: s.origin_crs.clone(),
            std: s.std.clone(),
            etd: s.etd.clone(),
            platform: s.platform.clone(),
            destination_crs: s.destination_crs.clone(),
            destination_name: s.destination_name.clone(),
            is_cancelled: s.is_cancelled,
            cancel_reason: s.cancel_reason.clone(),
            delay_reason: s.delay_reason.clone(),
            calling_point_crs: s.calling_point_crs.clone(),
        }
    }
}

/// The kind of change detected on a single service.
#[derive(Debug, Clone)]
pub enum ChangeKind {
    NewService,
    Cancelled,
    DelayChanged,
    PlatformChanged,
    StdChanged,
    ReasonChanged,
}

#[derive(Debug, Clone)]
pub struct ServiceChange {
    pub snapshot: ServiceSnapshot,
    #[allow(dead_code)]
    pub kind: ChangeKind,
}

/// Compute changes between the old snapshot map and the new one.
///
/// Both maps are keyed by service ID.
pub fn diff(
    old: Option<&HashMap<String, ServiceSnapshot>>,
    new: &HashMap<String, ServiceSnapshot>,
) -> Vec<ServiceChange> {
    let mut changes = Vec::new();

    for (id, new_snap) in new {
        match old.and_then(|o| o.get(id)) {
            None => {
                changes.push(ServiceChange {
                    snapshot: new_snap.clone(),
                    kind: ChangeKind::NewService,
                });
            }
            Some(old_snap) => {
                if old_snap.std != new_snap.std {
                    changes.push(ServiceChange {
                        snapshot: new_snap.clone(),
                        kind: ChangeKind::StdChanged,
                    });
                }
                if old_snap.etd != new_snap.etd {
                    if new_snap.is_cancelled && !old_snap.is_cancelled {
                        changes.push(ServiceChange {
                            snapshot: new_snap.clone(),
                            kind: ChangeKind::Cancelled,
                        });
                    } else {
                        changes.push(ServiceChange {
                            snapshot: new_snap.clone(),
                            kind: ChangeKind::DelayChanged,
                        });
                    }
                }
                if old_snap.platform != new_snap.platform {
                    changes.push(ServiceChange {
                        snapshot: new_snap.clone(),
                        kind: ChangeKind::PlatformChanged,
                    });
                }
                let old_reason = old_snap
                    .cancel_reason
                    .as_ref()
                    .or(old_snap.delay_reason.as_ref());
                let new_reason = new_snap
                    .cancel_reason
                    .as_ref()
                    .or(new_snap.delay_reason.as_ref());
                if old_reason != new_reason {
                    changes.push(ServiceChange {
                        snapshot: new_snap.clone(),
                        kind: ChangeKind::ReasonChanged,
                    });
                }
            }
        }
    }

    changes
}

/// Build a single consolidated update message for one user.
///
/// `snapshots` is the user's current view: service_id → snapshot, already
/// filtered to only include services the user is interested in.
///
/// `changed_ids` is the set of service IDs that changed this poll cycle.
/// Changed rows are prefixed with `*`; delayed/cancelled rows show the reason.
///
/// The output format is driven by `channel`: Telegram receives an HTML message
/// with `<pre>`-wrapped, column-aligned rows; Twilio/plain channels receive
/// compact plain text with single-space-separated fields and no markup.
///
/// `station_names` maps CRS codes to display names (e.g. "WAT" → "Waterloo").
/// If a code is not present the CRS is shown in its place.
///
/// Returns `None` if the snapshot is empty.
pub fn format_digest(
    subscriptions: &[Subscription],
    snapshots: &HashMap<String, ServiceSnapshot>,
    changed_ids: &HashSet<String>,
    now_str: &str,
    station_names: &HashMap<String, String>,
    channel: &crate::domain::Channel,
) -> Option<String> {
    // Determine display order of origin stations from subscription order (first-seen).
    let mut origin_order: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for sub in subscriptions {
        if seen.insert(sub.origin_crs.clone()) {
            origin_order.push(sub.origin_crs.clone());
        }
    }

    struct SectionData {
        crs: String,
        name: String,
        rows: Vec<String>,
    }

    let mut section_data: Vec<SectionData> = Vec::new();

    for origin_crs in &origin_order {
        // Collect services for this origin from the already-filtered snapshot.
        let mut services: Vec<&ServiceSnapshot> = snapshots
            .values()
            .filter(|s| &s.origin_crs == origin_crs)
            .collect();

        if services.is_empty() {
            continue;
        }

        // Sort by scheduled departure time (lexicographic "HH:MM" sort is correct here).
        services.sort_by(|a, b| a.std.cmp(&b.std));

        struct RawRow {
            prefix: &'static str, // "* " for changed, "" for unchanged
            time: String,
            dest: String,
            status: String,
            plat: String,
        }

        // First pass: collect raw field values so column widths can be measured.
        let raw: Vec<RawRow> = services
            .iter()
            .take(DISPLAY_ROWS)
            .map(|s| {
                let is_changed = changed_ids.contains(&s.service_id);
                let prefix = if is_changed { "* " } else { "  " };
                let status = if s.is_cancelled {
                    format!(
                        "Cancelled \u{2014} {}",
                        s.cancel_reason.as_deref().unwrap_or("reason unknown")
                    )
                } else if s.etd == "On time" || s.etd == s.std {
                    "On time".to_string()
                } else {
                    format!("exp {}", &s.etd)
                };
                let dest = {
                    let stop_count = subscriptions
                        .iter()
                        .filter(|sub| {
                            sub.origin_crs == s.origin_crs && !sub.destination_filter.is_empty()
                        })
                        .find_map(|sub| {
                            sub.destination_filter.iter().find_map(|dest_crs| {
                                s.calling_point_crs.iter().position(|crs| crs == dest_crs)
                            })
                        });
                    match stop_count {
                        Some(n) => format!("{} {}", s.destination_name, n + 1),
                        None => s.destination_name.clone(),
                    }
                };
                RawRow {
                    prefix,
                    time: s.std.clone(),
                    dest,
                    status,
                    plat: s.platform.as_deref().unwrap_or("TBC").to_string(),
                }
            })
            .collect();

        let rows: Vec<String> = match channel {
            crate::domain::Channel::Telegram => {
                let dest_w = col_width(raw.iter().map(|r| r.dest.len()));
                let status_w = col_width(raw.iter().map(|r| r.status.len()));
                raw.iter()
                    .map(|r| {
                        // Pad before escaping so spaces are not HTML-encoded.
                        let dest_padded = format!("{:<width$}", r.dest, width = dest_w);
                        let status_padded = format!("{:<width$}", r.status, width = status_w);
                        format!(
                            "{}{} {} {} Pl {}",
                            r.prefix,
                            esc(&r.time),
                            esc(&dest_padded),
                            esc(&status_padded),
                            esc(&r.plat),
                        )
                    })
                    .collect()
            }
            crate::domain::Channel::Twilio => {
                // Compact plain-text rows: no column padding, no HTML markup.
                raw.iter()
                    .map(|r| {
                        let plat = if r.plat == "TBC" {
                            "--".to_string()
                        } else {
                            format!("P{}", r.plat)
                        };
                        format!("{}{} {} {} {}", r.prefix, r.time, r.dest, r.status, plat)
                    })
                    .collect()
            }
        };

        let name = station_names
            .get(origin_crs.as_str())
            .cloned()
            .unwrap_or_else(|| origin_crs.clone());
        section_data.push(SectionData {
            crs: origin_crs.clone(),
            name,
            rows,
        });
    }

    if section_data.is_empty() {
        return None;
    }

    let text = match channel {
        crate::domain::Channel::Telegram => {
            if section_data.len() == 1 {
                let sd = &section_data[0];
                format!(
                    "<b>{} ({}) {}</b>\n<pre>{}</pre>",
                    esc(&sd.name),
                    esc(&sd.crs),
                    esc(now_str),
                    sd.rows.join("\n"),
                )
            } else {
                let sections: Vec<String> = section_data
                    .iter()
                    .map(|sd| {
                        format!(
                            "<b>{} ({})</b>\n<pre>{}</pre>",
                            esc(&sd.name),
                            esc(&sd.crs),
                            sd.rows.join("\n"),
                        )
                    })
                    .collect();
                format!(
                    "<b>Departures {}</b>\n\n{}",
                    esc(now_str),
                    sections.join("\n\n")
                )
            }
        }
        crate::domain::Channel::Twilio => {
            if section_data.len() == 1 {
                let sd = &section_data[0];
                format!(
                    "{} ({}) {}\n{}",
                    sd.name,
                    sd.crs,
                    now_str,
                    sd.rows.join("\n")
                )
            } else {
                let sections: Vec<String> = section_data
                    .iter()
                    .map(|sd| format!("{} ({}):\n{}", sd.name, sd.crs, sd.rows.join("\n")))
                    .collect();
                format!("Departures {}\n{}", now_str, sections.join("\n"))
            }
        }
    };

    Some(text)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Subscription;

    fn snap(
        id: &str,
        origin: &str,
        dest_crs: &str,
        dest_name: &str,
        etd: &str,
        platform: Option<&str>,
        cancelled: bool,
    ) -> ServiceSnapshot {
        ServiceSnapshot {
            service_id: id.to_string(),
            origin_crs: origin.to_string(),
            std: "12:00".to_string(),
            etd: etd.to_string(),
            platform: platform.map(str::to_string),
            destination_crs: dest_crs.to_string(),
            destination_name: dest_name.to_string(),
            is_cancelled: cancelled,
            cancel_reason: None,
            delay_reason: None,
            calling_point_crs: Vec::new(),
        }
    }

    fn wat_snap(id: &str, etd: &str, platform: Option<&str>, cancelled: bool) -> ServiceSnapshot {
        snap(id, "WAT", "RDG", "Reading", etd, platform, cancelled)
    }

    #[test]
    fn no_changes_when_identical() {
        let old: HashMap<_, _> =
            [("A".to_string(), wat_snap("A", "On time", Some("4"), false))].into();
        let new = old.clone();
        assert!(diff(Some(&old), &new).is_empty());
    }

    #[test]
    fn detects_new_service() {
        let old: HashMap<_, _> = HashMap::new();
        let new: HashMap<_, _> = [("A".to_string(), wat_snap("A", "On time", None, false))].into();
        let changes = diff(Some(&old), &new);
        assert_eq!(changes.len(), 1);
        assert!(matches!(changes[0].kind, ChangeKind::NewService));
    }

    #[test]
    fn detects_cancellation() {
        let old: HashMap<_, _> =
            [("A".to_string(), wat_snap("A", "On time", Some("4"), false))].into();
        let new: HashMap<_, _> =
            [("A".to_string(), wat_snap("A", "Cancelled", Some("4"), true))].into();
        let changes = diff(Some(&old), &new);
        assert!(
            changes
                .iter()
                .any(|c| matches!(c.kind, ChangeKind::Cancelled))
        );
    }

    #[test]
    fn detects_platform_change() {
        let old: HashMap<_, _> =
            [("A".to_string(), wat_snap("A", "On time", Some("4"), false))].into();
        let new: HashMap<_, _> =
            [("A".to_string(), wat_snap("A", "On time", Some("9"), false))].into();
        let changes = diff(Some(&old), &new);
        assert!(
            changes
                .iter()
                .any(|c| matches!(c.kind, ChangeKind::PlatformChanged))
        );
    }

    #[test]
    fn detects_delay() {
        let old: HashMap<_, _> =
            [("A".to_string(), wat_snap("A", "On time", Some("4"), false))].into();
        let new: HashMap<_, _> =
            [("A".to_string(), wat_snap("A", "12:15", Some("4"), false))].into();
        let changes = diff(Some(&old), &new);
        assert!(
            changes
                .iter()
                .any(|c| matches!(c.kind, ChangeKind::DelayChanged))
        );
    }

    #[test]
    fn digest_formats_on_time_service() {
        let snapshots: HashMap<String, ServiceSnapshot> = [(
            "A".to_string(),
            ServiceSnapshot {
                service_id: "A".to_string(),
                origin_crs: "WAT".to_string(),
                std: "09:00".to_string(),
                etd: "On time".to_string(),
                platform: Some("3".to_string()),
                destination_crs: "RDG".to_string(),
                destination_name: "Reading".to_string(),
                is_cancelled: false,
                cancel_reason: None,
                delay_reason: None,
                calling_point_crs: Vec::new(),
            },
        )]
        .into();

        let sub = Subscription::new("WAT".to_string(), vec![], None);
        let names: HashMap<String, String> = [("WAT".to_string(), "Waterloo".to_string())].into();
        let text = format_digest(
            &[sub],
            &snapshots,
            &HashSet::new(),
            "09:01",
            &names,
            &crate::domain::Channel::Telegram,
        )
        .expect("should produce a digest");
        assert!(text.contains("09:01"), "should include time");
        assert!(text.contains("WAT"), "should include CRS");
        assert!(text.contains("Reading"), "should include destination");
        assert!(text.contains("On time"), "should include status");
        assert!(text.contains("Pl 3"), "should include platform");
    }

    #[test]
    fn digest_shows_stop_count_for_destination_filter() {
        let snapshots: HashMap<String, ServiceSnapshot> = [(
            "A".to_string(),
            ServiceSnapshot {
                service_id: "A".to_string(),
                origin_crs: "WAT".to_string(),
                std: "09:00".to_string(),
                etd: "On time".to_string(),
                platform: Some("3".to_string()),
                destination_crs: "RDG".to_string(),
                destination_name: "Reading".to_string(),
                is_cancelled: false,
                cancel_reason: None,
                delay_reason: None,
                calling_point_crs: vec!["WOK".to_string(), "BAS".to_string(), "RDG".to_string()],
            },
        )]
        .into();

        let sub = Subscription::new("WAT".to_string(), vec!["RDG".to_string()], None);
        let names: HashMap<String, String> = [("WAT".to_string(), "Waterloo".to_string())].into();
        let text = format_digest(
            &[sub],
            &snapshots,
            &HashSet::new(),
            "09:01",
            &names,
            &crate::domain::Channel::Telegram,
        )
        .expect("should produce a digest");
        // RDG is at index 2 in calling_point_crs → 3 stops (including destination)
        assert!(text.contains("Reading 3"), "should include stop count");
    }

    #[test]
    fn digest_omits_stop_count_without_destination_filter() {
        let snapshots: HashMap<String, ServiceSnapshot> = [(
            "A".to_string(),
            ServiceSnapshot {
                service_id: "A".to_string(),
                origin_crs: "WAT".to_string(),
                std: "09:00".to_string(),
                etd: "On time".to_string(),
                platform: Some("3".to_string()),
                destination_crs: "RDG".to_string(),
                destination_name: "Reading".to_string(),
                is_cancelled: false,
                cancel_reason: None,
                delay_reason: None,
                calling_point_crs: vec!["WOK".to_string(), "RDG".to_string()],
            },
        )]
        .into();

        let sub = Subscription::new("WAT".to_string(), vec![], None);
        let names: HashMap<String, String> = HashMap::new();
        let text = format_digest(
            &[sub],
            &snapshots,
            &HashSet::new(),
            "09:01",
            &names,
            &crate::domain::Channel::Telegram,
        )
        .expect("should produce a digest");
        assert!(text.contains("Reading"), "should include destination name");
        assert!(!text.contains("Reading 1"), "should not include stop count");
    }

    #[test]
    fn digest_returns_none_when_no_snapshots() {
        let snapshots: HashMap<String, ServiceSnapshot> = HashMap::new();
        let sub = Subscription::new("WAT".to_string(), vec![], None);
        let names: HashMap<String, String> = HashMap::new();
        assert!(
            format_digest(
                &[sub],
                &snapshots,
                &HashSet::new(),
                "10:00",
                &names,
                &crate::domain::Channel::Telegram
            )
            .is_none()
        );
    }

    #[test]
    fn digest_marks_changed_rows() {
        let snapshots: HashMap<String, ServiceSnapshot> = [
            (
                "A".to_string(),
                ServiceSnapshot {
                    service_id: "A".to_string(),
                    origin_crs: "WAT".to_string(),
                    std: "09:00".to_string(),
                    etd: "On time".to_string(),
                    platform: Some("1".to_string()),
                    destination_crs: "RDG".to_string(),
                    destination_name: "Reading".to_string(),
                    is_cancelled: false,
                    cancel_reason: None,
                    delay_reason: None,
                    calling_point_crs: Vec::new(),
                },
            ),
            (
                "B".to_string(),
                ServiceSnapshot {
                    service_id: "B".to_string(),
                    origin_crs: "WAT".to_string(),
                    std: "09:10".to_string(),
                    etd: "09:25".to_string(),
                    platform: Some("2".to_string()),
                    destination_crs: "GLD".to_string(),
                    destination_name: "Guildford".to_string(),
                    is_cancelled: false,
                    cancel_reason: None,
                    delay_reason: None,
                    calling_point_crs: Vec::new(),
                },
            ),
        ]
        .into();

        // Service B changed (delayed); service A did not.
        let changed_ids: HashSet<String> = ["B".to_string()].into();

        let sub = Subscription::new("WAT".to_string(), vec![], None);
        let names: HashMap<String, String> = HashMap::new();
        let text = format_digest(
            &[sub],
            &snapshots,
            &changed_ids,
            "09:05",
            &names,
            &crate::domain::Channel::Telegram,
        )
        .expect("should produce digest");

        // The Guildford row (B, changed) must have the marker.
        let guildford_line = text
            .lines()
            .find(|l| l.contains("Guildford"))
            .expect("Guildford row");
        assert!(
            guildford_line.contains('*'),
            "changed row should have * marker"
        );

        // The Reading row (A, unchanged) must NOT have the marker.
        let reading_line = text
            .lines()
            .find(|l| l.contains("Reading"))
            .expect("Reading row");
        assert!(
            !reading_line.contains('*'),
            "unchanged row should not have * marker"
        );
    }
}
