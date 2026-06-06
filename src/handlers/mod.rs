//! Channel-agnostic command core.
//!
//! This module owns the shared [`AppState`] and the main entry-point used by
//! every channel adapter:
//!
//! - [`handle_input`] — responds to free-text messages with a "use /help" prompt.
//!
//! Actual command execution helpers (`exec_sub`, `exec_subs`, …) live in
//! [`commands`]. Channel-specific wiring lives in `src/channel/`.

pub mod commands;

use crate::domain::UserKey;
use crate::notifier::Notifier;
use crate::stations::StationIndex;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Shared state injected into every channel handler.
///
/// All fields are `Arc`-wrapped so [`AppState`] is cheap to `Clone` — Teloxide
/// requires the state type to be `Clone + Send + Sync + 'static`.
#[derive(Clone)]
pub struct AppState {
    pub store: Arc<crate::storage::UserStore>,
    pub stations: Arc<StationIndex>,
    pub darwin: Arc<dyn crate::darwin::DepartureSource>,
    pub notifier: Arc<dyn Notifier>,
    pub metrics: Arc<crate::metrics::MetricsStore>,
    pub polling_rows: u32,
    /// Sending on this channel triggers a graceful shutdown.
    pub shutdown: tokio::sync::mpsc::UnboundedSender<()>,
    pub kill_switch_enabled: bool,
}

// ---------------------------------------------------------------------------
// Dispatch outcome
// ---------------------------------------------------------------------------

/// The response produced by [`handle_input`].
pub struct DispatchOutcome {
    pub text: String,
}

// ---------------------------------------------------------------------------
// Unified entry-point (used by all channel adapters)
// ---------------------------------------------------------------------------

/// Unified free-text input handler for any channel.
///
/// Parses slash commands and dispatches to the shared execution helpers in
/// [`commands`].  The output format (HTML vs plain text) is derived from the
/// user's channel so each channel receives natively-formatted responses.
/// Plain-text messages receive a help prompt.
pub async fn handle_input(text: &str, user_key: &UserKey, state: &AppState) -> DispatchOutcome {
    let text = text.trim();
    let format = commands::Format::from(&user_key.channel);

    let (_user_arc, is_new) = state.store.get_or_create_new(user_key.clone()).await;
    if is_new {
        let user_arc = state.store.get_or_create(user_key.clone()).await;
        let user = user_arc.read().await;
        let _ = state.store.persist(&user).await;
    }

    let body = if let Some(rest) = text.strip_prefix('/') {
        let (cmd, args) = rest
            .split_once(char::is_whitespace)
            .map(|(c, a)| (c, a.trim()))
            .unwrap_or((rest, ""));

        dispatch_command(cmd, args, user_key, state, format).await
    } else {
        "I didn't recognise that — try /help to see available commands.".to_string()
    };

    DispatchOutcome { text: body }
}

async fn dispatch_command(
    cmd: &str,
    args: &str,
    user_key: &UserKey,
    state: &AppState,
    format: commands::Format,
) -> String {
    match cmd.to_lowercase().as_str() {
        "help" => commands::exec_help(),
        "find" => commands::exec_find(args, state),
        "now" => handle_now(args, state, format).await,
        "sub" => commands::exec_sub(args, state, user_key, format).await,
        "unsub" => commands::exec_unsub(args, state, user_key).await,
        "subs" => commands::exec_subs(state, user_key).await,
        "pause" => commands::exec_pause(args, state, user_key).await,
        "resume" => commands::exec_resume(state, user_key).await,
        "kill" => commands::exec_kill(state),
        _ => format!("Unknown command /{cmd} — try /help to see available commands."),
    }
}

async fn handle_now(args: &str, state: &AppState, format: commands::Format) -> String {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let crs = tokens.first().map(|s| s.to_uppercase()).unwrap_or_default();
    if crs.is_empty() {
        return "Usage: /now CRS [to CRS]".to_string();
    }
    let dests: Vec<String> = if tokens.len() >= 3 && tokens[1].eq_ignore_ascii_case("to") {
        vec![tokens[2].to_uppercase()]
    } else {
        Vec::new()
    };
    commands::exec_now(&crs, &dests, state, format).await
}
