//! Twilio cloud SMS channel adapter.
//!
//! **Inbound**: polls the Twilio Messages API every `poll_interval_secs`
//!   seconds for messages directed to the configured `from_number`.
//!   Only messages received after the adapter started are processed.
//!
//! **Outbound**: POSTs to the Twilio Messages API.  Twilio handles
//!   multi-segment splitting automatically, so the full reply text is
//!   sent in one API call.
//!
//! ```toml
//! [twilio]
//! account_sid        = "ACxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
//! auth_token         = "your_auth_token"
//! from_number        = "+14155551234"
//! poll_interval_secs = 10                 # optional, default 10
//! ```

use crate::channel::ChannelAdapter;
use crate::config::TwilioConfig;
use crate::domain::UserKey;
use crate::handlers::{AppState, handle_input};
use crate::notifier::{Notifier, NotifyError, OutboundMessage};
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use tracing::{error, info, warn};

const API_BASE: &str = "https://api.twilio.com/2010-04-01/Accounts";

// ---------------------------------------------------------------------------
// Twilio API response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct MessageList {
    messages: Vec<Message>,
}

#[derive(Deserialize)]
struct Message {
    sid: String,
    from: String,
    body: String,
    direction: String,
    date_created: String,
}

// ---------------------------------------------------------------------------
// TwilioNotifier — outbound SMS via Twilio REST API
// ---------------------------------------------------------------------------

pub struct TwilioNotifier {
    client: Client,
    account_sid: String,
    auth_token: String,
    from_number: String,
}

impl TwilioNotifier {
    pub fn new(cfg: &TwilioConfig) -> Result<Self> {
        let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
        Ok(Self {
            client,
            account_sid: cfg.account_sid.clone(),
            auth_token: cfg.auth_token.clone(),
            from_number: cfg.from_number.clone(),
        })
    }
}

#[async_trait]
impl Notifier for TwilioNotifier {
    async fn send(&self, user: &UserKey, message: &OutboundMessage) -> Result<(), NotifyError> {
        let to = user.channel_user_id.clone();
        let body = gsm_sanitise(&strip_html(&message.text));
        let url = format!("{}/{}/Messages.json", API_BASE, self.account_sid);

        let resp = self
            .client
            .post(&url)
            .basic_auth(&self.account_sid, Some(&self.auth_token))
            .form(&[
                ("To", to.as_str()),
                ("From", self.from_number.as_str()),
                ("Body", body.as_str()),
            ])
            .send()
            .await
            .map_err(|e| NotifyError::Send(format!("Twilio request failed: {e}")))?;

        if resp.status().is_success() {
            info!(%to, "Twilio SMS sent");
            Ok(())
        } else {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            Err(NotifyError::Send(format!("Twilio API {status}: {err}")))
        }
    }
}

// ---------------------------------------------------------------------------
// TwilioAdapter — inbound polling loop
// ---------------------------------------------------------------------------

pub struct TwilioAdapter {
    client: Client,
    account_sid: String,
    auth_token: String,
    our_number: String,
    poll_interval: Duration,
    state: AppState,
}

impl TwilioAdapter {
    pub fn new(cfg: &TwilioConfig, state: AppState) -> Result<Self> {
        let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
        Ok(Self {
            client,
            account_sid: cfg.account_sid.clone(),
            auth_token: cfg.auth_token.clone(),
            our_number: cfg.from_number.clone(),
            poll_interval: Duration::from_secs(cfg.poll_interval_secs),
            state,
        })
    }
}

#[async_trait]
impl ChannelAdapter for TwilioAdapter {
    async fn run(&self) -> Result<()> {
        info!(
            interval_secs = self.poll_interval.as_secs(),
            our_number = %self.our_number,
            "Twilio adapter started — polling for inbound messages"
        );

        let url = format!("{}/{}/Messages.json", API_BASE, self.account_sid);

        // Only process messages that arrive after this adapter starts.
        let mut cutoff: DateTime<Utc> = Utc::now();

        let mut ticker = tokio::time::interval(self.poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;

            let resp = match self
                .client
                .get(&url)
                .basic_auth(&self.account_sid, Some(&self.auth_token))
                .query(&[("To", self.our_number.as_str()), ("PageSize", "50")])
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    error!("Twilio poll failed: {e}");
                    continue;
                }
            };

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                error!("Twilio API {status}: {body}");
                continue;
            }

            let list: MessageList = match resp.json().await {
                Ok(l) => l,
                Err(e) => {
                    error!("Failed to parse Twilio response: {e}");
                    continue;
                }
            };

            // Twilio returns messages newest-first; collect only those after
            // the cutoff, then sort oldest-first so replies are sent in order.
            let mut inbound: Vec<(DateTime<Utc>, String, String)> = list
                .messages
                .into_iter()
                .filter(|m| m.direction == "inbound")
                .filter_map(|m| {
                    let date = match DateTime::parse_from_rfc2822(&m.date_created) {
                        Ok(d) => d.with_timezone(&Utc),
                        Err(e) => {
                            warn!(sid = %m.sid, "Could not parse date_created '{d}': {e}", d = m.date_created);
                            return None;
                        }
                    };
                    if date > cutoff { Some((date, m.from, m.body)) } else { None }
                })
                .collect();

            inbound.sort_by_key(|(date, _, _)| *date);

            let mut latest = cutoff;
            for (date, from, body) in inbound {
                info!(%from, %body, "Inbound Twilio SMS");
                let user_key = UserKey::twilio(&from);
                let reply = route_sms(body.trim(), &user_key, &self.state).await;

                if let Err(e) = self
                    .state
                    .notifier
                    .send(&user_key, &OutboundMessage::text(reply))
                    .await
                {
                    error!(%from, "Failed to send Twilio reply: {e}");
                }

                if date > latest {
                    latest = date;
                }
            }

            cutoff = latest;
        }
    }
}

async fn route_sms(body: &str, user_key: &UserKey, state: &AppState) -> String {
    let outcome = handle_input(body, user_key, state).await;
    strip_html(&outcome.text)
}

/// Strip HTML tags and decode the three entities Telegram's HTML subset uses.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// Map common non-ASCII characters to ASCII equivalents for GSM IRA text mode,
/// then replace any remaining non-ASCII with `?`.
fn gsm_sanitise(s: &str) -> String {
    let s = s
        .replace('\u{2192}', "->") // →
        .replace('\u{21D2}', "=>") // ⇒
        .replace('\u{2190}', "<-") // ←
        .replace('\u{2026}', "..."); // …
    s.chars()
        .filter(|&c| c != '\x1A') // strip Ctrl-Z (send sentinel)
        .map(|c| match c {
            '\u{2022}' | '\u{2023}' | '\u{25CF}' => '-', // bullet variants •
            '\u{2014}' | '\u{2013}' | '\u{2012}' => '-', // em/en/figure dash
            '\u{2018}' | '\u{2019}' => '\'',             // curly apostrophes
            '\u{201C}' | '\u{201D}' => '"',              // curly quotes
            c if c.is_ascii() => c,
            _ => '?',
        })
        .collect()
}
