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
use teloxide::prelude::*;
use tracing::info;

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
