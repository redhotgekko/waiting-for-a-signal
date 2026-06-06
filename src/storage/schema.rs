use crate::domain::User;
use anyhow::{Result, bail};
use serde_json::Value;

/// Current schema version written to every new user file.
pub const CURRENT_VERSION: u32 = 2;

/// Deserialise raw JSON bytes into a `User`, applying any necessary migrations.
pub fn deserialise_and_migrate(bytes: &[u8]) -> Result<User> {
    let value: Value = serde_json::from_slice(bytes)?;

    let version = value.get("version").and_then(Value::as_u64).unwrap_or(0) as u32;

    let migrated = migrate(value, version)?;
    let user: User = serde_json::from_value(migrated)?;
    Ok(user)
}

/// Apply sequential migrations from `from_version` up to `CURRENT_VERSION`.
fn migrate(mut value: Value, from_version: u32) -> Result<Value> {
    if from_version > CURRENT_VERSION {
        bail!(
            "File has schema version {from_version} but this binary only knows up to \
             {CURRENT_VERSION}. Please upgrade the bot."
        );
    }

    if from_version < 2 {
        // v1 → v2: backfill display_id on every existing subscription.
        // Assign "01", "02", … in the order they appear in the array.
        if let Some(subs) = value.get_mut("subscriptions").and_then(Value::as_array_mut) {
            for (i, sub) in subs.iter_mut().enumerate() {
                if let Some(obj) = sub.as_object_mut() {
                    let display_id = format!("{:02}", i + 1);
                    obj.insert("display_id".to_string(), Value::String(display_id));
                }
            }
        }
        value["version"] = Value::Number(2.into());
    }

    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_roundtrips() {
        use crate::domain::UserKey;
        let user = User::new(UserKey::telegram(1));
        let json = serde_json::to_vec(&user).expect("serialise");
        let loaded = deserialise_and_migrate(&json).expect("deserialise");
        assert_eq!(loaded.key, user.key);
        assert_eq!(loaded.version, CURRENT_VERSION);
    }

    #[test]
    fn unknown_version_errors() {
        let json = serde_json::json!({
            "version": 999,
            "channel": "telegram",
            "channel_user_id": "1",
            "created_at": "2024-01-01T00:00:00Z",
            "notifications_paused": false,
            "subscriptions": [],
        });
        let bytes = serde_json::to_vec(&json).expect("serialise");
        assert!(deserialise_and_migrate(&bytes).is_err());
    }

    #[test]
    fn v1_to_v2_backfills_display_ids() {
        let json = serde_json::json!({
            "version": 1,
            "channel": "telegram",
            "channel_user_id": "1",
            "created_at": "2024-01-01T00:00:00Z",
            "notifications_paused": false,
            "subscriptions": [
                {
                    "id": "01HZ0000000000000000000000",
                    "origin_crs": "WAT",
                    "destination_filter": [],
                    "schedule": null,
                    "created_at": "2024-01-01T00:00:00Z"
                },
                {
                    "id": "01HZ0000000000000000000001",
                    "origin_crs": "SUR",
                    "destination_filter": ["WAT"],
                    "schedule": null,
                    "created_at": "2024-01-01T00:00:01Z"
                }
            ],
        });
        let bytes = serde_json::to_vec(&json).expect("serialise");
        let user = deserialise_and_migrate(&bytes).expect("migrate");
        assert_eq!(user.subscriptions[0].display_id, "01");
        assert_eq!(user.subscriptions[1].display_id, "02");
        assert_eq!(user.version, 2);
    }
}
