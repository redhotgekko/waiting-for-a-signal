use crate::domain::Subscription;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Which messaging channel a user is reachable on.
///
/// # Adding a new channel
///
/// 1. Add a variant here (e.g. `WhatsApp`).
/// 2. Add an `as_str()` arm below (e.g. `"whatsapp"`).
/// 3. Add a `Format` arm in `handlers/commands.rs` (`Format::from(&Channel)`).
/// 4. Add a config section in `config.rs` and a notifier in `notifier/`.
/// 5. Implement `ChannelAdapter` in `channel/` and register in `main.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    Telegram,
    /// Twilio cloud SMS.
    Twilio,
}

impl Channel {
    /// A stable lowercase ASCII identifier for this channel.
    /// Used as the prefix in user JSON filenames (e.g. `"telegram-123"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Channel::Telegram => "telegram",
            Channel::Twilio => "twilio",
        }
    }
}

/// Channel-agnostic user identity.
///
/// Serialised as part of the per-user JSON file — add new variants to
/// `Channel` without bumping `User::version` unless the JSON shape changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserKey {
    pub channel: Channel,
    /// Channel-specific user ID:
    /// - Telegram: the numeric chat/user ID as a string ("123456789")
    /// - Twilio: E.164 phone number ("+441632960000")
    pub channel_user_id: String,
}

impl UserKey {
    /// Generic constructor — preferred when adding new channel adapters.
    pub fn new(channel: Channel, id: impl Into<String>) -> Self {
        Self {
            channel,
            channel_user_id: id.into(),
        }
    }

    pub fn telegram(id: i64) -> Self {
        Self::new(Channel::Telegram, id.to_string())
    }

    /// Create a key for a Twilio SMS user identified by their phone number.
    /// `phone` should be in E.164 format, e.g. `"+441632960000"`.
    pub fn twilio(phone: &str) -> Self {
        Self::new(Channel::Twilio, phone)
    }

    /// Returns the filename stem for this user's JSON file.
    ///
    /// The stem is `"<channel>-<sanitized_id>"` where the ID is sanitised by
    /// mapping `+` → `p` and replacing any non-alphanumeric character with `_`.
    ///
    /// Examples:
    /// - Telegram user 123 → `telegram-123`
    /// - Twilio "+441632960000" → `twilio-p441632960000`
    pub fn file_stem(&self) -> String {
        let sanitized: String = self
            .channel_user_id
            .chars()
            .map(|c| match c {
                '+' => 'p',
                c if c.is_ascii_alphanumeric() => c,
                _ => '_',
            })
            .collect();
        format!("{}-{}", self.channel.as_str(), sanitized)
    }
}

/// Full in-memory state for a single user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Schema version — increment when the JSON format changes incompatibly.
    pub version: u32,
    #[serde(flatten)]
    pub key: UserKey,
    pub created_at: DateTime<Utc>,
    pub notifications_paused: bool,
    /// If set, notifications are paused until this UTC time (timed pause).
    /// Takes precedence over `notifications_paused = false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_until: Option<DateTime<Utc>>,
    pub subscriptions: Vec<Subscription>,
}

impl User {
    pub fn new(key: UserKey) -> Self {
        Self {
            version: 2,
            key,
            created_at: Utc::now(),
            notifications_paused: false,
            paused_until: None,
            subscriptions: Vec::new(),
        }
    }

    /// Returns true if notifications should be suppressed at the given time.
    pub fn is_paused_at(&self, now: DateTime<Utc>) -> bool {
        self.notifications_paused || self.paused_until.is_some_and(|t| t > now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_stem_telegram() {
        let key = UserKey::telegram(123456789);
        assert_eq!(key.file_stem(), "telegram-123456789");
    }

    #[test]
    fn new_user_defaults() {
        let key = UserKey::telegram(1);
        let user = User::new(key);
        assert_eq!(user.version, 2);
        assert!(!user.notifications_paused);
        assert!(user.subscriptions.is_empty());
    }
}
