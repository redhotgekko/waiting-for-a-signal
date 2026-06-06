//! Channel-agnostic command execution helpers.
//!
//! Every function here takes an [`AppState`] reference and a [`UserKey`] and
//! returns a `String` (HTML-formatted for Telegram; channel adapters strip
//! the tags before sending via non-Telegram channels).
//!
//! None of these functions import Telegram or any other channel type.

use crate::domain::{Channel, Schedule, Subscription, TimeWindow};
use crate::handlers::AppState;
use anyhow::Result;
use chrono::{NaiveTime, Utc, Weekday};
use chrono_tz::Europe::London;

/// Output format for command responses.
///
/// `Html` targets Telegram's HTML subset — callers must pass the string
/// directly to the Telegram API.  `Plain` targets non-Telegram channels —
/// no tags, no HTML entities, no column padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    Html,
    Plain,
}

impl From<&Channel> for Format {
    fn from(ch: &Channel) -> Self {
        match ch {
            Channel::Telegram => Format::Html,
            Channel::Twilio => Format::Plain,
        }
    }
}

/// Escape a string for Telegram HTML mode.
/// Only the three entities Telegram's HTML subset recognises are encoded.
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

/// SMS help text - mirrors the Telegram Command descriptions.
/// Plain ASCII only: SMS (GSM-7) is a 7-bit encoding with no Unicode support.
pub fn exec_help() -> String {
    "Available commands:\n\
     /help - Show all commands.\n\
     /find - Search for a CRS station code by name.\n\
     /now - Live departures. Args: CRS [to CRS]\n\
     /sub - Create a subscription. Args: CRS [to CRS] [DAYS HH:MM-HH:MM]\n\
     /unsub - Remove subscription(s). Args: ID or all\n\
     /subs - List your subscriptions.\n\
     /pause - Pause notifications. Args: [MINUTES]\n\
     /resume - Resume notifications."
        .to_string()
}

// ---------------------------------------------------------------------------
// Station validation helper
// ---------------------------------------------------------------------------

/// Returns `Ok(())` if `crs` is a known station code, or `Err(HTML message)`.
pub(crate) fn require_crs(crs: &str, state: &AppState) -> Result<(), String> {
    if state.stations.by_crs(crs).is_some() {
        Ok(())
    } else {
        Err(format!(
            "<code>{}</code> is not a recognised station code. \
             Use <code>/find &lt;name&gt;</code> to look one up.",
            esc(crs)
        ))
    }
}

// ---------------------------------------------------------------------------
// Station search
// ---------------------------------------------------------------------------

pub(crate) fn exec_find(query: &str, state: &AppState) -> String {
    use crate::stations::FindResult;
    match state.stations.find(query) {
        FindResult::Exact(crs) => {
            let name = state
                .stations
                .by_crs(&crs)
                .map(|s| s.name.as_str())
                .unwrap_or("Unknown");
            format!("Found: <b>{}</b> — <code>{}</code>", esc(name), esc(&crs))
        }
        FindResult::Ambiguous(candidates) => {
            let list: Vec<String> = candidates
                .iter()
                .map(|(name, crs)| format!("• {} — <code>{}</code>", esc(name), esc(crs)))
                .collect();
            format!("Multiple matches:\n{}", list.join("\n"))
        }
        FindResult::NotFound => format!("No station found matching \"{}\".", esc(query)),
    }
}

// ---------------------------------------------------------------------------
// Subscriptions
// ---------------------------------------------------------------------------

pub(crate) async fn exec_subs(state: &AppState, user_key: &crate::domain::UserKey) -> String {
    let user_arc = state.store.get_or_create(user_key.clone()).await;
    let user = user_arc.read().await;
    if user.subscriptions.is_empty() {
        return "You have no active subscriptions. Use <code>/sub CRS</code> to subscribe."
            .to_string();
    }
    let now = Utc::now();
    let lines: Vec<String> = user
        .subscriptions
        .iter()
        .map(|s| {
            let dests = if s.destination_filter.is_empty() {
                "all destinations".to_string()
            } else {
                s.destination_filter.join(", ")
            };
            let sched = match &s.schedule {
                None => String::new(),
                Some(sc) => format!(
                    " | {} {}",
                    format_days(&sc.days),
                    format_windows(&sc.windows)
                ),
            };
            let expiry = match s.expires_at {
                None => String::new(),
                Some(exp) => {
                    let local = exp.with_timezone(&London);
                    let remaining = exp.signed_duration_since(now);
                    let mins = remaining.num_minutes().max(0);
                    format!(" | expires {} ({}m)", local.format("%H:%M"), mins)
                }
            };
            format!(
                "• <code>{}</code> → {}<code>[{}]</code>{}{}",
                esc(&s.origin_crs),
                esc(&dests),
                esc(&s.display_id),
                esc(&sched),
                esc(&expiry),
            )
        })
        .collect();
    format!(
        "Your subscriptions:\n{}\n\nUse <code>/unsub ID</code> to remove one, or <code>/unsub all</code> to remove all.",
        lines.join("\n")
    )
}

pub(crate) async fn exec_unsub(
    id: &str,
    state: &AppState,
    user_key: &crate::domain::UserKey,
) -> String {
    let id = id.trim();
    if id.is_empty() {
        return "Usage: <code>/unsub ID</code> or <code>/unsub all</code> — use <code>/subs</code> to see IDs.".to_string();
    }
    if id.eq_ignore_ascii_case("all") {
        return exec_clear(state, user_key).await;
    }

    let user_arc = state.store.get_or_create(user_key.clone()).await;
    let mut user = user_arc.write().await;

    let found = user
        .subscriptions
        .iter()
        .find(|s| s.display_id == id)
        .map(|s| {
            let dests = if s.destination_filter.is_empty() {
                "all destinations".to_string()
            } else {
                s.destination_filter.join(", ")
            };
            (
                s.id.clone(),
                format!("<code>{}</code> → {}", esc(&s.origin_crs), esc(&dests)),
            )
        });

    match found {
        None => format!(
            "No subscription found with ID <code>{}</code>. \
             Use <code>/subs</code> to see your subscriptions.",
            esc(id)
        ),
        Some((target_id, description)) => {
            user.subscriptions.retain(|s| s.id != target_id);
            let snapshot = user.clone();
            drop(user);
            match state.store.persist(&snapshot).await {
                Ok(()) => format!("Removed subscription: {}", description),
                Err(e) => format!("Error: {}", esc(&e.to_string())),
            }
        }
    }
}

pub(crate) async fn exec_watch(
    origin: &str,
    destinations: &[String],
    state: &AppState,
    user_key: &crate::domain::UserKey,
    format: Format,
) -> String {
    if let Err(msg) = require_crs(origin, state) {
        return msg;
    }
    for dest in destinations {
        if let Err(msg) = require_crs(dest, state) {
            return msg;
        }
    }

    let user_arc = state.store.get_or_create(user_key.clone()).await;

    {
        let user = user_arc.read().await;
        for existing in &user.subscriptions {
            if existing.origin_crs != origin {
                continue;
            }
            // Scheduled subs only fire during their window; a new temporary sub
            // is distinct and should always be allowed alongside them.
            if existing.schedule.is_some() {
                continue;
            }
            if existing.destination_filter.is_empty() {
                return if destinations.is_empty() {
                    format!(
                        "Already watching all departures from <code>{}</code>.",
                        esc(origin)
                    )
                } else {
                    format!(
                        "Already watching all departures from <code>{}</code> \
                         (that already covers those destinations).",
                        esc(origin)
                    )
                };
            }
            if destinations.is_empty() {
                continue;
            }
            let mut ex = existing.destination_filter.clone();
            ex.sort_unstable();
            let mut req = destinations.to_vec();
            req.sort_unstable();
            if ex == req {
                return format!(
                    "Already watching departures from <code>{}</code> to those destinations.",
                    esc(origin)
                );
            }
        }
    }

    let expires_at = Utc::now() + chrono::Duration::minutes(Subscription::DEFAULT_EXPIRY_MINUTES);
    let expires_local = expires_at.with_timezone(&London);

    let mut sub = Subscription::new(origin.to_string(), destinations.to_vec(), None);
    sub.expires_at = Some(expires_at);

    let mut user = user_arc.write().await;
    sub.display_id =
        Subscription::next_display_id(&user.subscriptions).unwrap_or_else(|| "??".to_string());
    user.subscriptions.push(sub);
    let snapshot = user.clone();
    drop(user);

    match state.store.persist(&snapshot).await {
        Ok(()) => {
            let confirmed = if destinations.is_empty() {
                format!(
                    "Watching all departures from <code>{}</code>. Expires at {} ({}m).",
                    esc(origin),
                    expires_local.format("%H:%M"),
                    Subscription::DEFAULT_EXPIRY_MINUTES,
                )
            } else {
                format!(
                    "Watching departures from <code>{}</code> to <code>{}</code>. Expires at {} ({}m).",
                    esc(origin),
                    esc(&destinations.join(", ")),
                    expires_local.format("%H:%M"),
                    Subscription::DEFAULT_EXPIRY_MINUTES,
                )
            };
            let board = exec_now(origin, destinations, state, format).await;
            format!("{confirmed}\n\n{board}")
        }
        Err(e) => format!("Error saving subscription: {}", esc(&e.to_string())),
    }
}

pub(crate) async fn exec_clear(state: &AppState, user_key: &crate::domain::UserKey) -> String {
    let user_arc = state.store.get_or_create(user_key.clone()).await;
    let mut user = user_arc.write().await;
    let count = user.subscriptions.len();
    user.subscriptions.clear();
    let snapshot = user.clone();
    drop(user);

    if count == 0 {
        return "You have no subscriptions to remove.".to_string();
    }
    match state.store.persist(&snapshot).await {
        Ok(()) => format!(
            "Removed {} subscription{}.",
            count,
            if count == 1 { "" } else { "s" }
        ),
        Err(e) => format!("Error: {}", esc(&e.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Pause / resume
// ---------------------------------------------------------------------------

pub(crate) async fn exec_resume(state: &AppState, user_key: &crate::domain::UserKey) -> String {
    let user_arc = state.store.get_or_create(user_key.clone()).await;
    let mut user = user_arc.write().await;
    user.notifications_paused = false;
    user.paused_until = None;
    let snapshot = user.clone();
    drop(user);
    match state.store.persist(&snapshot).await {
        Ok(()) => "Notifications resumed.".to_string(),
        Err(e) => format!("Error: {}", esc(&e.to_string())),
    }
}

pub(crate) async fn exec_pause(
    args: &str,
    state: &AppState,
    user_key: &crate::domain::UserKey,
) -> String {
    let args = args.trim();

    let user_arc = state.store.get_or_create(user_key.clone()).await;
    let mut user = user_arc.write().await;

    if args.is_empty() {
        user.notifications_paused = true;
        user.paused_until = None;
        let snapshot = user.clone();
        drop(user);
        return match state.store.persist(&snapshot).await {
            Ok(()) => "Notifications paused. Use <code>/resume</code> to restart them.".to_string(),
            Err(e) => format!("Error: {}", esc(&e.to_string())),
        };
    }

    match args.parse::<u64>() {
        Ok(mins) if mins > 0 => {
            let until = Utc::now() + chrono::Duration::minutes(mins as i64);
            let until_local = until.with_timezone(&London);
            user.notifications_paused = false;
            user.paused_until = Some(until);
            let snapshot = user.clone();
            drop(user);
            match state.store.persist(&snapshot).await {
                Ok(()) => format!(
                    "Notifications paused for {}m (until {}). Use <code>/resume</code> to restart early.",
                    mins,
                    until_local.format("%H:%M"),
                ),
                Err(e) => format!("Error: {}", esc(&e.to_string())),
            }
        }
        _ => "Usage: <code>/pause</code> or <code>/pause MINUTES</code>".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Kill switch
// ---------------------------------------------------------------------------

/// Triggers a graceful service shutdown. Responds before signalling so the
/// Telegram/SMS reply is delivered.
pub(crate) fn exec_kill(state: &AppState) -> String {
    if !state.kill_switch_enabled {
        return "Command not available.".to_string();
    }
    let _ = state.shutdown.send(());
    "Shutting down.".to_string()
}

// ---------------------------------------------------------------------------
// Unified subscription command
// ---------------------------------------------------------------------------

fn sub_usage() -> String {
    "Usage:\n\
     <code>/sub CRS</code> — temporary subscription\n\
     <code>/sub CRS to CRS</code> — filter to destination\n\
     <code>/sub CRS DAYS HH:MM-HH:MM</code> — recurring schedule\n\
     <code>/sub CRS to CRS DAYS HH:MM-HH:MM</code> — scheduled + filtered\n\
     Days: <code>weekdays</code>, <code>weekends</code>, <code>daily</code>, \
     <code>Mon-Fri</code>, <code>Mon,Wed,Fri</code>"
        .to_string()
}

pub(crate) async fn exec_sub(
    args: &str,
    state: &AppState,
    user_key: &crate::domain::UserKey,
    format: Format,
) -> String {
    let args = args.trim();
    if args.is_empty() {
        return sub_usage();
    }

    let tokens: Vec<&str> = args.split_whitespace().collect();
    let mut i = 0;
    let arg1 = tokens[i];
    i += 1;

    let origin = arg1.to_uppercase();
    let mut destinations = Vec::new();
    if tokens.get(i).map(|t| t.eq_ignore_ascii_case("to")) == Some(true) {
        i += 1;
        if let Some(dest_str) = tokens.get(i) {
            destinations = dest_str
                .split(',')
                .map(|s| s.trim().to_uppercase())
                .filter(|s| !s.is_empty())
                .collect();
            i += 1;
        }
    }

    if i >= tokens.len() {
        return exec_watch(&origin, &destinations, state, user_key, format).await;
    }

    // Schedule: DAYS HH:MM-HH:MM[,HH:MM-HH:MM]
    let days_str = tokens[i];
    i += 1;
    let days = match parse_days(days_str) {
        Ok(d) => d,
        Err(msg) => return msg,
    };
    if i >= tokens.len() {
        return sub_usage();
    }
    let windows_str = tokens[i..].join(",");
    let windows = match parse_windows(&windows_str) {
        Ok(w) => w,
        Err(msg) => return msg,
    };

    create_scheduled_sub(&origin, &destinations, days, windows, state, user_key).await
}

// ---------------------------------------------------------------------------
// Scheduled subscriptions — parsing helpers
// ---------------------------------------------------------------------------

const ALL_DAYS: [Weekday; 7] = [
    Weekday::Mon,
    Weekday::Tue,
    Weekday::Wed,
    Weekday::Thu,
    Weekday::Fri,
    Weekday::Sat,
    Weekday::Sun,
];

fn day_abbr(d: Weekday) -> &'static str {
    match d {
        Weekday::Mon => "Mon",
        Weekday::Tue => "Tue",
        Weekday::Wed => "Wed",
        Weekday::Thu => "Thu",
        Weekday::Fri => "Fri",
        Weekday::Sat => "Sat",
        Weekday::Sun => "Sun",
    }
}

fn day_index(d: Weekday) -> usize {
    match d {
        Weekday::Mon => 0,
        Weekday::Tue => 1,
        Weekday::Wed => 2,
        Weekday::Thu => 3,
        Weekday::Fri => 4,
        Weekday::Sat => 5,
        Weekday::Sun => 6,
    }
}

fn parse_day(s: &str) -> Result<Weekday, String> {
    match s.to_lowercase().as_str() {
        "mon" | "monday" => Ok(Weekday::Mon),
        "tue" | "tuesday" => Ok(Weekday::Tue),
        "wed" | "wednesday" => Ok(Weekday::Wed),
        "thu" | "thursday" => Ok(Weekday::Thu),
        "fri" | "friday" => Ok(Weekday::Fri),
        "sat" | "saturday" => Ok(Weekday::Sat),
        "sun" | "sunday" => Ok(Weekday::Sun),
        other => Err(format!(
            "Unknown day \"{}\" — use Mon/Tue/…/Sun",
            esc(other)
        )),
    }
}

/// Parse a days string into a list of weekdays.
/// Accepts: `weekdays`, `weekends`, `daily`, `Mon-Fri`, `Mon,Wed,Fri`, `Mon`.
fn parse_days(s: &str) -> Result<Vec<Weekday>, String> {
    match s.to_lowercase().as_str() {
        "weekdays" | "weekday" => return Ok(ALL_DAYS[..5].to_vec()),
        "weekends" | "weekend" => return Ok(ALL_DAYS[5..].to_vec()),
        "daily" | "everyday" | "all" => return Ok(ALL_DAYS.to_vec()),
        _ => {}
    }
    // Range: Mon-Fri
    if let Some((a, b)) = s.split_once('-')
        && let (Ok(from), Ok(to)) = (parse_day(a.trim()), parse_day(b.trim()))
    {
        let (fi, ti) = (day_index(from), day_index(to));
        if fi <= ti {
            return Ok(ALL_DAYS[fi..=ti].to_vec());
        }
        return Err(format!(
            "Day range {} before {} — write it as {}-{} if that's what you meant",
            day_abbr(from),
            day_abbr(to),
            day_abbr(to),
            day_abbr(from)
        ));
    }
    // Comma list: Mon,Wed,Fri
    s.split(',').map(|d| parse_day(d.trim())).collect()
}

fn parse_time(s: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M")
        .map_err(|_| format!("Invalid time \"{}\" — expected HH:MM", esc(s.trim())))
}

/// Parse a single `HH:MM-HH:MM` window.
fn parse_window(s: &str) -> Result<TimeWindow, String> {
    let s = s.trim();
    // split_once('-') is safe here: HH:MM-HH:MM contains exactly one '-'
    let (start_s, end_s) = s
        .split_once('-')
        .ok_or_else(|| format!("Invalid window \"{}\" — expected HH:MM-HH:MM", esc(s)))?;
    Ok(TimeWindow::new(parse_time(start_s)?, parse_time(end_s)?))
}

/// Parse comma-separated time windows.
fn parse_windows(s: &str) -> Result<Vec<TimeWindow>, String> {
    if s.is_empty() {
        return Err("No time window specified — expected HH:MM-HH:MM".to_string());
    }
    s.split(',').map(|w| parse_window(w.trim())).collect()
}

// ---------------------------------------------------------------------------
// Departure board formatting helpers
// ---------------------------------------------------------------------------

/// Returns the column width to use for a set of string lengths.
///
/// Uses the median as an anchor and ignores outliers (> 2×median + 10) so that
/// one unusually long station name does not blow out the whole table.
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

// ---------------------------------------------------------------------------
// Scheduled subscriptions — display helpers
// ---------------------------------------------------------------------------

pub(crate) fn format_days(days: &[Weekday]) -> String {
    let set: std::collections::HashSet<usize> = days.iter().map(|d| day_index(*d)).collect();
    if (0..7).all(|i| set.contains(&i)) {
        return "daily".to_string();
    }
    if (0..5).all(|i| set.contains(&i)) && !set.contains(&5) && !set.contains(&6) {
        return "weekdays".to_string();
    }
    if set.contains(&5) && set.contains(&6) && set.len() == 2 {
        return "weekends".to_string();
    }
    days.iter()
        .map(|d| day_abbr(*d))
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn format_windows(windows: &[TimeWindow]) -> String {
    windows
        .iter()
        .map(|w| format!("{}-{}", w.start.format("%H:%M"), w.end.format("%H:%M")))
        .collect::<Vec<_>>()
        .join(",")
}

// ---------------------------------------------------------------------------
// Scheduled subscriptions — command handlers
// ---------------------------------------------------------------------------

async fn create_scheduled_sub(
    origin: &str,
    destinations: &[String],
    days: Vec<Weekday>,
    windows: Vec<TimeWindow>,
    state: &AppState,
    user_key: &crate::domain::UserKey,
) -> String {
    if let Err(msg) = require_crs(origin, state) {
        return msg;
    }
    for dest in destinations {
        if let Err(msg) = require_crs(dest, state) {
            return msg;
        }
    }

    let schedule = Schedule { days, windows };

    let user_arc = state.store.get_or_create(user_key.clone()).await;
    let mut user = user_arc.write().await;

    let display_id = match crate::domain::Subscription::next_display_id(&user.subscriptions) {
        Some(id) => id,
        None => return "You have reached the maximum number of subscriptions (99).".to_string(),
    };

    let mut sub = crate::domain::Subscription::new(
        origin.to_string(),
        destinations.to_vec(),
        Some(schedule.clone()),
    );
    sub.display_id = display_id.clone();
    user.subscriptions.push(sub);
    let snapshot = user.clone();
    drop(user);

    match state.store.persist(&snapshot).await {
        Ok(()) => {
            let dest_str = if destinations.is_empty() {
                "all destinations".to_string()
            } else {
                destinations
                    .iter()
                    .map(|d| format!("<code>{}</code>", esc(d)))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            format!(
                "Scheduled subscription added <code>[{}]</code>:\n\
                 <code>{}</code> \u{2192} {} | {} {}",
                esc(&display_id),
                esc(origin),
                dest_str,
                esc(&format_days(&schedule.days)),
                esc(&format_windows(&schedule.windows)),
            )
        }
        Err(e) => format!("Error saving subscription: {}", esc(&e.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Live departures
// ---------------------------------------------------------------------------

pub(crate) async fn exec_now(
    origin_crs: &str,
    destinations: &[String],
    state: &AppState,
    format: Format,
) -> String {
    if let Err(msg) = require_crs(origin_crs, state) {
        return msg;
    }
    for dest in destinations {
        if let Err(msg) = require_crs(dest, state) {
            return msg;
        }
    }

    let filter_crs = if destinations.len() == 1 {
        destinations.first().map(String::as_str)
    } else {
        None
    };

    state.metrics.record_darwin_requests(1).await;

    match state
        .darwin
        .get_departure_board(origin_crs, state.polling_rows, filter_crs)
        .await
    {
        Ok(services) if services.is_empty() => match format {
            Format::Html => format!("No departures found from <b>{}</b>.", esc(origin_crs)),
            Format::Plain => format!("No departures found from {}.", origin_crs),
        },
        Ok(services) => {
            // Include trains that terminate at the requested station OR call at it
            // as an intermediate stop.
            let filtered: Vec<&crate::darwin::Service> = if destinations.is_empty() {
                services.iter().collect()
            } else {
                services
                    .iter()
                    .filter(|s| {
                        destinations.iter().any(|d| {
                            d.eq_ignore_ascii_case(&s.destination_crs)
                                || s.calling_point_crs
                                    .iter()
                                    .any(|cp| cp.eq_ignore_ascii_case(d))
                        })
                    })
                    .collect()
            };

            if filtered.is_empty() {
                return match format {
                    Format::Html => format!(
                        "No departures from <b>{}</b> to the requested destination(s).",
                        esc(origin_crs)
                    ),
                    Format::Plain => format!(
                        "No departures from {} to the requested destination(s).",
                        origin_crs
                    ),
                };
            }

            struct NowRow {
                time: String,
                dest: String,
                status: String,
                plat: String,
            }

            let raw: Vec<NowRow> = filtered
                .iter()
                .take(10)
                .map(|s| {
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
                    let dest = if destinations.is_empty() {
                        s.destination_name.clone()
                    } else {
                        let stop_count = destinations.iter().find_map(|dest_crs| {
                            s.calling_point_crs
                                .iter()
                                .position(|crs| crs.eq_ignore_ascii_case(dest_crs))
                        });
                        match stop_count {
                            Some(n) => format!("{} {}", s.destination_name, n + 1),
                            None => s.destination_name.clone(),
                        }
                    };
                    NowRow {
                        time: s.std.clone(),
                        dest,
                        status,
                        plat: s.platform.as_deref().unwrap_or("TBC").to_string(),
                    }
                })
                .collect();

            match format {
                Format::Html => {
                    let dest_w = col_width(raw.iter().map(|r| r.dest.len()));
                    let status_w = col_width(raw.iter().map(|r| r.status.len()));

                    let rows: Vec<String> = raw
                        .iter()
                        .map(|r| {
                            // Pad before escaping so spaces are not HTML-encoded.
                            let dest_padded = format!("{:<width$}", r.dest, width = dest_w);
                            let status_padded = format!("{:<width$}", r.status, width = status_w);
                            format!(
                                "{} {} {} Pl {}",
                                esc(&r.time),
                                esc(&dest_padded),
                                esc(&status_padded),
                                esc(&r.plat),
                            )
                        })
                        .collect();

                    let pre_rows = format!("<pre>{}</pre>", rows.join("\n"));

                    if destinations.is_empty() {
                        format!("<b>Departures from {}:</b>\n{}", esc(origin_crs), pre_rows)
                    } else {
                        let dest_codes = destinations
                            .iter()
                            .map(|d| format!("<code>{}</code>", esc(d)))
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!(
                            "<b>Departures from {} to {}:</b>\n{}",
                            esc(origin_crs),
                            dest_codes,
                            pre_rows
                        )
                    }
                }
                Format::Plain => {
                    // Compact plain-text layout for SMS: no column padding,
                    // no HTML markup.
                    let header = if destinations.is_empty() {
                        format!("{} departures:", origin_crs)
                    } else {
                        format!("{} to {}:", origin_crs, destinations.join(", "))
                    };
                    let rows: Vec<String> = raw
                        .iter()
                        .map(|r| {
                            let plat = if r.plat == "TBC" {
                                "--".to_string()
                            } else {
                                format!("P{}", r.plat)
                            };
                            format!("{} {} {} {}", r.time, r.dest, r.status, plat)
                        })
                        .collect();
                    format!("{}\n{}", header, rows.join("\n"))
                }
            }
        }
        Err(e) => format!("Could not fetch departures: {}", esc(&e.to_string())),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_days_keywords() {
        assert_eq!(parse_days("weekdays").unwrap(), ALL_DAYS[..5].to_vec());
        assert_eq!(parse_days("weekends").unwrap(), ALL_DAYS[5..].to_vec());
        assert_eq!(parse_days("daily").unwrap(), ALL_DAYS.to_vec());
    }

    #[test]
    fn parse_days_range() {
        let days = parse_days("Mon-Fri").unwrap();
        assert_eq!(
            days,
            vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri
            ]
        );
    }

    #[test]
    fn parse_days_comma_list() {
        let days = parse_days("Mon,Wed,Fri").unwrap();
        assert_eq!(days, vec![Weekday::Mon, Weekday::Wed, Weekday::Fri]);
    }

    #[test]
    fn parse_window_normal() {
        let w = parse_window("07:30-09:00").unwrap();
        assert_eq!(w.start, NaiveTime::from_hms_opt(7, 30, 0).unwrap());
        assert_eq!(w.end, NaiveTime::from_hms_opt(9, 0, 0).unwrap());
    }

    #[test]
    fn parse_window_overnight() {
        let w = parse_window("23:00-02:00").unwrap();
        assert_eq!(w.start, NaiveTime::from_hms_opt(23, 0, 0).unwrap());
        assert_eq!(w.end, NaiveTime::from_hms_opt(2, 0, 0).unwrap());
    }

    #[test]
    fn format_days_roundtrips() {
        assert_eq!(format_days(&ALL_DAYS[..5].to_vec()), "weekdays");
        assert_eq!(format_days(&ALL_DAYS[5..].to_vec()), "weekends");
        assert_eq!(format_days(&ALL_DAYS.to_vec()), "daily");
        assert_eq!(format_days(&[Weekday::Mon, Weekday::Wed]), "Mon,Wed");
    }

    #[test]
    fn format_windows_output() {
        let w = TimeWindow::new(
            NaiveTime::from_hms_opt(7, 30, 0).unwrap(),
            NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
        );
        assert_eq!(format_windows(&[w]), "07:30-09:00");
    }
}
