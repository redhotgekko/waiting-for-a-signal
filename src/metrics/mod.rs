/// Lightweight append-only metrics store.
///
/// Each flush writes one row to `metrics.csv` (aggregate totals) and one row
/// per user to `metrics_users.csv`.  The header is written automatically the
/// first time the file is created.  No JSON, no atomic rename — just open in
/// append mode and write.
///
/// Cumulative counters (`darwin_requests_total`, `messages_sent_total`) reset
/// to zero on process restart; the CSV provides the long-term history.
use crate::domain::UserKey;
use crate::storage::UserStore;
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{debug, warn};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct Counters {
    pub darwin_requests_total: u64,
    pub messages_sent_total: u64,
    pub per_user: HashMap<String, UserCounters>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct UserCounters {
    pub messages_sent: u64,
}

pub struct MetricsStore {
    metrics_path: PathBuf,
    users_path: PathBuf,
    data: Mutex<Counters>,
}

impl MetricsStore {
    pub fn new(metrics_path: PathBuf, users_path: PathBuf) -> Self {
        Self {
            metrics_path,
            users_path,
            data: Mutex::new(Counters::default()),
        }
    }

    /// Add `n` to the Darwin requests counter.
    pub async fn record_darwin_requests(&self, n: u64) {
        if n > 0 {
            self.data.lock().await.darwin_requests_total += n;
        }
    }

    /// Increment the messages-sent counter for one successful notification.
    pub async fn record_message_sent(&self, user_key: &UserKey) {
        let mut data = self.data.lock().await;
        data.messages_sent_total += 1;
        data.per_user
            .entry(user_key.file_stem())
            .or_default()
            .messages_sent += 1;
    }

    /// Append one CSV row to each metrics file.
    ///
    /// The mutex is held only for the in-memory snapshot; file I/O happens
    /// outside the lock so slow storage cannot block counter updates.
    pub async fn flush(&self, user_store: &UserStore) {
        let ts = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

        // Gather current subscription counts from the user store (no lock held yet).
        let all_users = user_store.all_users().await;
        let mut user_subs: Vec<(String, u32)> = Vec::with_capacity(all_users.len());
        let mut subscriptions_total: u32 = 0;
        for user_arc in &all_users {
            let user = user_arc.read().await;
            let n = user.subscriptions.len() as u32;
            subscriptions_total += n;
            user_subs.push((user.key.file_stem(), n));
        }

        // Snapshot counters (mutex held briefly, released before any I/O).
        let (darwin_total, messages_total, per_user) = {
            let data = self.data.lock().await;
            (
                data.darwin_requests_total,
                data.messages_sent_total,
                data.per_user.clone(),
            )
        };

        debug!(
            darwin_total,
            messages_total, subscriptions_total, "Metrics flush"
        );

        // Aggregate row: metrics.csv
        let agg_row = format!(
            "{},{},{},{}\n",
            ts, darwin_total, messages_total, subscriptions_total
        );
        if let Err(e) = append_row(
            &self.metrics_path,
            "timestamp,darwin_requests_total,messages_sent_total,subscriptions_total",
            &agg_row,
        )
        .await
        {
            warn!(path = %self.metrics_path.display(), err = %e, "Failed to append metrics row");
        }

        // Per-user rows: metrics_users.csv
        if !user_subs.is_empty() {
            let mut rows = String::new();
            for (user_id, subs) in &user_subs {
                let msgs = per_user.get(user_id).map(|u| u.messages_sent).unwrap_or(0);
                rows.push_str(&format!("{},{},{},{}\n", ts, user_id, msgs, subs));
            }
            if let Err(e) = append_row(
                &self.users_path,
                "timestamp,user_id,messages_sent,subscriptions",
                &rows,
            )
            .await
            {
                warn!(path = %self.users_path.display(), err = %e, "Failed to append user metrics rows");
            }
        }
    }
}

/// Append `data` to `path`.  If the file does not yet exist, write `header`
/// as the first line before the data.
async fn append_row(path: &PathBuf, header: &str, data: &str) -> Result<()> {
    let needs_header = !path.exists();

    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(true)
        .open(path)
        .await
        .with_context(|| format!("Cannot open {}", path.display()))?;

    if needs_header {
        file.write_all(format!("{}\n", header).as_bytes())
            .await
            .context("Write CSV header")?;
    }

    file.write_all(data.as_bytes())
        .await
        .with_context(|| format!("Write CSV row to {}", path.display()))?;

    file.flush()
        .await
        .with_context(|| format!("Flush {}", path.display()))?;

    Ok(())
}
