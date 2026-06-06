use chrono::{DateTime, Datelike, NaiveTime, Utc, Weekday};
use chrono_tz::Europe::London;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use ulid::Ulid;

/// A time window within a day (local UK time).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeWindow {
    pub start: NaiveTime,
    pub end: NaiveTime,
}

impl TimeWindow {
    pub fn new(start: NaiveTime, end: NaiveTime) -> Self {
        Self { start, end }
    }

    /// Returns true if `t` falls within this window, handling overnight wrap-around.
    pub fn contains(&self, t: NaiveTime) -> bool {
        if self.start <= self.end {
            t >= self.start && t < self.end
        } else {
            // overnight: e.g. 23:00–02:00
            t >= self.start || t < self.end
        }
    }
}

/// Restricts a subscription to specific days and time windows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schedule {
    /// Days of the week this schedule applies to.
    pub days: Vec<Weekday>,
    pub windows: Vec<TimeWindow>,
}

impl Schedule {
    /// Returns true if `now` (UTC) falls within an active window for this
    /// schedule, evaluated in Europe/London local time.
    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        let local = now.with_timezone(&London);
        let weekday = local.weekday();
        if !self.days.contains(&weekday) {
            return false;
        }
        let current_time = local.time();
        self.windows.iter().any(|w| w.contains(current_time))
    }
}

/// A user's subscription to departures from an origin station.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    /// Stable globally-unique ULID.
    pub id: String,
    /// Short per-user display ID shown in /list (e.g. "01", "02").
    /// Assigned at creation; unique within one user's subscription list.
    /// `default` allows old files without this field to deserialise cleanly;
    /// the v1→v2 schema migration then backfills it.
    #[serde(default)]
    pub display_id: String,
    /// 3-letter CRS code, uppercase.
    pub origin_crs: String,
    /// Empty = all destinations.
    pub destination_filter: Vec<String>,
    /// `None` = always active.
    pub schedule: Option<Schedule>,
    pub created_at: DateTime<Utc>,
    /// Absolute UTC time after which this subscription auto-expires.
    /// `None` = no expiry (scheduled subscriptions never auto-expire).
    /// Set by the caller when creating an unscheduled subscription.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

impl Subscription {
    /// Default auto-expiry duration for unscheduled subscriptions (30 minutes).
    pub const DEFAULT_EXPIRY_MINUTES: i64 = 30;
    pub fn new(
        origin_crs: String,
        destination_filter: Vec<String>,
        schedule: Option<Schedule>,
    ) -> Self {
        Self {
            id: Ulid::new().to_string(),
            display_id: String::new(), // caller must set via next_display_id()
            origin_crs,
            destination_filter,
            schedule,
            created_at: Utc::now(),
            expires_at: None,
        }
    }

    /// Returns true if this subscription has passed its `expires_at` deadline.
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|exp| now >= exp)
    }

    /// Returns the smallest two-digit decimal ID (`"01"`–`"99"`) not already
    /// used by any subscription in `existing`.
    ///
    /// Returns `None` only if all 99 slots are occupied, which should never
    /// happen for a normal user.
    pub fn next_display_id(existing: &[Subscription]) -> Option<String> {
        let used: HashSet<&str> = existing.iter().map(|s| s.display_id.as_str()).collect();
        for n in 1u32..=99 {
            let candidate = format!("{n:02}");
            if !used.contains(candidate.as_str()) {
                return Some(candidate);
            }
        }
        None
    }

    /// Returns true if this subscription should produce notifications right now.
    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        match &self.schedule {
            None => true,
            Some(sched) => sched.is_active_at(now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn utc(y: i32, mo: u32, d: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, m, 0).unwrap()
    }

    fn t(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    // -----------------------------------------------------------------------
    // TimeWindow::contains
    // -----------------------------------------------------------------------

    #[test]
    fn window_normal_contains() {
        let w = TimeWindow::new(t(9, 0), t(17, 0));
        assert!(w.contains(t(9, 0)));
        assert!(w.contains(t(12, 30)));
        assert!(!w.contains(t(17, 0))); // end is exclusive
        assert!(!w.contains(t(8, 59)));
    }

    #[test]
    fn window_overnight_contains() {
        let w = TimeWindow::new(t(23, 0), t(2, 0));
        assert!(w.contains(t(23, 30)));
        assert!(w.contains(t(0, 0)));
        assert!(w.contains(t(1, 59)));
        assert!(!w.contains(t(2, 0)));
        assert!(!w.contains(t(12, 0)));
    }

    // -----------------------------------------------------------------------
    // Schedule::is_active_at  (all times in Europe/London = UTC+1 in summer)
    // -----------------------------------------------------------------------

    fn weekday_schedule() -> Schedule {
        Schedule {
            days: vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
            windows: vec![TimeWindow::new(t(7, 30), t(9, 0))],
        }
    }

    /// 2024-05-15 is a Wednesday (BST = UTC+1).
    /// UTC 06:30 = London 07:30 → inside window.
    #[test]
    fn schedule_active_inside_window_bst() {
        let now = utc(2024, 5, 15, 6, 30);
        assert!(weekday_schedule().is_active_at(now));
    }

    /// UTC 06:16 = London 07:16 → before window start (07:30), should be inactive.
    #[test]
    fn schedule_inactive_before_window() {
        let now = utc(2024, 5, 15, 6, 16);
        assert!(!weekday_schedule().is_active_at(now));
    }

    /// UTC 06:29 = London 07:29 → one minute before window start, inactive.
    #[test]
    fn schedule_inactive_one_minute_before_window() {
        let now = utc(2024, 5, 15, 6, 29);
        assert!(!weekday_schedule().is_active_at(now));
    }

    /// UTC 08:01 = London 09:01 → after window ends.
    #[test]
    fn schedule_inactive_after_window() {
        let now = utc(2024, 5, 15, 8, 1);
        assert!(!weekday_schedule().is_active_at(now));
    }

    /// 2024-05-18 is a Saturday → not in weekday schedule.
    #[test]
    fn schedule_inactive_wrong_day() {
        let now = utc(2024, 5, 18, 6, 30);
        assert!(!weekday_schedule().is_active_at(now));
    }

    /// BST→GMT transition: 2024-10-27 clocks go back.
    /// UTC 06:30 on that Sunday = London 07:30 (BST ends at 01:00 UTC).
    /// But it's Sunday so the weekday schedule is still inactive.
    #[test]
    fn schedule_bst_gmt_transition_day() {
        let now = utc(2024, 10, 27, 6, 30);
        assert!(!weekday_schedule().is_active_at(now)); // Sunday
    }

    /// GMT: 2024-01-15 Monday. UTC 07:30 = London 07:30 (GMT, no offset).
    #[test]
    fn schedule_active_gmt() {
        let now = utc(2024, 1, 15, 7, 30);
        assert!(weekday_schedule().is_active_at(now));
    }
}
