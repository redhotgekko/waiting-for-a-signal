//! Telegram slash-command handler and the [`Command`] enum.
//!
//! The channel-agnostic execution helpers (`exec_sub`, `exec_subs`, …)
//! live in [`crate::handlers::commands`]; this module only contains the
//! Telegram-specific wiring: parsing the incoming [`teloxide::types::Message`]
//! and dispatching to those helpers.

use crate::handlers::AppState;
use crate::handlers::commands::{Format, esc};
use anyhow::Result;
use teloxide::macros::BotCommands;
use teloxide::prelude::*;
use teloxide::types::Message;

pub fn welcome_message() -> String {
    "<b>WaitingForASignal</b>\n\n\
     Use <code>/sub WAT</code> to subscribe to a station, \
     <code>/find waterloo</code> to look up CRS codes, \
     or <code>/help</code> to see all commands.\n\n\
     Use <code>/pause</code> or <code>/pause 30</code> to pause notifications and <code>/resume</code> to restart them."
        .to_string()
}

/// All slash commands the Telegram bot understands.
#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase", description = "Available commands:")]
pub enum Command {
    // Active commands
    #[command(description = "Show all commands.")]
    Help,
    #[command(description = "Search for a CRS station code by name.")]
    Find(String),
    #[command(description = "Live departures. Args: CRS [to CRS]")]
    Now(String),
    #[command(description = "Create a subscription. Args: CRS [to CRS] [DAYS HH:MM-HH:MM]")]
    Sub(String),
    #[command(description = "Remove subscription(s). Args: ID or all")]
    Unsub(String),
    #[command(description = "List your subscriptions.")]
    Subs,
    #[command(description = "Pause notifications. Args: [MINUTES]")]
    Pause(String),
    #[command(description = "Resume notifications.")]
    Resume,
    // Secret kill switch — hidden from help
    #[command(hide)]
    Kill,
}

/// Telegram-formatted help text, generated from the [`Command`] enum's
/// derive macro descriptions (angle brackets HTML-escaped for Telegram).
pub fn exec_help() -> String {
    let raw = <Command as teloxide::utils::command::BotCommands>::descriptions().to_string();
    esc(&raw)
}

pub async fn handle_command(bot: Bot, msg: Message, cmd: Command, state: AppState) -> Result<()> {
    use crate::handlers::commands;

    let chat_id = msg.chat.id;
    let user_key = match msg.from.as_ref() {
        Some(u) => crate::domain::UserKey::telegram(u.id.0 as i64),
        None => return Ok(()),
    };

    let (user_arc, is_new) = state.store.get_or_create_new(user_key.clone()).await;

    if is_new {
        let user = user_arc.read().await;
        let _ = state.store.persist(&user).await;
    }

    let body = match cmd {
        Command::Help => exec_help(),
        Command::Find(query) => commands::exec_find(&query, &state),
        Command::Now(args) => handle_now(&args, &state).await,
        Command::Sub(args) => commands::exec_sub(&args, &state, &user_key, Format::Html).await,
        Command::Unsub(id) => commands::exec_unsub(&id, &state, &user_key).await,
        Command::Subs => commands::exec_subs(&state, &user_key).await,
        Command::Pause(args) => commands::exec_pause(&args, &state, &user_key).await,
        Command::Resume => commands::exec_resume(&state, &user_key).await,
        Command::Kill => commands::exec_kill(&state),
    };

    let reply = if is_new {
        format!("{}\n\n{body}", welcome_message())
    } else {
        body
    };

    bot.send_message(chat_id, reply)
        .parse_mode(teloxide::types::ParseMode::Html)
        .await
        .map_err(|e| anyhow::anyhow!("Telegram send error: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Argument parsers (Telegram slash-command wrappers around exec_*)
// ---------------------------------------------------------------------------

async fn handle_now(args: &str, state: &AppState) -> String {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let crs = tokens.first().map(|s| s.to_uppercase()).unwrap_or_default();
    if crs.is_empty() {
        return "Usage: <code>/now CRS [to CRS]</code>".to_string();
    }
    let dests: Vec<String> = if tokens.len() >= 3 && tokens[1].eq_ignore_ascii_case("to") {
        vec![tokens[2].to_uppercase()]
    } else {
        Vec::new()
    };
    crate::handlers::commands::exec_now(&crs, &dests, state, Format::Html).await
}
