mod diff;

use crate::config::PollingConfig;
use crate::darwin::DepartureSource;
use crate::domain::UserKey;
use crate::notifier::{Notifier, OutboundMessage};
use crate::stations::StationIndex;
use crate::storage::UserStore;
use chrono::Utc;
use chrono_tz::Europe::London;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Duration, sleep};
use tracing::{debug, error, info, warn};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// One Darwin API call: an origin CRS plus an optional destination filter.
/// Subscriptions with no destination become `(origin, None)`.
/// Subscriptions with N destinations become N entries `(origin, Some(dest))`.
/// Identical keys across all users are deduplicated so each unique pair is
/// fetched exactly once per cycle.
type CallKey = (String, Option<String>);
type DarwinResult = (
    String,
    Option<String>,
    Result<Vec<crate::darwin::Service>, crate::darwin::DarwinError>,
);

pub fn spawn(
    store: Arc<UserStore>,
    darwin: Arc<dyn DepartureSource>,
    notifier: Arc<dyn Notifier>,
    stations: Arc<StationIndex>,
    cfg: PollingConfig,
    metrics: Arc<crate::metrics::MetricsStore>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        run_loop(store, darwin, notifier, stations, cfg, metrics).await;
    })
}

async fn run_loop(
    store: Arc<UserStore>,
    darwin: Arc<dyn DepartureSource>,
    notifier: Arc<dyn Notifier>,
    stations: Arc<StationIndex>,
    cfg: PollingConfig,
    metrics: Arc<crate::metrics::MetricsStore>,
) {
    // Per-user last-sent snapshot: user_key → { service_id → snapshot }.
    // Updated only when a notification is sent successfully, so the diff
    // always reflects what the user last saw.
    let mut user_snapshots: HashMap<UserKey, HashMap<String, diff::ServiceSnapshot>> =
        HashMap::new();
    let interval = Duration::from_secs(cfg.interval_seconds);

    info!(interval_seconds = cfg.interval_seconds, "Poller started");

    loop {
        let now = Utc::now();
        debug!("Poll cycle: collecting active subscriptions");
        let all_users = store.all_users().await;

        // -------------------------------------------------------------------
        // 1. Single pass over all users:
        //    - Collect each user's active subscriptions.
        //    - Build the deduplicated set of (origin, Option<dest>) call keys.
        //
        //    Subscriptions with no destination filter → (origin, None).
        //    Subscriptions with N destinations → N entries (origin, Some(dest)),
        //    one per destination, so each gets its own numRows budget.
        // -------------------------------------------------------------------
        let mut call_keys: HashSet<CallKey> = HashSet::new();
        let mut user_active_subs: Vec<(UserKey, Vec<crate::domain::Subscription>)> = Vec::new();

        for user_arc in &all_users {
            let user = user_arc.read().await;
            if user.is_paused_at(now) {
                debug!(user = %user.key.channel_user_id, "Skipping user: notifications paused");
                continue;
            }
            let active: Vec<_> = user
                .subscriptions
                .iter()
                .filter(|s| s.is_active_at(now))
                .cloned()
                .collect();
            if active.is_empty() {
                debug!(
                    user = %user.key.channel_user_id,
                    total_subs = user.subscriptions.len(),
                    "Skipping user: no active subscriptions right now"
                );
                continue;
            }
            debug!(
                user = %user.key.channel_user_id,
                active_subs = active.len(),
                "User has active subscriptions"
            );
            for sub in &active {
                if !cfg.filter_destination_at_api || sub.destination_filter.is_empty() {
                    // Default: fetch the full origin board and filter client-side.
                    // This produces one call per unique origin regardless of how many
                    // destination filters exist across all subscriptions.
                    call_keys.insert((sub.origin_crs.clone(), None));
                } else {
                    for dest in &sub.destination_filter {
                        call_keys.insert((sub.origin_crs.clone(), Some(dest.clone())));
                    }
                }
            }
            user_active_subs.push((user.key.clone(), active));
        }

        // -------------------------------------------------------------------
        // 1b. Remove expired (non-scheduled) subscriptions, persist, and
        //     notify each affected user before proceeding to poll.
        // -------------------------------------------------------------------
        for user_arc in &all_users {
            let expired: Vec<(String, String, Vec<String>)> = {
                let user = user_arc.read().await;
                user.subscriptions
                    .iter()
                    .filter(|s| s.is_expired_at(now))
                    .map(|s| {
                        (
                            s.id.clone(),
                            s.origin_crs.clone(),
                            s.destination_filter.clone(),
                        )
                    })
                    .collect()
            };
            if expired.is_empty() {
                continue;
            }

            let (user_key, snapshot) = {
                let mut user = user_arc.write().await;
                let expired_ids: HashSet<&str> =
                    expired.iter().map(|(id, _, _)| id.as_str()).collect();
                user.subscriptions
                    .retain(|s| !expired_ids.contains(s.id.as_str()));
                (user.key.clone(), user.clone())
            };

            info!(
                user = %user_key.channel_user_id,
                count = expired.len(),
                "Removing expired subscription(s)"
            );

            if let Err(e) = store.persist(&snapshot).await {
                error!(
                    user = %user_key.channel_user_id,
                    err = %e,
                    "Failed to persist user after subscription expiry"
                );
            }

            // Also clear their last-sent snapshot so a fresh board is sent
            // if they re-subscribe.
            user_snapshots.remove(&user_key);

            let lines: Vec<String> = expired
                .iter()
                .map(|(_, origin, dests)| {
                    if dests.is_empty() {
                        format!("• <code>{}</code> → all destinations", esc(origin))
                    } else {
                        format!(
                            "• <code>{}</code> → <code>{}</code>",
                            esc(origin),
                            esc(&dests.join(", "))
                        )
                    }
                })
                .collect();

            let plural = if expired.len() == 1 { "" } else { "s" };
            let verb = if expired.len() == 1 { "has" } else { "have" };
            let text = format!(
                "Subscription{plural} {verb} expired and been automatically removed:\n{}\n\nUse <code>/sub</code> to subscribe again.",
                lines.join("\n"),
            );

            match notifier.send(&user_key, &OutboundMessage::text(text)).await {
                Ok(()) => metrics.record_message_sent(&user_key).await,
                Err(e) => warn!(
                    user = %user_key.channel_user_id,
                    err = %e,
                    "Failed to notify user of subscription expiry"
                ),
            }
        }

        if call_keys.is_empty() {
            info!(
                "Poll cycle: no active subscriptions; sleeping {}s",
                cfg.interval_seconds
            );
            metrics.flush(&store).await;
            sleep(interval).await;
            continue;
        }

        info!(call_keys = call_keys.len(), "Poll cycle starting");
        metrics.record_darwin_requests(call_keys.len() as u64).await;

        // -------------------------------------------------------------------
        // 2. Fire all (origin, Option<dest>) calls concurrently; merge results
        //    into a flat service_id → ServiceSnapshot map.  Running in parallel
        //    means total wait time = slowest individual call, not the sum.
        //    Duplicate service IDs from overlapping calls (e.g. an unfiltered
        //    and a filtered call for the same origin) are deduplicated by the
        //    HashMap insert — both carry identical data.
        // -------------------------------------------------------------------
        let mut set: JoinSet<DarwinResult> = JoinSet::new();

        for (crs, filter_crs) in &call_keys {
            let darwin = Arc::clone(&darwin);
            let crs = crs.clone();
            let filter_crs = filter_crs.clone();
            let rows = cfg.poll_rows;
            set.spawn(async move {
                let result = darwin
                    .get_departure_board(&crs, rows, filter_crs.as_deref())
                    .await;
                (crs, filter_crs, result)
            });
        }

        let mut service_map: HashMap<String, diff::ServiceSnapshot> = HashMap::new();

        while let Some(join_result) = set.join_next().await {
            match join_result {
                Ok((crs, filter_crs, Ok(services))) => {
                    let count = services.len();
                    info!(crs, filter_crs = ?filter_crs, services = count, "Darwin returned services");
                    for svc in services {
                        service_map
                            .insert(svc.service_id.clone(), diff::ServiceSnapshot::from(&svc));
                    }
                }
                Ok((crs, filter_crs, Err(e))) => {
                    error!(crs, filter_crs = ?filter_crs, err = %e, "Darwin poll failed; skipping this call");
                }
                Err(e) => {
                    error!(err = %e, "Darwin poll task panicked");
                }
            }
        }

        if service_map.is_empty() {
            warn!("All Darwin calls failed or returned no services; sleeping");
            sleep(interval).await;
            continue;
        }

        info!(services = service_map.len(), "Service map built");

        // -------------------------------------------------------------------
        // 3. For each user, filter the service map to their interests, diff
        //    against their last snapshot, and send a notification if changed.
        // -------------------------------------------------------------------
        let now_str = now.with_timezone(&London).format("%H:%M").to_string();

        for (user_key, active_subs) in &user_active_subs {
            // Services this user cares about: origin matches AND destination
            // is in their filter (empty filter = all destinations).
            let raw_view: HashMap<String, diff::ServiceSnapshot> = service_map
                .iter()
                .filter(|(_, snap)| {
                    active_subs.iter().any(|sub| {
                        sub.origin_crs == snap.origin_crs
                            && (sub.destination_filter.is_empty()
                                || sub.destination_filter.contains(&snap.destination_crs)
                                || sub
                                    .destination_filter
                                    .iter()
                                    .any(|d| snap.calling_point_crs.contains(d)))
                    })
                })
                .map(|(id, snap)| (id.clone(), snap.clone()))
                .collect();

            // Truncate to the same per-origin window shown in the digest so that
            // services outside the display window don't trigger notifications.
            let user_view = truncate_to_display_window(raw_view, diff::DISPLAY_ROWS);

            info!(
                user = %user_key.channel_user_id,
                matched_services = user_view.len(),
                "User view built"
            );

            let last = user_snapshots.get(user_key);
            let is_first_run = last.is_none();
            let changes = diff::diff(last, &user_view);

            debug!(
                user = %user_key.channel_user_id,
                changes = changes.len(),
                first_run = is_first_run,
                "Diff computed"
            );

            if changes.is_empty() {
                debug!(user = %user_key.channel_user_id, "No changes; skipping notification");
                continue;
            }

            let changed_ids: HashSet<String> = changes
                .iter()
                .map(|c| c.snapshot.service_id.clone())
                .collect();

            let station_names: HashMap<String, String> = active_subs
                .iter()
                .map(|s| s.origin_crs.clone())
                .filter_map(|crs| {
                    let name = stations.by_crs(&crs)?.name.clone();
                    Some((crs, name))
                })
                .collect();

            let Some(text) = diff::format_digest(
                active_subs,
                &user_view,
                &changed_ids,
                &now_str,
                &station_names,
                &user_key.channel,
            ) else {
                warn!(user = %user_key.channel_user_id, "format_digest returned None despite non-empty changes");
                continue;
            };

            info!(
                user = %user_key.channel_user_id,
                changes = changes.len(),
                first_run = is_first_run,
                "Sending notification"
            );

            match notifier.send(user_key, &OutboundMessage::text(text)).await {
                Ok(()) => {
                    metrics.record_message_sent(user_key).await;
                    // Only advance the snapshot when the notification was delivered.
                    user_snapshots.insert(user_key.clone(), user_view);
                }
                Err(e) => warn!(
                    user = %user_key.channel_user_id,
                    err = %e,
                    "Failed to send notification"
                ),
            }
        }

        info!(
            "Poll cycle: complete; flushing metrics then sleeping {}s",
            cfg.interval_seconds
        );
        metrics.flush(&store).await;
        sleep(interval).await;
    }
}

/// Keep only the first `per_origin` services per origin CRS (sorted by STD),
/// matching the rows shown in `format_digest`. Services beyond this window are
/// excluded from the diff so they cannot trigger spurious notifications.
fn truncate_to_display_window(
    view: HashMap<String, diff::ServiceSnapshot>,
    per_origin: usize,
) -> HashMap<String, diff::ServiceSnapshot> {
    let mut by_origin: HashMap<String, Vec<(String, diff::ServiceSnapshot)>> = HashMap::new();
    for (id, snap) in view {
        by_origin
            .entry(snap.origin_crs.clone())
            .or_default()
            .push((id, snap));
    }
    let mut result = HashMap::new();
    for (_, mut services) in by_origin {
        services.sort_by(|a, b| a.1.std.cmp(&b.1.std));
        for (id, snap) in services.into_iter().take(per_origin) {
            result.insert(id, snap);
        }
    }
    result
}
