pub mod telegram;
pub mod twilio;

use anyhow::Result;
use async_trait::async_trait;

/// A pluggable messaging-channel adapter.
///
/// Each channel (Telegram, Twilio, …) implements this trait. When a channel is
/// present in the config its adapter is spawned as an independent `tokio`
/// task in `main`. All adapters share the same [`crate::handlers::AppState`]
/// for command execution and the same [`crate::notifier::Notifier`]
/// (a [`crate::notifier::multi::MultiChannelNotifier`]) for proactive
/// push notifications from the poller.
///
/// To add a new channel:
/// 1. Implement `ChannelAdapter` + a matching `Notifier`.
/// 2. Add a config section (`Option<XyzConfig>`).
/// 3. Register in `main.rs`.
#[async_trait]
pub trait ChannelAdapter: Send + Sync {
    /// Start the adapter's inbound event loop. Runs until the process shuts
    /// down or a fatal, unrecoverable error occurs.
    async fn run(&self) -> Result<()>;
}
