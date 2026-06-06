pub mod multi;
pub mod telegram;

use crate::domain::UserKey;
use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum NotifyError {
    #[error("Failed to send message: {0}")]
    Send(String),
    #[error("User not reachable on channel")]
    UserUnreachable,
}

/// A structured outbound message.
#[derive(Debug, Clone)]
pub struct OutboundMessage {
    pub text: String,
}

impl OutboundMessage {
    pub fn text(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// Channel-agnostic notification interface.
///
/// Implement this trait to add a new outbound messaging channel.
/// The [`multi::MultiChannelNotifier`] wraps multiple implementations and
/// routes each message to the correct one based on [`UserKey::channel`].
#[async_trait]
pub trait Notifier: Send + Sync {
    async fn send(&self, user: &UserKey, message: &OutboundMessage) -> Result<(), NotifyError>;
}
