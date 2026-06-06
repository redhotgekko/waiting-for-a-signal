//! Plain-text message handler for the Telegram channel.
//!
//! Passes non-command messages to [`crate::handlers::handle_input`], which
//! returns a "use /help" prompt for any unrecognised input.

use crate::handlers::{AppState, handle_input};
use anyhow::Result;
use teloxide::prelude::*;
use teloxide::types::{Message, ParseMode};

pub async fn handle_message(bot: Bot, msg: Message, state: AppState) -> Result<()> {
    let text = match msg.text() {
        Some(t) => t,
        None => return Ok(()),
    };
    let user_key = match msg.from.as_ref() {
        Some(u) => crate::domain::UserKey::telegram(u.id.0 as i64),
        None => return Ok(()),
    };
    let chat_id = msg.chat.id;

    let (user_arc, is_new) = state.store.get_or_create_new(user_key.clone()).await;

    if is_new {
        let user = user_arc.read().await;
        let _ = state.store.persist(&user).await;
    }

    let reply = if is_new {
        super::commands::welcome_message()
    } else {
        handle_input(text, &user_key, &state).await.text
    };

    bot.send_message(chat_id, &reply)
        .parse_mode(ParseMode::Html)
        .await
        .map_err(|e| anyhow::anyhow!(e))?;

    Ok(())
}
