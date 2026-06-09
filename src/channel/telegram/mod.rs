//! Telegram channel adapter.
//!
//! Runs the Teloxide long-poll dispatcher. All inbound Telegram updates
//! (slash commands, plain messages) are handled here and routed to the
//! channel-agnostic [`crate::handlers`] core.

pub mod commands;
mod messages;

use crate::channel::ChannelAdapter;
use crate::handlers::AppState;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use teloxide::prelude::*;
use tokio::sync::RwLock;
use tracing::info;

/// When `capture_user_info` is enabled, update the stored display name and
/// @username for a Telegram user, persisting only if something changed.
pub(super) async fn maybe_capture_user_info(
    tg_user: &teloxide::types::User,
    user_arc: &Arc<RwLock<crate::domain::User>>,
    store: &crate::storage::UserStore,
) {
    let name = match &tg_user.last_name {
        Some(last) => format!("{} {}", tg_user.first_name, last),
        None => tg_user.first_name.clone(),
    };
    let username = tg_user.username.clone();

    let mut user = user_arc.write().await;
    let changed =
        user.telegram_name.as_deref() != Some(name.as_str()) || user.telegram_username != username;
    if changed {
        user.telegram_name = Some(name);
        user.telegram_username = username;
        let _ = store.persist(&user).await;
    }
}

/// Telegram-specific adapter.
///
/// `main` creates the [`teloxide::Bot`] (via the [`crate::notifier::telegram::TelegramNotifier`]
/// which is also registered in the [`crate::notifier::multi::MultiChannelNotifier`]),
/// then passes it here alongside the shared [`AppState`].
pub struct TelegramAdapter {
    state: AppState,
    bot: Bot,
}

impl TelegramAdapter {
    pub fn new(bot: Bot, state: AppState) -> Self {
        Self { state, bot }
    }
}

#[async_trait]
impl ChannelAdapter for TelegramAdapter {
    async fn run(&self) -> Result<()> {
        info!("Starting Telegram dispatcher");

        Dispatcher::builder(
            self.bot.clone(),
            dptree::entry()
                .branch(
                    Update::filter_message()
                        .filter_command::<commands::Command>()
                        .endpoint(commands::handle_command),
                )
                .branch(Update::filter_message().endpoint(messages::handle_message)),
        )
        .dependencies(dptree::deps![self.state.clone()])
        .build()
        .dispatch()
        .await;

        Ok(())
    }
}
