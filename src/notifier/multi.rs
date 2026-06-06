//! Multi-channel notifier that routes by [`crate::domain::Channel`].
//!
//! The poller holds a single `Arc<dyn Notifier>` — this implementation.
//! At runtime it looks at `user.channel` and delegates to whichever
//! per-channel notifier was registered for that channel.
//!
//! # Adding a new channel
//!
//! No changes are required here. In `main.rs`, call `register` with the new
//! channel variant and its `Box<dyn Notifier>` during Phase 2 startup.

use crate::domain::{Channel, UserKey};
use crate::notifier::{Notifier, NotifyError, OutboundMessage};
use async_trait::async_trait;
use std::collections::HashMap;

pub struct MultiChannelNotifier {
    notifiers: HashMap<Channel, Box<dyn Notifier>>,
}

impl MultiChannelNotifier {
    pub fn new() -> Self {
        Self {
            notifiers: HashMap::new(),
        }
    }

    /// Register a notifier for a channel. Replaces any existing registration.
    pub fn register(&mut self, channel: Channel, notifier: Box<dyn Notifier>) {
        self.notifiers.insert(channel, notifier);
    }
}

#[async_trait]
impl Notifier for MultiChannelNotifier {
    async fn send(&self, user: &UserKey, message: &OutboundMessage) -> Result<(), NotifyError> {
        match self.notifiers.get(&user.channel) {
            Some(n) => n.send(user, message).await,
            None => Err(NotifyError::UserUnreachable),
        }
    }
}
