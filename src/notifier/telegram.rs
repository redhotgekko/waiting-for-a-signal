use crate::domain::UserKey;
use crate::notifier::{Notifier, NotifyError, OutboundMessage};
use anyhow::Result;
use async_trait::async_trait;
use teloxide::prelude::*;
use teloxide::types::ParseMode;
use tracing::debug;

pub struct TelegramNotifier {
    bot: Bot,
}

impl TelegramNotifier {
    pub async fn new(token: &str) -> Result<Self> {
        let bot = Bot::new(token);
        Ok(Self { bot })
    }

    pub fn bot(&self) -> &Bot {
        &self.bot
    }
}

#[async_trait]
impl Notifier for TelegramNotifier {
    async fn send(&self, user: &UserKey, message: &OutboundMessage) -> Result<(), NotifyError> {
        let chat_id: i64 = user
            .channel_user_id
            .parse()
            .map_err(|_| NotifyError::UserUnreachable)?;

        self.bot
            .send_message(ChatId(chat_id), &message.text)
            .parse_mode(ParseMode::Html)
            .await
            .map_err(|e| NotifyError::Send(e.to_string()))?;

        debug!(chat_id, "Sent Telegram message");
        Ok(())
    }
}
